//! 基于 Apache-2.0 的 cpr-quota-guard v0.2.3 配额保护逻辑；
//! 保留原有五小时与周窗口的证据检查规则，不自行推断额度恢复。
mod config;
mod policy;
mod runtime;
pub use config::{AccountRule, QuotaGuardConfig};
pub use policy::{Assessment, assess, unavailable};
pub use runtime::{QuotaGuard, protected_ids};
