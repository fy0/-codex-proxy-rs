//! 调度时间与累计尝试数持久化在桶内，重启不丢失当前 hunt 的进度。
//!
//! 并发按账号分组：同一账号的多个模型桶在一个会话内串行打票，
//! 共享预算、死线与种子 pair；不同账号之间仍并发到 4。

use chrono::Utc;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::{
    account::{ProviderAccountId, TurnStateBucket},
    task::{ScheduledTask, WorkerCycleContext, WorkerTaskError},
};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use super::{TurnStateService, probe, service::ProbeOutcome};

pub(crate) struct TurnStateTask {
    service: Arc<TurnStateService>,
}

impl TurnStateTask {
    pub(crate) fn new(service: Arc<TurnStateService>) -> Self {
        Self { service }
    }

    /// 按桶的旧持久化语义排定下一次探测：Skipped 或每 budget 次尝试转空闲间隔，
    /// 其余按重试间隔+抖动；Cookie 续约窗口对延迟封顶。
    async fn schedule_next(
        service: &TurnStateService,
        account_id: &ProviderAccountId,
        bucket: &TurnStateBucket,
        outcome: ProbeOutcome,
    ) {
        if matches!(outcome, ProbeOutcome::Installed) {
            return;
        }
        let mut delay = if matches!(outcome, ProbeOutcome::Skipped)
            || (bucket.hunt_attempts + 1).is_multiple_of(u64::from(bucket.config.budget.max(1)))
        {
            bucket.config.idle_seconds
        } else {
            bucket.config.retry_seconds
                + probe::random_index(bucket.config.jitter_seconds as usize + 1) as u64
        };
        // 以本次探测后的池计算上限，长空闲间隔不能错过续约窗口。
        if bucket.config.cookie_lock_enabled
            && let Some(current) = service.current(account_id, &bucket.model).await
            && let Some(cookie) = current.routing_cookie(Utc::now().timestamp())
        {
            let remaining = cookie.expires_at - Utc::now().timestamp();
            let before_refresh = remaining - bucket.config.cookie_refresh_before_seconds as i64;
            let cap = if before_refresh > 0 {
                before_refresh
            } else {
                remaining.max(2) / 2
            };
            delay = delay.min(cap.max(1) as u64);
        }
        if service
            .store
            .schedule_turn_state(
                account_id,
                &bucket.model,
                Utc::now().timestamp() + delay as i64,
            )
            .await
            .is_err()
        {
            tracing::warn!(
                account_id = bucket.account_id,
                model = bucket.model,
                "turn state schedule persistence failed"
            );
        }
    }
}

