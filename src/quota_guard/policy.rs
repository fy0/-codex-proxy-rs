//! 根据宿主额度事实评估账号保护，不缓存或推算额度恢复

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
            excluded: !reasons.is_empty(),
            reasons,
        }
    }
}

#[must_use]
pub fn unavailable(rule: &AccountRule) -> Assessment {
    Assessment::new(
        rule,
        if rule.windows_enabled() {
            vec![Reason::new(
                ReasonCode::QuotaUnavailable,
                "无法读取额度观测，暂不使用此账号",
            )]
        } else {
            Vec::new()
        },
    )
}

#[must_use]
pub fn assess(
    rule: &AccountRule,
    quota: &QuotaDetails,
    now_ms: i64,
    max_age_seconds: u64,
) -> Assessment {
    if !rule.windows_enabled() {
        return Assessment::new(rule, Vec::new());
    }
    if quota.schema_version != 1 {
        return Assessment::new(
            rule,
            vec![Reason::new(
                ReasonCode::UnsupportedSchema,
                "额度观测版本不受支持，暂不使用此账号",
            )],
        );
    }
    if quota.account_id != rule.account_id {
        return Assessment::new(
            rule,
            vec![Reason::new(
                ReasonCode::AccountMismatch,
                "额度观测与所选账号不一致，暂不使用此账号",
            )],
        );
    }
    let Some(observed_at_ms) = quota.observed_at_ms else {
        return Assessment::new(
            rule,
            vec![Reason::new(
                ReasonCode::ObservationMissing,
                "缺少额度观测时间，暂不使用此账号",
            )],
        );
    };
    if observed_at_ms > now_ms {
        return Assessment::new(
            rule,
            vec![Reason::new(
                ReasonCode::ObservationInFuture,
                "额度观测时间晚于当前时间，暂不使用此账号",
            )],
        );
    }
    let max_age_ms = max_age_seconds
        .checked_mul(1_000)
        .and_then(|milliseconds| i64::try_from(milliseconds).ok());
    let age_ms = now_ms.checked_sub(observed_at_ms);
    if !matches!((age_ms, max_age_ms), (Some(age), Some(maximum)) if maximum > 0 && age <= maximum)
    {
        return Assessment::new(
            rule,
            vec![Reason::new(
                ReasonCode::ObservationStale,
                "额度观测已过期，暂不使用此账号",
            )],
        );
    }

    let mut reasons = Vec::new();
    for (seconds, limit) in [
        (FIVE_HOUR_SECONDS, rule.five_hour_limit_percent),
        (WEEK_SECONDS, rule.weekly_limit_percent),
    ] {
        let Some(limit) = limit else { continue };
        // primary/secondary 是协议槽位，具名桶也不能代表账号通用额度
        let mut found = false;
        for window in quota.windows.iter().filter(|window| {
            window
                .limit_id
                .as_deref()
                .is_some_and(|id| id.trim().eq_ignore_ascii_case("codex"))
                && window.window_seconds == Some(seconds)
        }) {
            found = true;
            if let Some(reason) = assess_window(window, now_ms, limit) {
                reasons.push(reason);
            }
        }
        if !found {
            reasons.push(Reason {
                window_seconds: Some(seconds),
                limit_percent: Some(limit),
                ..Reason::new(
                    ReasonCode::WindowMissing,
                    "缺少已启用保护的额度窗口，暂不使用此账号",
                )
            });
        }
    }
    Assessment::new(rule, reasons)
}

/// 上游百分比窗口与本地已记录费用分别判断。
/// 百分比保护全部关闭时，不要求宿主必须提供 quota_details。
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
            "今日费用观测不完整或含未知费用，暂不使用此账号",
        ),
        DailyStatus::Stale => (
            ReasonCode::DailyObservationStale,
            "今日费用观测已过期，暂不使用此账号",
        ),
    };
    assessment.excluded = true;
    assessment.reasons.push(Reason {
        window_seconds: Some(86_400),
        reset_at_ms: Some(usage.reset_at_ms),
        ..Reason::new(code, message)
    });
}

fn assess_window(window: &QuotaWindowDetails, now_ms: i64, limit: f64) -> Option<Reason> {
    let reason = |code, message| Some(Reason::for_window(code, message, window, limit));
    let Some(used) = window
        .used_percent
        .filter(|used| used.is_finite() && (0.0..=100.0).contains(used))
    else {
        return reason(
            ReasonCode::UsageUnknown,
            "额度窗口缺少可靠的使用率，暂不使用此账号",
        );
    };
    let Some(reset_at_ms) = window.reset_at_ms else {
        return reason(
            ReasonCode::ResetUnknown,
            "额度窗口缺少重置时间，暂不使用此账号",
        );
    };
    // 旧窗口跨过重置时间不代表已经恢复，需由宿主提供新样本
    if reset_at_ms <= now_ms {
        return reason(
            ReasonCode::WindowExpired,
            "额度窗口已到重置时间，等待新的额度观测",
        );
    }
    if window.limit_reached {
        return reason(ReasonCode::LimitReached, "额度窗口已触顶");
    }
    if used >= limit {
        return reason(ReasonCode::LimitReached, "额度使用率已达到保护上限");
    }
    None
}
