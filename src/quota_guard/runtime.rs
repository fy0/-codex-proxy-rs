use super::{QuotaGuardConfig, assess, unavailable};
use futures_util::{StreamExt as _, stream};
use gateway_plugin_sdk::{ErrorCode, PluginFault, call::data::QuotaFactsQuery, client::HostClient};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const MAX_REFRESHES: usize = 20;
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

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
                let quota = host
                    .quota_details(QuotaFactsQuery {
                        account_id: rule.account_id.clone(),
                    })
                    .await;
                let forbidden = match quota {
                    Ok(details) => assess(&rule, &details, now_ms()?, max_age).excluded,
                    Err(e) if e.code == ErrorCode::Cancelled => return Err(e),
                    Err(_) => unavailable(&rule).excluded,
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
    attempts: HashMap<String, Instant>,
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
    pub async fn reconcile(&self, host: &HostClient) -> Result<(), PluginFault> {
        let Ok(mut state) = self.state.try_lock() else {
            return Ok(());
        };
        let rules = &self.config.accounts;
        if rules.is_empty() {
            return Ok(());
        }
        let mut refreshed = 0;
        for _ in 0..rules.len() {
            if refreshed >= MAX_REFRESHES {
                break;
            }
            let i = state.cursor % rules.len();
            let rule = &rules[i];
            state.cursor = (i + 1) % rules.len();
            if !rule.is_enabled()
                || state
                    .attempts
                    .get(&rule.account_id)
                    .is_some_and(|at| at.elapsed() < MIN_REFRESH_INTERVAL)
            {
                continue;
            }
            let query = QuotaFactsQuery {
                account_id: rule.account_id.clone(),
            };
            let obsolete = match host.quota_details(query.clone()).await {
                Ok(details) => assess(
                    rule,
                    &details,
                    now_ms()?,
                    self.config.max_observation_age_seconds,
                )
                .reasons
                .iter()
                .any(|reason| reason.code != super::policy::ReasonCode::LimitReached),
                Err(e) if e.code == ErrorCode::Cancelled => return Err(e),
                Err(_) => true,
            };
            if obsolete {
                state
                    .attempts
                    .insert(rule.account_id.clone(), Instant::now());
                refreshed += 1;
                if let Err(e) = host.refresh_account_quota(query).await {
                    if e.code == ErrorCode::Cancelled {
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }
}
