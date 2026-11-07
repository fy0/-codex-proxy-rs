//! 基于 Apache-2.0 的 cpr-quota-guard v0.2.3 配额保护逻辑；
//! 五小时、周窗口与 UTC 每日费用仅在已知超限时拦截，未知状态保留提示。
mod config;
mod daily;
mod host_calls;
mod policy;
mod runtime;
pub use config::{AccountRule, QuotaGuardConfig};
pub use daily::{DailyUsage, daily_usage};
pub(crate) use host_calls::limited as limit_host_call;
pub use policy::{Assessment, add_daily, assess, unavailable};
pub use runtime::{QuotaGuard, protected_ids, refresh_daily_overview};