impl ScheduledTask for TurnStateTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let buckets = self
                .service
                .store
                .turn_state_buckets()
                .await
                .map_err(|_| WorkerTaskError::safe("turn state buckets unavailable"))?;
            let cancellation = context.cancellation();
            let service = Arc::clone(&self.service);
            let probe_cancellation = cancellation.clone();
            // 先按账号归组：组内串行打票，组间并发，杜绝同账号并发裸打。
            let mut groups: BTreeMap<String, Vec<TurnStateBucket>> = BTreeMap::new();
            for bucket in buckets.into_iter().filter(|bucket| {
                bucket.config.enabled
                    || bucket.config.cookie_lock_enabled
                    || bucket.manual_probe_requested_at.is_some()
            }) {
                groups
                    .entry(bucket.account_id.clone())
                    .or_default()
                    .push(bucket);
            }
            let probes: BoxFuture<'static, ()> = Box::pin(
                futures::stream::iter(groups).for_each_concurrent(
                    4,
                    move |(account_key, mut buckets)| {
                        let service = Arc::clone(&service);
                        let cancellation = probe_cancellation.clone();
                        Box::pin(async move {
                            if cancellation.is_cancelled() {
                                return;
                            }
                            let Ok(account_id) = ProviderAccountId::new(account_key) else {
                                return;
                            };
                            for bucket in &mut buckets {
                                bucket
                                    .routing_cookies
                                    .retain(|cookie| cookie.origin == service.endpoint());
                            }
                            let session = service.new_session(&account_id, &buckets);
                            let mut pending: Vec<&TurnStateBucket> = Vec::new();
                            for bucket in &buckets {
                                if cancellation.is_cancelled() || session.exhausted() {
                                    return;
                                }
                                if bucket.manual_probe_requested_at.is_some() {
                                    match service
                                        .store
                                        .claim_turn_state_probe(&account_id, &bucket.model)
                                        .await
                                    {
                                        Ok(true) => {
                                            tokio::select! {
                                                () = cancellation.cancelled() => {},
                                                _ = service.probe(bucket, &account_id, true, &session) => {},
                                            }
                                        }
                                        Ok(false) => {}
                                        Err(_) => tracing::warn!(
                                            account_id = bucket.account_id,
                                            model = bucket.model,
                                            "manual turn state probe claim failed"
                                        ),
                                    }
                                    continue;
                                }
                                let now = Utc::now().timestamp();
                                // 票的真实到期才刷新，不再按 refresh_after 提前轮换；
                                // 声明模型/云端打票必须有已装票，旧 Cookie 锁定仍按 pair 驱动。
                                if bucket.config.requires_route_pair()
                                    || !bucket.config.cookie_lock_enabled
                                {
                                    if bucket.installed_token(now).is_some() {
                                        continue;
                                    }
                                } else if bucket.routing_cookie(now).is_some()
                                    && !bucket.cookie_renewal_due(now)
                                {
                                    continue;
                                }
                                // 被动候选可以随时结束 hunt，不必等到下一次主动探测。
                                if (!bucket.config.cookie_lock_enabled
                                    || bucket.config.requires_route_pair())
                                    && service.install(&account_id, &bucket.model).await
                                {
                                    continue;
                                }
                                if bucket
                                    .next_probe_at
                                    .is_some_and(|next| next > Utc::now().timestamp())
                                {
                                    continue;
                                }
                                pending.push(bucket);
                            }
                            // 重试轮次：一轮内每个仍缺票的模型各试一次，而不是把单个模型
                            // 打到枯竭才轮到下一个；429/5xx 进下一轮，400/404/422 永久
                            // 出局本模型，401/403 中止整个账号任务；预算与死线跨轮共享。
                            while !pending.is_empty() {
                                if cancellation.is_cancelled() || session.aborted() {
                                    return;
                                }
                                if session.exhausted() {
                                    for bucket in pending.drain(..) {
                                        Self::schedule_next(
                                            &service,
                                            &account_id,
                                            bucket,
                                            ProbeOutcome::Skipped,
                                        )
                                        .await;
                                    }
                                    break;
                                }
                                let mut retry: Vec<&TurnStateBucket> = Vec::new();
                                for bucket in std::mem::take(&mut pending) {
                                    if cancellation.is_cancelled() || session.aborted() {
                                        return;
                                    }
                                    if session.exhausted() {
                                        Self::schedule_next(
                                            &service,
                                            &account_id,
                                            bucket,
                                            ProbeOutcome::Skipped,
                                        )
                                        .await;
                                        continue;
                                    }
                                    let outcome = tokio::select! {
                                        () = cancellation.cancelled() => return,
                                        outcome = service.probe(bucket, &account_id, false, &session) => outcome,
                                    };
                                    match outcome {
                                        ProbeOutcome::Miss(Some(401 | 403)) => return,
                                        ProbeOutcome::Miss(Some(429))
                                        | ProbeOutcome::Miss(Some(500..=599)) => {
                                            retry.push(bucket)
                                        }
                                        _ => {
                                            Self::schedule_next(
                                                &service, &account_id, bucket, outcome,
                                            )
                                            .await;
                                        }
                                    }
                                }
                                pending = retry;
                                if pending.is_empty() {
                                    break;
                                }
                                // 轮间冷却一次，截断到会话死线；死线耗尽时按重试间隔
                                // 排好下次调度，不丢桶。
                                let wait = Duration::from_secs(
                                    pending
                                        .iter()
                                        .map(|bucket| bucket.config.retry_seconds)
                                        .min()
                                        .unwrap_or(30)
                                        + probe::random_index(
                                            pending
                                                .iter()
                                                .map(|bucket| {
                                                    bucket.config.jitter_seconds as usize + 1
                                                })
                                                .max()
                                                .unwrap_or(1),
                                        ) as u64,
                                );
                                let remaining = session
                                    .deadline
                                    .saturating_duration_since(Instant::now());
                                let wait = wait.min(remaining);
                                tokio::select! {
                                    () = cancellation.cancelled() => return,
                                    () = tokio::time::sleep(wait) => {},
                                }
                                if Instant::now() >= session.deadline || !session.budget_left() {
                                    for bucket in pending.drain(..) {
                                        Self::schedule_next(
                                            &service,
                                            &account_id,
                                            bucket,
                                            ProbeOutcome::Miss(Some(429)),
                                        )
                                        .await;
                                    }
                                    break;
                                }
                            }
                        }) as BoxFuture<'static, ()>
                    },
                ),
            );
            let notifications: BoxFuture<'_, ()> =
                Box::pin(self.service.notify_installations(cancellation));
            tokio::join!(probes, notifications);
            Ok(())
        })
    }
}
