//! 只有已观测到的超限才拦截；缺失或过期观测仅作提示。

use gateway_plugin_sdk::call::data::{QuotaDetails, QuotaWindowDetails};
use serde::Serialize;

use super::config::AccountRule;

const FIVE_HOUR_SECONDS: u64 = 18_000;
const WEEK_SECONDS: u64 = 604_800;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Assessment {
    pub account_id: String,
    pub excluded: bool,
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reason {
    pub code: ReasonCode,
    pub message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    QuotaUnavailable,
    UnsupportedSchema,
    AccountMismatch,
    ObservationMissing,
    ObservationInFuture,
    ObservationStale,
    WindowMissing,
    UsageUnknown,
    ResetUnknown,
    WindowExpired,
    LimitReached,
    DailyLimitReached,
    DailyUsageUnknown,
    DailyObservationStale,
}

impl Reason {
    const fn new(code: ReasonCode, message: &'static str) -> Self {
        Self {
            code,
            message,
            window_seconds: None,
            used_percent: None,
            limit_percent: None,
            reset_at_ms: None,
        }
    }

    fn for_window(
        code: ReasonCode,
        message: &'static str,
        window: &QuotaWindowDetails,
        limit: f64,
    ) -> Self {
        Self {
            window_seconds: window.window_seconds,
            used_percent: window.used_percent.filter(|value| value.is_finite()),
            limit_percent: Some(limit),
            reset_at_ms: window.reset_at_ms,
            ..Self::new(code, message)
        }
    }
}

impl Assessment {
    fn new(rule: &AccountRule, reasons: Vec<Reason>) -> Self {
        Self {
            account_id: rule.account_id.clone(),
            excluded: reasons.iter().any(|reason| {
                matches!(
                    reason.code,
                    ReasonCode::LimitReached | ReasonCode::DailyLimitReached
                )
            }),
            reasons,
        }
    }
}

fn zero_limits(rule: &AccountRule) -> Vec<Reason> {
    [
        (FIVE_HOUR_SECONDS, rule.five_hour_limit_percent),
        (WEEK_SECONDS, rule.weekly_limit_percent),
    ]
    .into_iter()
    .filter(|(_, limit)| *limit == Some(0.0))
    .map(|(seconds, _)| Reason {
        window_seconds: Some(seconds),
        limit_percent: Some(0.0),
        ..Reason::new(ReasonCode::LimitReached, "保护阈值设为 0，已暂停该账号")
    })
    .collect()
}

#[must_use]
pub fn unavailable(rule: &AccountRule) -> Assessment {
    let mut reasons = zero_limits(rule);
    if rule.windows_enabled() {
        reasons.push(Reason::new(
            ReasonCode::QuotaUnavailable,
            "暂未读取到额度观测；此信息不拦截账号",
        ));
    }
    Assessment::new(rule, reasons)
}

#[must_use]
pub fn assess(
    rule: &AccountRule,
    quota: &QuotaDetails,
    now_ms: i64,
    max_age_seconds: u64,
) -> Assessment {
    let mut reasons = zero_limits(rule);
    if !rule.windows_enabled() {
        return Assessment::new(rule, reasons);
    }
    let invalid = if quota.schema_version != 1 {
        Some((
            ReasonCode::UnsupportedSchema,
            "额度观测版本不受支持；此信息不拦截账号",
        ))
    } else if quota.account_id != rule.account_id {
        Some((ReasonCode::AccountMismatch, "额度观测属于其他账号，已忽略"))
    } else if quota.observed_at_ms.is_none() {
        Some((
            ReasonCode::ObservationMissing,
            "缺少额度观测时间；此信息不拦截账号",
        ))
    } else if quota.observed_at_ms.is_some_and(|at| at > now_ms) {
        Some((
            ReasonCode::ObservationInFuture,
            "额度观测时间晚于当前时间，已忽略",
        ))
    } else {
        None
    };
    if let Some((code, message)) = invalid {
        reasons.push(Reason::new(code, message));
        return Assessment::new(rule, reasons);
    }
    let max_age_ms = max_age_seconds.saturating_mul(1_000).min(i64::MAX as u64) as i64;
    let age_ms = quota.observed_at_ms.and_then(|at| now_ms.checked_sub(at));
    let fresh = matches!(age_ms, Some(age) if age >= 0 && age <= max_age_ms);
    if !fresh {
        reasons.push(Reason::new(
            ReasonCode::ObservationStale,
            "额度观测较旧；仅同一窗口的已知超限继续生效",
        ));
    }

    for (seconds, limit) in [
        (FIVE_HOUR_SECONDS, rule.five_hour_limit_percent),
        (WEEK_SECONDS, rule.weekly_limit_percent),
    ] {
        let Some(limit) = limit.filter(|value| *value > 0.0) else {
            continue;
        };
        // primary/secondary 是协议槽位；只使用账号通用的 Codex 窗口。
        let mut found = false;
        for window in quota.windows.iter().filter(|window| {
            window
                .limit_id
                .as_deref()
                .is_some_and(|id| id.trim().eq_ignore_ascii_case("codex"))
                && window.window_seconds == Some(seconds)
        }) {
            found = true;
            if let Some(reason) = assess_window(window, now_ms, limit, fresh) {
                reasons.push(reason);
            }
        }
        if !found {
            reasons.push(Reason {
                window_seconds: Some(seconds),
                limit_percent: Some(limit),
                ..Reason::new(
                    ReasonCode::WindowMissing,
                    "暂未观测到此额度窗口；此信息不拦截账号",
                )
            });
        }
    }
    Assessment::new(rule, reasons)
}

/// 百分比窗口和当天已记录费用独立判断，信息提示不会改变排除状态。
pub fn add_daily(assessment: &mut Assessment, usage: &super::daily::DailyUsage) {
    use super::daily::DailyStatus;
    let (code, message) = match usage.status {
        DailyStatus::Disabled | DailyStatus::Ready => return,
        DailyStatus::Reached => (
            ReasonCode::DailyLimitReached,
            "今日已记录 USD 费用达到保护上限",
        ),
        DailyStatus::Unknown => (
            ReasonCode::DailyUsageUnknown,
            "今日费用尚未完整观测；已知费用未触及限额时仍可使用",
        ),
        DailyStatus::Stale => (
            ReasonCode::DailyObservationStale,
            "今日费用观测待更新；此信息不拦截账号",
        ),
    };
    assessment.excluded |= usage.excluded();
    assessment.reasons.push(Reason {
        window_seconds: Some(86_400),
        reset_at_ms: Some(usage.reset_at_ms),
        ..Reason::new(code, message)
    });
}

fn assess_window(
    window: &QuotaWindowDetails,
    now_ms: i64,
    limit: f64,
    fresh: bool,
) -> Option<Reason> {
    let reason = |code, message| Some(Reason::for_window(code, message, window, limit));
    // 重置边界之后旧窗口不再是当前窗口的超限证据。
    if window.reset_at_ms.is_some_and(|at| at <= now_ms) {
        return reason(
            ReasonCode::WindowExpired,
            "旧额度窗口已结束，等待新观测；此信息不拦截账号",
        );
    }
    // 无重置时间的旧样本不能证明仍处于同一窗口。
    if !fresh && window.reset_at_ms.is_none() {
        return reason(ReasonCode::ResetUnknown, "旧额度观测缺少重置时间，已忽略");
    }
    if window.limit_reached {
        return reason(ReasonCode::LimitReached, "额度窗口已触顶");
    }
    match window
        .used_percent
        .filter(|used| used.is_finite() && *used >= 0.0)
    {
        Some(used) if used >= limit => reason(ReasonCode::LimitReached, "额度使用率已达到保护上限"),
        None => reason(
            ReasonCode::UsageUnknown,
            "额度窗口使用率未知；此信息不拦截账号",
        ),
        Some(_) if window.reset_at_ms.is_none() => reason(
            ReasonCode::ResetUnknown,
            "额度窗口重置时间未知；此信息不拦截账号",
        ),
        Some(_) => None,
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
