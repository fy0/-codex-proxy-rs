//! 按账号和模型隔离的路由令牌合同及不透明信封校验。

use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnStateConfig {
    pub enabled: bool,
    pub target_length: usize,
    pub ttl_seconds: u64,
    pub refresh_after_seconds: u64,
    pub retry_seconds: u64,
    pub jitter_seconds: u64,
    pub budget: u32,
    pub idle_seconds: u64,
    pub timezone: chrono_tz::Tz,
    pub originator: String,
    pub user_agent: String,
    pub include_account_proxy: bool,
    pub include_direct: bool,
    pub proxy_ids: Vec<String>,
    pub stop_strategy: TurnStateStopStrategy,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStateStopStrategy {
    #[default]
    Headers,
    FirstOutput,
    Mixed,
}

impl Default for TurnStateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_length: 292,
            ttl_seconds: 3600,
            refresh_after_seconds: 2100,
            retry_seconds: 30,
            jitter_seconds: 15,
            budget: 40,
            idle_seconds: 300,
            timezone: chrono_tz::UTC,
            originator: "codex-tui".to_owned(),
            user_agent: String::new(),
            include_account_proxy: true,
            include_direct: false,
            proxy_ids: Vec::new(),
            stop_strategy: TurnStateStopStrategy::Headers,
        }
    }
}

impl TurnStateConfig {
    pub fn is_valid(&self) -> bool {
        (76..=4096).contains(&self.target_length)
            && (60..=3600).contains(&self.ttl_seconds)
            && (30..self.ttl_seconds).contains(&self.refresh_after_seconds)
            && (1..=3600).contains(&self.retry_seconds)
            && self.jitter_seconds <= 3600
            && (1..=100).contains(&self.budget)
            && (10..=86400).contains(&self.idle_seconds)
            && !self.originator.trim().is_empty()
            && self.originator.len() <= 128
            && self
                .originator
                .bytes()
                .all(|byte| (0x20..=0x7e).contains(&byte))
            && self.user_agent.len() <= 1024
            && (self.user_agent.is_empty() || !self.user_agent.trim().is_empty())
            && self
                .user_agent
                .bytes()
                .all(|byte| (0x20..=0x7e).contains(&byte))
            && (self.include_account_proxy || self.include_direct || !self.proxy_ids.is_empty())
            && self.proxy_ids.len() <= 32
            && self.proxy_ids.iter().all(|id| {
                id.starts_with("proxy_") && id.len() <= 128 && !id.chars().any(char::is_control)
            })
    }
}

#[derive(Clone)]
pub struct TurnStateToken {
    pub value: String,
    pub issued_at: i64,
}

impl std::fmt::Debug for TurnStateToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnStateToken")
            .field("length", &self.value.len())
            .field("issued_at", &self.issued_at)
            .finish()
    }
}

impl TurnStateToken {
    /// 仅解析上游不透明信封，不声称验证其签名或服务端接受期限。
    pub fn parse(value: &str) -> Option<Self> {
        if value.len() > 4096 || value.len() < 76 {
            return None;
        }
        let bytes = URL_SAFE
            .decode(value)
            .or_else(|_| URL_SAFE_NO_PAD.decode(value))
            .ok()?;
        if bytes.len() < 57 || bytes[0] != 0x80 {
            return None;
        }
        let issued_at = i64::try_from(u64::from_be_bytes(bytes[1..9].try_into().ok()?)).ok()?;
        Some(Self {
            value: value.to_owned(),
            issued_at,
        })
    }

    pub fn is_fresh(&self, now: i64, ttl_seconds: u64) -> bool {
        self.issued_at > 0
            && now
                .checked_sub(self.issued_at)
                .is_some_and(|age| age >= 0 && (age as u64) < ttl_seconds)
    }

    pub fn is_newer_than(&self, issued_at: Option<i64>) -> bool {
        issued_at.is_none_or(|current| self.issued_at > current)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStateObservation {
    pub account_id: String,
    #[serde(skip)]
    pub upstream_account_id: Option<String>,
    #[serde(skip)]
    pub upstream_user_id: Option<String>,
    pub model: String,
    pub observed_at: i64,
    #[serde(default)]
    pub started_at: Option<i64>,
    pub source: String,
    #[serde(default)]
    pub request_state_source: Option<String>,
    #[serde(default)]
    pub response_source: Option<String>,
    #[serde(default)]
    pub probe_trigger: Option<String>,
    pub outcome: String,
    pub http_status: Option<u16>,
    pub token_length: Option<usize>,
    pub issued_at: Option<i64>,
    pub egress: String,
    pub shape: Option<String>,
    pub effort: Option<String>,
    pub elapsed_ms: u64,
    pub probe_id: Option<String>,
    #[serde(default)]
    pub stop_mode: Option<String>,
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStateInstallation {
    pub installed_at: i64,
    pub issued_at: i64,
    pub token_length: usize,
    pub source: String,
    pub acquired_at: i64,
    pub attempts: u64,
    pub hunt_seconds: u64,
}

#[derive(Clone)]
pub struct TurnStateBucket {
    pub account_id: String,
    pub upstream_account_id: Option<String>,
    pub upstream_user_id: Option<String>,
    pub model: String,
    pub config: TurnStateConfig,
    pub current: Option<TurnStateToken>,
    pub current_issued_at: Option<i64>,
    pub current_length: Option<usize>,
    pub candidate: Option<TurnStateToken>,
    pub hunt_attempts: u64,
    pub next_probe_at: Option<i64>,
    pub manual_probe_requested_at: Option<i64>,
    pub manual_override: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStateStatus {
    pub account_id: String,
    pub account_name: String,
    pub account_email: Option<String>,
    pub model: String,
    pub config: TurnStateConfig,
    pub token_length: Option<usize>,
    pub issued_at: Option<i64>,
    pub age_seconds: Option<i64>,
    pub active: bool,
    pub account_enabled: bool,
    pub hunt_attempts: u64,
    pub next_probe_at: Option<i64>,
    pub manual_probe_requested_at: Option<i64>,
    pub manual_override: bool,
    pub candidate_issued_at: Option<i64>,
    pub candidate_length: Option<usize>,
    pub observations: Vec<TurnStateObservation>,
    pub installations: Vec<TurnStateInstallation>,
}
