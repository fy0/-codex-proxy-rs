//! 账号额度保护配置的解析、默认值与约束

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuotaGuardConfig {
    #[serde(default)]
    pub accounts: Vec<AccountRule>,
    #[serde(default = "default_max_observation_age_seconds")]
    pub max_observation_age_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountRule {
    pub account_id: String,
    #[serde(default = "default_limit_percent")]
    pub five_hour_limit_percent: Option<f64>,
    #[serde(default = "default_limit_percent")]
    pub weekly_limit_percent: Option<f64>,
}

impl QuotaGuardConfig {
    pub fn parse(accounts: Option<&Value>, max_age: Option<&Value>) -> Result<Self, String> {
        let value = serde_json::json!({
            "accounts": accounts.cloned().unwrap_or(Value::Array(Vec::new())),
            "maxObservationAgeSeconds": max_age.cloned().unwrap_or(Value::from(300)),
        });
        Self::from_value(value)
    }

    pub fn from_value(value: Value) -> Result<Self, String> {
        if !value.is_object()
            || value.get("accounts").is_some_and(|accounts| {
                accounts
                    .as_array()
                    .is_none_or(|accounts| accounts.iter().any(|account| !account.is_object()))
            })
        {
            return Err("插件配置格式不合法".to_owned());
        }
        let config: Self =
            serde_json::from_value(value).map_err(|_| "插件配置格式不合法".to_owned())?;
        if !(60..=86_400).contains(&config.max_observation_age_seconds) {
            return Err("额度观测有效期必须在 60 到 86400 秒之间".to_owned());
        }
        if config.accounts.len() > 1_000 {
            return Err("受保护账号不能超过 1000 个".to_owned());
        }
        let mut account_ids = BTreeSet::new();
        for rule in &config.accounts {
            if rule.account_id.is_empty() || rule.account_id.trim() != rule.account_id {
                return Err("账号 ID 不能为空或包含前后空白".to_owned());
            }
            if rule.account_id.chars().count() > 256
                || rule
                    .account_id
                    .contains(['\n', '\r', '\u{2028}', '\u{2029}'])
            {
                return Err("账号 ID 不能超过 256 个字符或包含换行".to_owned());
            }
            if !account_ids.insert(rule.account_id.as_str()) {
                return Err("账号 ID 不能重复".to_owned());
            }
            for limit in [rule.five_hour_limit_percent, rule.weekly_limit_percent]
                .into_iter()
                .flatten()
            {
                if !limit.is_finite() || !(0.0..=100.0).contains(&limit) {
                    return Err("额度使用率上限必须在 0 到 100 之间，或设为 null".to_owned());
                }
            }
        }
        Ok(config)
    }
}

impl AccountRule {
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.five_hour_limit_percent.is_some() || self.weekly_limit_percent.is_some()
    }
}

const fn default_limit_percent() -> Option<f64> {
    Some(95.0)
}

const fn default_max_observation_age_seconds() -> u64 {
    300
}
