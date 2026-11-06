//! 基于宿主持久用量账本的账号级 UTC 每日费用保护。
//!
//! 保护依据已记录费用阈值；记账延迟和仍在执行的请求可能使最终费用超过阈值。
//! 查询覆盖该账号全部请求类型、客户端 Key 和模型，不提供原子预算预占。
//! 重启后重新读取宿主账本构建缓存，不会把重启当作费用归零。

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::services::usage::{
        ListRecords, ListUsageRecords, UsageCost, UsageOutcome, UsageRecordFilter, UsageRecordPage,
    },
    client::HostClient,
};
use serde::Serialize;

use super::config::AccountRule;

const DAY_MS: i64 = 86_400_000;
const SCALE: u128 = 1_000_000_000_000_000_000;
const MAX_PAGES: usize = 100;
const PAGE_SIZE: u16 = 200;
const MAX_CACHE_ACCOUNTS: usize = 1_000;
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(15);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

/// 精确计算 USD：限额允许 12 位小数，宿主费用保留最多 18 位小数。
/// 超出精度或数值溢出时视为未知费用，禁止截断或四舍五入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(super) struct Usd(u128);

impl Usd {
    fn parse(value: &str, fractional_digits: usize) -> Option<Self> {
        let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
        if whole.is_empty()
            || whole.len() > 18
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || fraction.len() > fractional_digits
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            || (value.contains('.') && fraction.is_empty())
        {
            return None;
        }
        let integer = whole.parse::<u128>().ok()?.checked_mul(SCALE)?;
        let fraction_value = if fraction.is_empty() {
            0
        } else {
            fraction
                .parse::<u128>()
                .ok()?
                .checked_mul(10_u128.pow(18 - fraction.len() as u32))?
        };
        integer.checked_add(fraction_value).map(Self)
    }

    pub(super) fn limit(value: &str) -> Option<Self> {
        Self::parse(value, 12)
    }

    fn cost(value: &str) -> Option<Self> {
        Self::parse(value, 18)
    }

