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
    pub missing_state_policy: MissingTurnStatePolicy,
    pub target_length: usize,
    pub ttl_seconds: u64,
    pub refresh_after_seconds: u64,
    pub retry_seconds: u64,
    pub jitter_seconds: u64,
    pub budget: u32,
    pub idle_seconds: u64,
    pub timezone: chrono_tz::Tz,
    pub originator: String,
    pub client_version: String,
    pub user_agent: String,
    pub include_account_proxy: bool,
    pub include_direct: bool,
    pub proxy_ids: Vec<String>,
    pub stop_strategy: TurnStateStopStrategy,
    pub feishu_webhook_url: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingTurnStatePolicy {
    #[default]
    Allow,
    Pause,
}

/// 调度只携带有效期，不传播令牌正文；每次选号仍按当前时间判断。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TurnStateAvailability {
    #[default]
    Optional,
    Required {
        expires_at: Option<std::time::SystemTime>,
    },
}

impl TurnStateAvailability {
    pub fn allows(self, now: std::time::SystemTime) -> bool {
        match self {
            Self::Optional => true,
            Self::Required { expires_at } => expires_at.is_some_and(|expiry| now < expiry),
        }
    }
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
            missing_state_policy: MissingTurnStatePolicy::Allow,
            target_length: 292,
            // 2026-09-22 凌晨起，上游对 292 的接受窗口大约只有 240 秒。
            ttl_seconds: 240,
            refresh_after_seconds: 120,
            retry_seconds: 30,
            jitter_seconds: 15,
            budget: 40,
            idle_seconds: 300,
            timezone: chrono_tz::UTC,
            originator: "codex-tui".to_owned(),
            // 探测固定使用已发布的 CLI 版本，不继承 Desktop 内嵌的预发布 Core。
            client_version: "0.154.0".to_owned(),
            user_agent: String::new(),
            include_account_proxy: true,
            include_direct: false,
            proxy_ids: Vec::new(),
            stop_strategy: TurnStateStopStrategy::Headers,
            feishu_webhook_url: String::new(),
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
            && self.client_version.len() <= 64
            && semver::Version::parse(&self.client_version)
                .is_ok_and(|version| version.pre.is_empty() && version.build.is_empty())
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
            && (self.feishu_webhook_url.is_empty()
                || valid_feishu_webhook(&self.feishu_webhook_url))
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn valid_feishu_webhook(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    value.len() <= 512
        && value.trim() == value
        && url.scheme() == "https"
        && matches!(
            url.host_str(),
            Some("open.feishu.cn" | "open.larksuite.com")
        )
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url
            .path()
            .strip_prefix("/open-apis/bot/v2/hook/")
            .is_some_and(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStateObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_id: Option<String>,
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
    /// 上游在同一响应声明的实际模型；与令牌长度一样是观测事实，不代表生效模型。
    #[serde(default)]
    pub reported_model: Option<String>,
    /// 有效信封正文随观测行持久化；状态查询剥离正文，操作使用观测 ID 精确定位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// 状态查询在剥离正文后回填的标记，不落进事件 detail。
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_token: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunt_attempts: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunt_seconds: Option<u64>,
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

impl std::fmt::Debug for TurnStateObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnStateObservation")
            .field("observation_id", &self.observation_id)
            .field("source", &self.source)
            .field("outcome", &self.outcome)
            .field("token_length", &self.token_length)
            .field("issued_at", &self.issued_at)
            .finish_non_exhaustive()
    }
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

/// 通知从当前有效票按需组装，持久化通知任务不复制令牌正文。
pub struct TurnStateNotification {
    pub account_id: String,
    pub account_name: String,
    pub model: String,
    pub webhook_url: String,
    pub token: TurnStateToken,
    pub installation: TurnStateInstallation,
    pub previous_installed_at: Option<i64>,
    pub manual: bool,
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

impl TurnStateBucket {
    /// 已安装正文才是票，候选及账号级通用覆盖不能满足模型桶的要求。
    pub fn installed_token(&self, now: i64) -> Option<&TurnStateToken> {
        self.current.as_ref().filter(|token| {
            Some(token.issued_at) == self.current_issued_at
                && token.value.len() == self.config.target_length
                && token.is_fresh(now, self.config.ttl_seconds)
                && TurnStateToken::parse(&token.value)
                    .is_some_and(|parsed| parsed.issued_at == token.issued_at)
        })
    }

    pub fn matches_account(&self, account: &super::ProviderAccount, model: &str) -> bool {
        self.account_id == account.id().as_str()
            && self.model == model
            && self.upstream_account_id.as_deref() == account.upstream_account_id()
            && self.upstream_user_id.as_deref() == account.upstream_user_id()
            && account.authentication_kind() == "oauth"
    }

    pub fn scheduling_availability(
        &self,
        account: &super::ProviderAccount,
        model: &str,
        now: i64,
    ) -> TurnStateAvailability {
        if self.config.missing_state_policy == MissingTurnStatePolicy::Allow {
            return TurnStateAvailability::Optional;
        }
        let expires_at = self
            .installed_token(now)
            .filter(|_| self.matches_account(account, model))
            .and_then(|token| {
                std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(
                    token.issued_at as u64 + self.config.ttl_seconds,
                ))
            });
        TurnStateAvailability::Required { expires_at }
    }

    pub fn manages_injection(&self) -> bool {
        self.config.enabled
            || self.manual_override
            || self.current_issued_at.is_some()
            || self.config.missing_state_policy == MissingTurnStatePolicy::Pause
    }
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
    pub has_installed_state: bool,
    pub business_status: TurnStateBusinessStatus,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStateBusinessStatus {
    Ready,
    ManualDisabled,
    WaitingForState,
    ModelDenied,
    QuotaExhausted,
    RateLimited,
    AccountError,
}
