use super::host_calls::limited;
use super::{QuotaGuardConfig, assess, daily::refresh_daily_usage, daily_usage, unavailable};
use futures_util::{StreamExt as _, stream};
use gateway_plugin_sdk::{ErrorCode, PluginFault, call::data::QuotaFactsQuery, client::HostClient};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, Notify};

const MAX_CHECKS: usize = 20;
const MAX_PARALLEL_REFRESHES: usize = 4;
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const DAILY_CHECK_INTERVAL: Duration = Duration::from_secs(15);
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const QUOTA_REFRESH_TIMEOUT: Duration = Duration::from_secs(3);
const RECONCILE_BUDGET: Duration = Duration::from_secs(8);
const OVERVIEW_BUDGET: Duration = Duration::from_secs(5);

/// 在同一父调用内刷新额度，保留两条任务的结果；后台任务结束通知不会丢失。
async fn reconcile_until_jobs_finish<Q, QF, J>(
    mut refresh: Q,
    jobs: J,
    interval: Duration,
) -> (Result<(), PluginFault>, Result<(), PluginFault>)
where
    Q: FnMut() -> QF,
    QF: Future<Output = Result<(), PluginFault>>,
    J: Future<Output = Result<(), PluginFault>>,
{
    let jobs_completed = Notify::new();
    let quota_work = async {
        let mut first_error = None;
        loop {
            // 即使图片队列为空，也先完成首次有界刷新，再消费结束通知。
            if let Err(error) = refresh().await {
                first_error.get_or_insert(error);
            }
            tokio::select! {
                biased;
                _ = jobs_completed.notified() => break,
                _ = tokio::time::sleep(interval) => {}
            }
        }
        first_error.map_or(Ok(()), Err)
    };
    let image_work = async {
        let result = jobs.await;
        // Notify 保留一个许可，图片先结束也不会使刷新循环永久等待。
        jobs_completed.notify_one();
        result
    };
    // 额度错误只在两条任务结束后返回，不提前取消正在计费的图片执行。
    tokio::join!(quota_work, image_work)
}

fn now_ms() -> Result<i64, PluginFault> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .ok_or_else(|| PluginFault::new(ErrorCode::Fault, "quota clock unavailable"))
}

