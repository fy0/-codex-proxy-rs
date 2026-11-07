//! 基于宿主持久用量账本的账号级本地日费用保护。
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

use chrono_tz::Tz;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::services::usage::{
        ListRecords, ListUsageRecords, UsageCost, UsageOutcome, UsageRecordFilter, UsageRecordPage,
    },
    client::HostClient,
};
use serde::Serialize;

use super::{
    config::AccountRule,
    day_window::{DailyCalibration, DayWindow},
};

#[cfg(test)]
const DAY_MS: i64 = 86_400_000;
const SCALE: u128 = 1_000_000_000_000_000_000;
const MAX_PAGES: usize = 100;
const PAGE_SIZE: u16 = 200;
const MAX_CACHE_ACCOUNTS: usize = 1_000;
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(15);
const REFRESH_TIMEOUT: Duration = Duration::from_millis(4_500);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncompleteReason {
    NotObserved,
    WindowChanged,
    CalibrationChanged,
    UnpricedRecords,
    UnsupportedCost,
    ReadFailed,
    ReadTimeout,
    PageLimit,
    InvalidRecords,
    InvalidCursor,
    ObservationStale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveCalibration {
    #[serde(flatten)]
    pub calibration: DailyCalibration,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    pub limit_usd: Option<String>,
    /// 用于限额判断：原始账本，或手工基准加校准后开始的请求费用。
    pub used_usd: Option<String>,
    pub remaining_usd: Option<String>,
    pub status: DailyStatus,
    /// 有效记账区间内已结束请求是否完整计价；运行中请求另行计数。
    pub complete: bool,
    pub incomplete_reasons: Vec<IncompleteReason>,
    pub known_cost_count: u64,
    pub unknown_cost_count: u64,
    pub in_flight_count: u64,
    pub ledger_used_usd: Option<String>,
    pub ledger_known_cost_count: u64,
    pub ledger_unknown_cost_count: u64,
    pub ledger_in_flight_count: u64,
    pub post_calibration_used_usd: Option<String>,
    pub calibration: Option<ActiveCalibration>,
    pub observed_at_ms: Option<i64>,
    pub window_start_ms: Option<i64>,
    pub window_end_ms: Option<i64>,
    pub reset_at_ms: Option<i64>,
    pub timezone: Tz,
    pub currency: &'static str,
}

impl DailyUsage {
    pub fn excluded(&self) -> bool {
        self.status == DailyStatus::Reached
    }
}

#[derive(Debug, Clone, Default)]
struct CostTotal {
    total: Usd,
    known_cost_count: u64,
    unknown_cost_count: u64,
    in_flight_count: u64,
    unpriced: bool,
    unsupported: bool,
}

impl CostTotal {
    fn record(&mut self, cost: &UsageCost, outcome: &UsageOutcome) {
        if *outcome == UsageOutcome::Running {
            self.in_flight_count += 1;
            return;
        }
        let amount = match cost {
            UsageCost::Known {
                amount, currency, ..
            } if currency == "USD" => Usd::cost(amount),
            UsageCost::Unavailable
                if matches!(outcome, UsageOutcome::Failed | UsageOutcome::Cancelled) =>
            {
                return;
            }
            UsageCost::Unavailable => {
                self.unpriced = true;
                self.unknown_cost_count += 1;
                return;
            }
            _ => None,
        };
        if let Some(total) = amount.and_then(|amount| self.total.checked_add(amount)) {
            self.total = total;
            self.known_cost_count += 1;
        } else {
            self.unsupported = true;
            self.unknown_cost_count += 1;
        }
    }

    fn known_amount(&self, all_pages: bool) -> Option<Usd> {
        (self.known_cost_count > 0 || (all_pages && self.unknown_cost_count == 0))
            .then_some(self.total)
    }

    fn retain_known_lower_bound(&mut self, previous: &Self, all_pages: bool) {
        if (!all_pages || self.unknown_cost_count != 0) && previous.total > self.total {
            self.total = previous.total;
            self.known_cost_count = previous.known_cost_count;
        }
    }
}

#[derive(Debug, Clone)]
struct Snapshot {
    window: DayWindow,
    observed_at_ms: i64,
    ledger: CostTotal,
    calibration_at_ms: Option<i64>,
    after_calibration: CostTotal,
    complete: bool,
    read_reason: Option<IncompleteReason>,
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

fn active_calibration(
    rule: &AccountRule,
    window: DayWindow,
    now_ms: i64,
) -> Option<&DailyCalibration> {
    rule.daily_calibration
        .as_ref()
        .filter(|value| value.active_in(window, now_ms))
}

fn view(
    rule: &AccountRule,
    entry: Option<&CacheEntry>,
    now_ms: i64,
    max_age_seconds: u64,
    timezone: Tz,
) -> DailyUsage {
    let window = DayWindow::at(now_ms, timezone);
    let calibration = window.and_then(|window| active_calibration(rule, window, now_ms));
    let baseline = calibration.and_then(|value| Usd::limit(&value.amount_usd));
    let mut result = DailyUsage {
        limit_usd: rule.daily_limit_usd.clone(),
        used_usd: baseline.map(Usd::text),
        remaining_usd: None,
        status: DailyStatus::Disabled,
        complete: false,
        incomplete_reasons: Vec::new(),
        known_cost_count: 0,
        unknown_cost_count: 0,
        in_flight_count: 0,
        ledger_used_usd: None,
        ledger_known_cost_count: 0,
        ledger_unknown_cost_count: 0,
        ledger_in_flight_count: 0,
        post_calibration_used_usd: None,
        calibration: calibration.cloned().map(|calibration| ActiveCalibration {
            calibration,
            active: true,
        }),
        observed_at_ms: None,
        window_start_ms: window.map(|value| value.start_ms),
        window_end_ms: window.map(|value| value.end_ms),
        reset_at_ms: window.map(|value| value.end_ms),
        timezone,
        currency: "USD",
    };
    let Some(limit) = rule.daily_limit_usd.as_deref().and_then(Usd::limit) else {
        return result;
    };
    result.limit_usd = Some(limit.text());
    result.status = DailyStatus::Unknown;
    let mut used = baseline;
    let mut stale = false;
    if let Some((entry, snapshot)) =
        entry.and_then(|entry| entry.snapshot.as_ref().map(|snapshot| (entry, snapshot)))
    {
        if Some(snapshot.window) != window {
            result
                .incomplete_reasons
                .push(IncompleteReason::WindowChanged);
            stale = true;
        } else if snapshot.observed_at_ms > now_ms {
            result
                .incomplete_reasons
                .push(IncompleteReason::ObservationStale);
            stale = true;
        } else {
            let raw = &snapshot.ledger;
            result.ledger_used_usd = raw.known_amount(snapshot.complete).map(Usd::text);
            result.ledger_known_cost_count = raw.known_cost_count;
            result.ledger_unknown_cost_count = raw.unknown_cost_count;
            result.ledger_in_flight_count = raw.in_flight_count;
            let cutover = calibration.map(|value| value.at_ms);
            // 全天原始总额仍可展示；校准切点不同不能复用旧的增量。
            if cutover.is_some() && cutover != snapshot.calibration_at_ms {
                result
                    .incomplete_reasons
                    .push(IncompleteReason::CalibrationChanged);
            } else {
                result.observed_at_ms = Some(snapshot.observed_at_ms);
                let effective = if calibration.is_some() {
                    &snapshot.after_calibration
                } else {
                    raw
                };
                result.known_cost_count = effective.known_cost_count;
                result.unknown_cost_count = effective.unknown_cost_count;
                result.in_flight_count = effective.in_flight_count;
                if let Some(base) = baseline {
                    result.post_calibration_used_usd =
                        effective.known_amount(snapshot.complete).map(Usd::text);
                    used = Some(base.checked_add(effective.total).unwrap_or_else(|| {
                        result
                            .incomplete_reasons
                            .push(IncompleteReason::UnsupportedCost);
                        base.max(effective.total)
                    }));
                } else {
                    used = effective.known_amount(snapshot.complete);
                }
                if effective.unpriced {
                    result
                        .incomplete_reasons
                        .push(IncompleteReason::UnpricedRecords);
                }
                if effective.unsupported
                    && !result
                        .incomplete_reasons
                        .contains(&IncompleteReason::UnsupportedCost)
                {
                    result
                        .incomplete_reasons
                        .push(IncompleteReason::UnsupportedCost);
                }
                if let Some(reason) = snapshot.read_reason {
                    result.incomplete_reasons.push(reason);
                }
                if entry.failed
                    && !result
                        .incomplete_reasons
                        .contains(&IncompleteReason::ReadTimeout)
                {
                    result
                        .incomplete_reasons
                        .push(IncompleteReason::ReadTimeout);
                }
                let max_age_ms = max_age_seconds.saturating_mul(1_000).min(i64::MAX as u64) as i64;
                stale = now_ms.saturating_sub(snapshot.observed_at_ms) > max_age_ms;
                if stale {
                    result
                        .incomplete_reasons
                        .push(IncompleteReason::ObservationStale);
                }
                result.complete = snapshot.complete && result.incomplete_reasons.is_empty();
            }
        }
    } else {
        result
            .incomplete_reasons
            .push(if entry.is_some_and(|entry| entry.failed) {
                IncompleteReason::ReadTimeout
            } else {
                IncompleteReason::NotObserved
            });
    }
    result.used_usd = used.map(Usd::text);
    // 用户显式设为 0，或同一窗口内的已知金额达到上限，才排除账号。
    result.status = if limit == Usd::default() || used.is_some_and(|used| used >= limit) {
        DailyStatus::Reached
    } else if stale {
        DailyStatus::Stale
    } else if result.complete {
        DailyStatus::Ready
    } else {
        DailyStatus::Unknown
    };
    if result.complete || result.excluded() {
        result.remaining_usd = used.map(|used| limit.remaining(used).text());
        if limit == Usd::default() {
            result.remaining_usd = Some("0".into());
        }
    }
    result
}

/// 调度只读取有界缓存，不在请求路径分页扫描整日账本。
pub fn daily_usage(
    rule: &AccountRule,
    now_ms: i64,
    max_age_seconds: u64,
    timezone: Tz,
) -> DailyUsage {
    match CACHE.lock() {
        Ok(cache) => view(
            rule,
            cache.get(&rule.account_id),
            now_ms,
            max_age_seconds,
            timezone,
        ),
        Err(_) => view(rule, None, now_ms, max_age_seconds, timezone),
    }
}

fn unavailable() -> PluginFault {
    PluginFault::new(ErrorCode::Fault, "daily usage snapshot unavailable")
}

async fn collect_usage<F, Fut>(
    account_id: &str,
    observed_at_ms: i64,
    window: DayWindow,
    calibration_at_ms: Option<i64>,
    deadline: tokio::time::Instant,
    mut fetch: F,
) -> Result<Snapshot, PluginFault>
where
    F: FnMut(ListUsageRecords) -> Fut,
    Fut: Future<Output = Result<UsageRecordPage, PluginFault>>,
{
    let mut snapshot = Snapshot {
        window,
        observed_at_ms,
        ledger: CostTotal::default(),
        calibration_at_ms,
        after_calibration: CostTotal::default(),
        complete: false,
        read_reason: None,
    };
    let mut cursor = None;
    let mut cursors = HashSet::new();
    let mut ids = HashSet::new();
    for _ in 0..MAX_PAGES {
        let query = ListUsageRecords {
            filter: UsageRecordFilter {
                start_ms: window.start_ms,
                end_ms: window.end_ms,
                provider: None,
                account_ref: Some(account_id.to_owned()),
                model: None,
                client_key_ref: None,
            },
            cursor: cursor.clone(),
            limit: Some(PAGE_SIZE),
        };
        let page = match tokio::time::timeout_at(deadline, fetch(query)).await {
            Ok(Ok(page)) => page,
            Ok(Err(fault)) if fault.code == ErrorCode::Cancelled => return Err(fault),
            Ok(Err(_)) => {
                snapshot.read_reason = Some(IncompleteReason::ReadFailed);
                return Ok(snapshot);
            }
            Err(_) => {
                snapshot.read_reason = Some(IncompleteReason::ReadTimeout);
                return Ok(snapshot);
            }
        };
        if page.items.len() > usize::from(PAGE_SIZE) {
            snapshot.read_reason = Some(IncompleteReason::InvalidRecords);
            return Ok(snapshot);
        }
        for record in page.items {
            if record.id.is_empty()
                || record.id.len() > 512
                || !ids.insert(record.id)
                || record.data.account_ref.as_deref() != Some(account_id)
                || record.data.started_at_ms < window.start_ms
                || record.data.started_at_ms >= window.end_ms
            {
                snapshot.read_reason = Some(IncompleteReason::InvalidRecords);
                return Ok(snapshot);
            }
            snapshot
                .ledger
                .record(&record.data.cost, &record.data.outcome);
            if calibration_at_ms.is_some_and(|at| record.data.started_at_ms >= at) {
                snapshot
                    .after_calibration
                    .record(&record.data.cost, &record.data.outcome);
            }
        }
        match page.next_cursor {
            None => {
                snapshot.complete = true;
                return Ok(snapshot);
            }
            Some(next)
                if !next.is_empty() && next.len() <= 4_096 && cursors.insert(next.clone()) =>
            {
                cursor = Some(next);
            }
            Some(_) => {
                snapshot.read_reason = Some(IncompleteReason::InvalidCursor);
                return Ok(snapshot);
            }
        }
    }
    snapshot.read_reason = Some(IncompleteReason::PageLimit);
    Ok(snapshot)
}

/// 刷新名额不持互斥锁守卫；取消或超时释放名额，保留当前窗口已知费用。
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
                if let Some(mut snapshot) = snapshot {
                    if let Some(previous) = entry.snapshot.as_ref() {
                        // 部分读取保留同一日窗口的已知下界；完整账本可纠正旧值。
                        if previous.window == snapshot.window
                            && previous.observed_at_ms <= snapshot.observed_at_ms
                        {
                            snapshot
                                .ledger
                                .retain_known_lower_bound(&previous.ledger, snapshot.complete);
                            if previous.calibration_at_ms == snapshot.calibration_at_ms {
                                snapshot.after_calibration.retain_known_lower_bound(
                                    &previous.after_calibration,
                                    snapshot.complete,
                                );
                            }
                        }
                    }
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

/// 宿主持久记录是原始账本；校准仅设置当前日的预算基准，不改写原始费用。
pub async fn refresh_daily_usage(
    host: &HostClient,
    rule: &AccountRule,
    now_ms: i64,
    timezone: Tz,
) -> Result<(), PluginFault> {
    if rule.daily_limit_usd.is_none() {
        return Ok(());
    }
    let Some(window) = DayWindow::at(now_ms, timezone) else {
        return Ok(());
    };
    let Some(lease) = RefreshLease::begin(&rule.account_id) else {
        return Ok(());
    };
    let cutover = active_calibration(rule, window, now_ms).map(|value| value.at_ms);
    let deadline = tokio::time::Instant::now() + REFRESH_TIMEOUT;
    let result = collect_usage(
        &rule.account_id,
        now_ms,
        window,
        cutover,
        deadline,
        |query| {
            super::host_calls::limited(async move {
                host.service::<ListRecords>(query).await.map_err(|error| {
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
        },
    )
    .await;
    match result {
        Ok(snapshot) => {
            lease.finish(Some(snapshot));
            Ok(())
        }
        Err(fault) => {
            lease.finish(None);
            Err(fault)
        }
    }
}

#[cfg(test)]
#[path = "daily_tests.rs"]
mod tests;