    pub(super) fn text(self) -> String {
        let whole = self.0 / SCALE;
        let fraction = self.0 % SCALE;
        if fraction == 0 {
            whole.to_string()
        } else {
            format!(
                "{whole}.{}",
                format!("{fraction:018}").trim_end_matches('0')
            )
        }
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    fn remaining(self, used: Self) -> Self {
        Self(self.0.saturating_sub(used.0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DailyStatus {
    Disabled,
    Ready,
    Reached,
    Unknown,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    pub limit_usd: Option<String>,
    pub used_usd: Option<String>,
    pub remaining_usd: Option<String>,
    pub status: DailyStatus,
    pub unknown_cost_count: u64,
    pub in_flight_count: u64,
    pub observed_at_ms: Option<i64>,
    pub reset_at_ms: i64,
    pub timezone: &'static str,
    pub currency: &'static str,
}

impl DailyUsage {
    pub fn excluded(&self) -> bool {
        matches!(
            self.status,
            DailyStatus::Reached | DailyStatus::Unknown | DailyStatus::Stale
        )
    }
}

#[derive(Debug, Clone)]
struct Snapshot {
    day_start_ms: i64,
    observed_at_ms: i64,
    total: Usd,
    unknown_cost_count: u64,
    in_flight_count: u64,
}

#[derive(Default)]
struct CacheEntry {
    snapshot: Option<Snapshot>,
    failed: bool,
    last_attempt: Option<Instant>,
    refreshing: bool,
}

static CACHE: LazyLock<Mutex<HashMap<String, CacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn day_bounds(now_ms: i64) -> (i64, i64) {
    let start = now_ms.div_euclid(DAY_MS).saturating_mul(DAY_MS);
    (start, start.saturating_add(DAY_MS))
}

fn view(
    rule: &AccountRule,
    entry: Option<&CacheEntry>,
    now_ms: i64,
    max_age_seconds: u64,
) -> DailyUsage {
    let (day_start_ms, reset_at_ms) = day_bounds(now_ms);
    let mut result = DailyUsage {
        limit_usd: rule.daily_limit_usd.clone(),
        used_usd: None,
        remaining_usd: None,
        status: DailyStatus::Disabled,
        unknown_cost_count: 0,
        in_flight_count: 0,
        observed_at_ms: None,
        reset_at_ms,
        timezone: "UTC",
        currency: "USD",
    };
    let Some(raw_limit) = rule.daily_limit_usd.as_deref() else {
        return result;
    };
    result.status = DailyStatus::Unknown;
    let Some(limit) = Usd::limit(raw_limit) else {
        return result;
    };
    result.limit_usd = Some(limit.text());
    // 启用的零阈值立即阻止调度，冷启动时也一样。
    if limit == Usd::default() {
        result.status = DailyStatus::Reached;
    }
    let Some(entry) = entry else {
        return result;
    };
    let Some(snapshot) = entry.snapshot.as_ref() else {
        return result;
    };
    result.observed_at_ms = Some(snapshot.observed_at_ms);
    if snapshot.day_start_ms != day_start_ms {
        if limit != Usd::default() {
            result.status = DailyStatus::Stale;
        }
        return result;
    }
    result.unknown_cost_count = snapshot.unknown_cost_count;
    result.in_flight_count = snapshot.in_flight_count;
    let max_age_ms = max_age_seconds.saturating_mul(1_000).min(i64::MAX as u64) as i64;
    let age = now_ms.checked_sub(snapshot.observed_at_ms);
    if !matches!(age, Some(age) if age >= 0 && age <= max_age_ms) {
        if limit != Usd::default() {
            result.status = DailyStatus::Stale;
        }
        return result;
    }
    if entry.failed || snapshot.unknown_cost_count != 0 {
        return result;
    }
    result.used_usd = Some(snapshot.total.text());
    result.remaining_usd = Some(limit.remaining(snapshot.total).text());
    result.status = if snapshot.total >= limit {
        DailyStatus::Reached
    } else {
        DailyStatus::Ready
    };
    result
}

/// 调度只读取有界缓存，不在请求路径分页扫描整日账本。
pub fn daily_usage(rule: &AccountRule, now_ms: i64, max_age_seconds: u64) -> DailyUsage {
    match CACHE.lock() {
        Ok(cache) => view(rule, cache.get(&rule.account_id), now_ms, max_age_seconds),
        Err(_) => view(rule, None, now_ms, max_age_seconds),
    }
}

fn unavailable() -> PluginFault {
    PluginFault::new(ErrorCode::Fault, "daily usage snapshot unavailable")
}

async fn collect_usage<F, Fut>(
    account_id: &str,
    observed_at_ms: i64,
    mut fetch: F,
) -> Result<Snapshot, PluginFault>
where
    F: FnMut(ListUsageRecords) -> Fut,
    Fut: Future<Output = Result<UsageRecordPage, PluginFault>>,
{
    // 官方账本按 started_at 筛选；跨午夜完成的请求仍归属开始时的 UTC 日期。
    let (day_start_ms, end_ms) = day_bounds(observed_at_ms);
    let mut snapshot = Snapshot {
        day_start_ms,
        observed_at_ms,
        total: Usd::default(),
        unknown_cost_count: 0,
        in_flight_count: 0,
    };
    let mut cursor = None;
    let mut cursors = HashSet::new();
    let mut records = HashSet::new();
    for _ in 0..MAX_PAGES {
        let page = fetch(ListUsageRecords {
            filter: UsageRecordFilter {
                start_ms: day_start_ms,
                end_ms,
                provider: None,
                model: None,
                account_ref: Some(account_id.to_owned()),
                client_key_ref: None,
            },
            cursor: cursor.clone(),
            limit: Some(PAGE_SIZE),
        })
        .await?;
        if page.items.len() > usize::from(PAGE_SIZE) {
            return Err(unavailable());
        }
        for record in page.items {
            if record.id.is_empty()
                || record.id.len() > 512
                || !records.insert(record.id)
                || record.data.account_ref.as_deref() != Some(account_id)
                || !(day_start_ms..end_ms).contains(&record.data.started_at_ms)
            {
                // 部分页、重复记录或归属不匹配的结果不能视为完整总额。
                return Err(unavailable());
            }
            if record.data.outcome == UsageOutcome::Running {
                snapshot.in_flight_count += 1;
                continue;
            }
            let cost = match record.data.cost {
                UsageCost::Known {
                    amount, currency, ..
                } if currency.eq_ignore_ascii_case("USD") => Usd::cost(&amount),
                _ => None,
            };
            match cost.and_then(|cost| snapshot.total.checked_add(cost)) {
                Some(total) => snapshot.total = total,
                None => snapshot.unknown_cost_count += 1,
            }
        }
        match page.next_cursor {
            None => return Ok(snapshot),
            Some(next)
                if !next.is_empty() && next.len() <= 4_096 && cursors.insert(next.clone()) =>
            {
                cursor = Some(next);
            }
            _ => return Err(unavailable()),
        }
    }
    Err(unavailable())
}

/// 刷新名额不持互斥锁守卫；取消或超时会使旧观测失效并释放刷新标记。
struct RefreshLease {
    account_id: String,
    completed: bool,
}

impl RefreshLease {
    fn begin(account_id: &str) -> Option<Self> {
        let mut cache = CACHE.lock().ok()?;
        if !cache.contains_key(account_id) && cache.len() >= MAX_CACHE_ACCOUNTS {
            return None;
        }
        let entry = cache.entry(account_id.to_owned()).or_default();
        if entry.refreshing
            || entry
                .last_attempt
                .is_some_and(|at| at.elapsed() < MIN_REFRESH_INTERVAL)
        {
            return None;
        }
        entry.refreshing = true;
        entry.last_attempt = Some(Instant::now());
        Some(Self {
            account_id: account_id.to_owned(),
            completed: false,
        })
    }

    fn finish(mut self, snapshot: Option<Snapshot>) {
        if let Ok(mut cache) = CACHE.lock() {
            if let Some(entry) = cache.get_mut(&self.account_id) {
                entry.failed = snapshot.is_none();
                if let Some(snapshot) = snapshot {
                    entry.snapshot = Some(snapshot);
                }
                entry.refreshing = false;
            }
        }
        self.completed = true;
    }
}

impl Drop for RefreshLease {
    fn drop(&mut self) {
        if !self.completed {
            if let Ok(mut cache) = CACHE.lock() {
                if let Some(entry) = cache.get_mut(&self.account_id) {
                    entry.failed = true;
                    entry.refreshing = false;
                }
            }
        }
    }
}

/// 宿主持久用量记录充当账本，不新增插件状态命名空间或推测计数。
/// 刷新不完整时保持账号关闭，等待后续完整观测。
pub async fn refresh_daily_usage(
    host: &HostClient,
    rule: &AccountRule,
    now_ms: i64,
) -> Result<(), PluginFault> {
    if rule.daily_limit_usd.is_none() {
        return Ok(());
    }
    let Some(lease) = RefreshLease::begin(&rule.account_id) else {
        return Ok(());
    };
    let operation = collect_usage(&rule.account_id, now_ms, |query| {
        super::host_calls::limited(async move {
            host.service::<ListRecords>(query).await.map_err(|error| {
                // 不透传服务错误正文、计费快照或详细资料。
                if error.kind == "cancelled"
                    || error
                        .details
                        .as_ref()
                        .and_then(|details| details.get("code"))
                        .and_then(serde_json::Value::as_str)
                        == Some("cancelled")
                {
                    PluginFault::new(ErrorCode::Cancelled, "daily usage refresh cancelled")
                } else {
                    unavailable()
                }
            })
        })
    });
    match tokio::time::timeout(REFRESH_TIMEOUT, operation).await {
        Ok(Ok(snapshot)) => {
            lease.finish(Some(snapshot));
            Ok(())
        }
        Ok(Err(fault)) => {
            lease.finish(None);
            if fault.code == ErrorCode::Cancelled {
                Err(fault)
            } else {
                Ok(())
            }
        }
        Err(_) => {
            lease.finish(None);
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "daily_tests.rs"]
mod tests;
