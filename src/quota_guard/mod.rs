//! 基于 Apache-2.0 的 cpr-quota-guard v0.2.3 配额保护逻辑；
//! 保留原有五小时与周窗口的证据检查规则，不自行推断额度恢复。
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
