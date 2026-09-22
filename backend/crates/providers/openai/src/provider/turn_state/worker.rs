//! 调度时间与累计尝试数持久化在桶内，重启不丢失当前 hunt 的进度。

use chrono::Utc;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::{
    account::ProviderAccountId,
    task::{ScheduledTask, WorkerCycleContext, WorkerTaskError},
};
use std::sync::Arc;

use super::{TurnStateService, probe, service::ProbeOutcome};

pub(crate) struct TurnStateTask {
    service: Arc<TurnStateService>,
}

impl TurnStateTask {
    pub(crate) fn new(service: Arc<TurnStateService>) -> Self {
        Self { service }
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
            let probes: BoxFuture<'static, ()> = Box::pin(
                futures::stream::iter(buckets.into_iter().filter(|bucket| {
                    bucket.config.enabled
                        || bucket.config.cookie_lock_enabled
                        || bucket.manual_probe_requested_at.is_some()
                }))
                .for_each_concurrent(4, move |mut bucket| {
                    let service = Arc::clone(&service);
                    let cancellation = probe_cancellation.clone();
                    Box::pin(async move {
                        if cancellation.is_cancelled() {
                            return;
                        }
                        let Ok(account_id) = ProviderAccountId::new(bucket.account_id.clone())
                        else {
                            return;
                        };
                        bucket
                            .routing_cookies
                            .retain(|cookie| cookie.origin == service.endpoint());
                        if bucket.manual_probe_requested_at.is_some() {
                            match service
                                .store
                                .claim_turn_state_probe(&account_id, &bucket.model)
                                .await
                            {
                                Ok(true) => {
                                    tokio::select! {
                                        () = cancellation.cancelled() => {},
                                        _ = service.probe(&bucket, &account_id, true) => {},
                                    }
                                }
                                Ok(false) => {}
                                Err(_) => tracing::warn!(
                                    account_id = bucket.account_id,
                                    model = bucket.model,
                                    "manual turn state probe claim failed"
                                ),
                            }
                            return;
                        }
                        if !bucket.config.cookie_lock_enabled
                            && bucket
                                .installed_token(Utc::now().timestamp())
                                .is_some_and(|token| {
                                    token.is_fresh(
                                        Utc::now().timestamp(),
                                        bucket.config.refresh_after_seconds,
                                    )
                                })
                        {
                            return;
                        }
                        // 被动候选可以随时结束 hunt，不必等到下一次主动探测。
                        if !bucket.config.cookie_lock_enabled
                            && service.install(&account_id, &bucket.model).await
                        {
                            return;
                        }
                        let now = Utc::now().timestamp();
                        if bucket.config.cookie_lock_enabled
                            && !bucket.cookie_renewal_due(now)
                            && bucket
                                .routing_cookies
                                .iter()
                                .filter(|cookie| cookie.is_usable(&bucket.model, now))
                                .count()
                                >= 3
                        {
                            return;
                        }
                        if bucket
                            .next_probe_at
                            .is_some_and(|next| next > Utc::now().timestamp())
                        {
                            return;
                        }
                        let outcome = tokio::select! {
                            () = cancellation.cancelled() => return,
                            outcome = service.probe(&bucket, &account_id, false) => outcome,
                        };
                        if !bucket.config.cookie_lock_enabled
                            && matches!(outcome, ProbeOutcome::Installed)
                        {
                            return;
                        }
                        let mut delay = if matches!(outcome, ProbeOutcome::Skipped)
                            || (bucket.hunt_attempts + 1)
                                .is_multiple_of(u64::from(bucket.config.budget))
                        {
                            bucket.config.idle_seconds
                        } else {
                            bucket.config.retry_seconds
                                + probe::random_index(bucket.config.jitter_seconds as usize + 1)
                                    as u64
                        };
                        // 以本次探测后的池计算上限，长空闲间隔不能错过续约窗口。
                        if bucket.config.cookie_lock_enabled
                            && let Some(current) = service.current(&account_id, &bucket.model).await
                            && let Some(cookie) = current
                                .routing_cookies
                                .iter()
                                .filter(|cookie| {
                                    cookie.is_usable(&bucket.model, Utc::now().timestamp())
                                })
                                .min_by_key(|cookie| cookie.expires_at)
                        {
                            let remaining = cookie.expires_at - Utc::now().timestamp();
                            let before_refresh =
                                remaining - bucket.config.cookie_refresh_before_seconds as i64;
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
                                &account_id,
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
                    }) as BoxFuture<'static, ()>
                }),
            );
            let notifications: BoxFuture<'_, ()> =
                Box::pin(self.service.notify_installations(cancellation));
            tokio::join!(probes, notifications);
            Ok(())
        })
    }
}