pub async fn protected_ids(
    host: &HostClient,
    candidate_ids: &[String],
    config: &QuotaGuardConfig,
) -> Result<HashSet<String>, PluginFault> {
    if config.accounts.is_empty() {
        return Ok(HashSet::new());
    }
    let wanted: HashSet<&str> = candidate_ids.iter().map(String::as_str).collect();
    let list = config
        .accounts
        .iter()
        .filter(|rule| wanted.contains(rule.account_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let host = host.clone();
    let max_age = config.max_observation_age_seconds;
    let results = stream::iter(list)
        .map(move |rule| {
            let host = host.clone();
            async move {
                if !rule.is_enabled() {
                    return Ok(None);
                }
                // 所有账号调度共用每日保护，不限于图片任务。
                // 冷缓存、过期或不完整的观测直接阻止账号；请求路径不扫描日账本。
                if daily_usage(&rule, now_ms()?, max_age).excluded() {
                    return Ok(Some(rule.account_id));
                }
                if !rule.windows_enabled() {
                    return Ok(None);
                }
                let quota = tokio::time::timeout(
                    QUERY_TIMEOUT,
                    limited(host.quota_details(QuotaFactsQuery {
                        account_id: rule.account_id.clone(),
                    })),
                )
                .await;
                let forbidden = match quota {
                    Ok(Ok(details)) => assess(&rule, &details, now_ms()?, max_age).excluded,
                    Ok(Err(e)) if e.code == ErrorCode::Cancelled => return Err(e),
                    _ => unavailable(&rule).excluded,
                };
                Ok(if forbidden {
                    Some(rule.account_id)
                } else {
                    None
                })
            }
        })
        .buffer_unordered(8)
        .collect::<Vec<Result<Option<String>, PluginFault>>>()
        .await;
    let mut excluded = HashSet::new();
    for result in results {
        if let Some(id) = result? {
            excluded.insert(id);
        }
    }
    Ok(excluded)
}

#[derive(Default)]
struct RefreshState {
    cursor: usize,
    checks: HashMap<String, Instant>,
    refreshes: HashMap<String, Instant>,
}

impl RefreshState {
    fn select(&mut self, rules: &[super::AccountRule], at: Instant) -> Vec<super::AccountRule> {
        let mut selected = Vec::new();
        for _ in 0..rules.len() {
            if selected.len() >= MAX_CHECKS {
                break;
            }
            let index = self.cursor % rules.len();
            self.cursor = (index + 1) % rules.len();
            let rule = &rules[index];
            let interval = if rule.daily_limit_usd.is_some() {
                DAILY_CHECK_INTERVAL
            } else {
                MIN_REFRESH_INTERVAL
            };
            if !rule.is_enabled()
                || self
                    .checks
                    .get(&rule.account_id)
                    .is_some_and(|previous| at.saturating_duration_since(*previous) < interval)
            {
                continue;
            }
            self.checks.insert(rule.account_id.clone(), at);
            selected.push(rule.clone());
        }
        selected
    }

    fn reserve_refresh(&mut self, account_id: &str, at: Instant) -> bool {
        if self
            .refreshes
            .get(account_id)
            .is_some_and(|previous| at.saturating_duration_since(*previous) < MIN_REFRESH_INTERVAL)
        {
            return false;
        }
        self.refreshes.insert(account_id.to_owned(), at);
        true
    }
}

pub struct QuotaGuard {
    config: Arc<QuotaGuardConfig>,
    state: Mutex<RefreshState>,
}

impl QuotaGuard {
    pub fn new(config: Arc<QuotaGuardConfig>) -> Self {
        Self {
            config,
            state: Mutex::new(RefreshState::default()),
        }
    }

    async fn refresh_windows(
        &self,
        host: &HostClient,
        rule: &super::AccountRule,
    ) -> Result<(), PluginFault> {
        if !rule.windows_enabled() {
            return Ok(());
        }
        let query = QuotaFactsQuery {
            account_id: rule.account_id.clone(),
        };
        let obsolete =
            match tokio::time::timeout(QUERY_TIMEOUT, limited(host.quota_details(query.clone())))
                .await
            {
                Ok(Ok(details)) => assess(
                    rule,
                    &details,
                    now_ms()?,
                    self.config.max_observation_age_seconds,
                )
                .reasons
                .iter()
                .any(|reason| reason.code != super::policy::ReasonCode::LimitReached),
                Ok(Err(error)) if error.code == ErrorCode::Cancelled => return Err(error),
                _ => true,
            };
        if obsolete {
            // 锁只用于预留一次刷新；等待宿主或网络时不持锁。
            let reserved = self
                .state
                .lock()
                .await
                .reserve_refresh(&rule.account_id, Instant::now());
            if reserved {
                if let Ok(Err(error)) = tokio::time::timeout(
                    QUOTA_REFRESH_TIMEOUT,
                    limited(host.refresh_account_quota(query)),
                )
                .await
                {
                    if error.code == ErrorCode::Cancelled {
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }

    /// 图片执行期间每轮完成后等待 15 秒继续刷新；全部工作仍归属当前父调用。
    pub async fn reconcile_with_jobs<J>(
        &self,
        host: &HostClient,
        jobs: J,
    ) -> (Result<(), PluginFault>, Result<(), PluginFault>)
    where
        J: Future<Output = Result<(), PluginFault>>,
    {
        reconcile_until_jobs_finish(|| self.reconcile(host), jobs, DAILY_CHECK_INTERVAL).await
    }

    pub async fn reconcile(&self, host: &HostClient) -> Result<(), PluginFault> {
        let selected = {
            let Ok(mut state) = self.state.try_lock() else {
                return Ok(());
            };
            state.select(&self.config.accounts, Instant::now())
        };
        let work = stream::iter(selected)
            .map(|rule| async move {
                let now = now_ms()?;
                let (windows, daily) = tokio::join!(
                    self.refresh_windows(host, &rule),
                    refresh_daily_usage(host, &rule, now),
                );
                windows?;
                daily
            })
            .buffer_unordered(MAX_PARALLEL_REFRESHES)
            .collect::<Vec<Result<(), PluginFault>>>();
        // 账号较多、额度服务缓慢或日账本较大时，维护回调仍受总预算约束。
        // 被取消的每日刷新会释放名额，并保持未知观测对应的账号关闭。
        if let Ok(results) = tokio::time::timeout(RECONCILE_BUDGET, work).await {
            for result in results {
                result?;
            }
        }
        Ok(())
    }
}

/// 页面刷新可在预算内补齐每日观测，与维护任务共用缓存和单账号刷新锁。
/// 优先刷新没有观测或观测最旧的账号。
pub async fn refresh_daily_overview(
    host: &HostClient,
    config: &QuotaGuardConfig,
) -> Result<(), PluginFault> {
    let now = now_ms()?;
    let mut rules = config
        .accounts
        .iter()
        .filter(|rule| rule.daily_limit_usd.is_some())
        .cloned()
        .filter_map(|rule| {
            let usage = daily_usage(&rule, now, config.max_observation_age_seconds);
            let observed = usage.observed_at_ms.unwrap_or(i64::MIN);
            if observed == i64::MIN || now.saturating_sub(observed) >= 15_000 {
                Some((observed, rule))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    rules.sort_by_key(|(observed, _)| *observed);
    let work = stream::iter(rules.into_iter().take(MAX_CHECKS))
        .map(|(_, rule)| async move { refresh_daily_usage(host, &rule, now).await })
        .buffer_unordered(MAX_PARALLEL_REFRESHES)
        .collect::<Vec<_>>();
    if let Ok(results) = tokio::time::timeout(OVERVIEW_BUDGET, work).await {
        for result in results {
            result?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[tokio::test]
    async fn long_jobs_receive_repeated_refreshes_before_finishing() {
        let refreshes = AtomicUsize::new(0);
        let second_refresh = Notify::new();
        let (quota_result, jobs_result) = tokio::time::timeout(
            Duration::from_secs(2),
            reconcile_until_jobs_finish(
                || async {
                    if refreshes.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                        second_refresh.notify_one();
                    }
                    Ok(())
                },
                async {
                    second_refresh.notified().await;
                    Ok(())
                },
                Duration::from_millis(1),
            ),
        )
        .await
        .expect("long jobs must not prevent the second quota refresh");
        quota_result.unwrap();
        jobs_result.unwrap();
        assert!(refreshes.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn empty_jobs_still_wait_for_the_initial_refresh_to_finish() {
        let refreshes = AtomicUsize::new(0);
        let refreshed = AtomicBool::new(false);
        let (quota_result, jobs_result) = tokio::time::timeout(
            Duration::from_secs(2),
            reconcile_until_jobs_finish(
                || async {
                    refreshes.fetch_add(1, Ordering::SeqCst);
                    // 让空图片任务先完成，证明初次额度刷新不会因此被取消。
                    tokio::task::yield_now().await;
                    refreshed.store(true, Ordering::SeqCst);
                    Ok(())
                },
                std::future::ready(Ok(())),
                Duration::from_millis(1),
            ),
        )
        .await
        .expect("an early completion notification must not be lost");
        quota_result.unwrap();
        jobs_result.unwrap();
        assert!(refreshed.load(Ordering::SeqCst));
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn quota_failure_does_not_drop_jobs_or_hide_either_result() {
        let refreshes = AtomicUsize::new(0);
        let second_refresh = Notify::new();
        let (quota_result, jobs_result) = tokio::time::timeout(
            Duration::from_secs(2),
            reconcile_until_jobs_finish(
                || async {
                    if refreshes.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err(PluginFault::new(ErrorCode::Fault, "quota refresh failed"));
                    }
                    second_refresh.notify_one();
                    Ok(())
                },
                async {
                    second_refresh.notified().await;
                    Err(PluginFault::new(ErrorCode::Rejected, "image job finished"))
                },
                Duration::from_millis(1),
            ),
        )
        .await
        .expect("a quota error must not abandon the image future");
        assert_eq!(quota_result.unwrap_err().code, ErrorCode::Fault);
        assert_eq!(jobs_result.unwrap_err().code, ErrorCode::Rejected);
        assert!(refreshes.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn refresh_selection_is_bounded_fair_and_attempts_do_not_hold_a_network_lock() {
        let rules = (0..25)
            .map(|index| {
                serde_json::from_value(json!({
                    "accountId": format!("account-{index}"),
                    "fiveHourLimitPercent": 95,
                    "weeklyLimitPercent": 95,
                }))
                .unwrap()
            })
            .collect::<Vec<super::super::AccountRule>>();
        let mut state = RefreshState::default();
        let now = Instant::now();
        let first = state.select(&rules, now);
        assert_eq!(first.len(), MAX_CHECKS);
        let second = state.select(&rules, now);
        assert_eq!(second.len(), 5);
        assert!(state.select(&rules, now).is_empty());
        assert!(
            first
                .iter()
                .all(|a| second.iter().all(|b| a.account_id != b.account_id))
        );
        assert!(state.reserve_refresh("account-1", now));
        assert!(!state.reserve_refresh("account-1", now + Duration::from_secs(59)));
        assert!(state.reserve_refresh("account-1", now + Duration::from_secs(60)));
    }
}
