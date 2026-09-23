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
    pub cookie_lock_enabled: bool,
    pub cookie_refresh_before_seconds: u64,
    pub cookie_gateway_ids: String,
    pub missing_state_policy: MissingTurnStatePolicy,
    /// 暂停业务调度时，已附着的实际模型突然变化则作废当前票。
    pub detect_actual_model: bool,
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
            cookie_lock_enabled: false,
            cookie_refresh_before_seconds: 300,
            cookie_gateway_ids: String::new(),
            missing_state_policy: MissingTurnStatePolicy::Allow,
            detect_actual_model: false,
            target_length: 292,
            // 同一张 292 在 191.7 秒时仍被接受，267.1 秒时上游已重新签发。240 秒落在这两次实测之间。
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
        (30..=1800).contains(&self.cookie_refresh_before_seconds)
            && (76..=4096).contains(&self.target_length)
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
            && self.cookie_gateway_ids_valid()
            && (self.feishu_webhook_url.is_empty()
                || valid_feishu_webhook(&self.feishu_webhook_url))
    }

    pub fn cookie_gateway_ids_valid(&self) -> bool {
        self.cookie_gateway_ids.len() <= 256
            && !self.cookie_gateway_ids.chars().any(char::is_control)
            && (self.cookie_gateway_ids.is_empty()
                || (self.cookie_gateway_ids.split('|').count() <= 32
                    && self.cookie_gateway_ids.split('|').all(|id| {
                        !id.is_empty()
                            && id.len() <= 10
                            && id.bytes().all(|byte| byte.is_ascii_digit())
                    })))
    }

    pub fn allows_cookie_gateway(&self, pod: &str) -> bool {
        if self.cookie_gateway_ids.is_empty() {
            return false;
        }
        let Some(gateway_id) = super::RoutingCookie::gateway_id_from_pod(pod) else {
            return false;
        };
        self.cookie_gateway_ids
            .split('|')
            .any(|id| id == gateway_id)
    }

    /// 实际模型检测只在暂停业务调度时把脱离的票作废，避免继续注入失效票。
    pub fn revokes_ticket_when_model_detaches(&self) -> bool {
        self.detect_actual_model && self.missing_state_policy == MissingTurnStatePolicy::Pause
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
    #[serde(default)]
    pub oailb_host: Option<String>,
    #[serde(default)]
    pub cookie_expires_at: Option<i64>,
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
    /// 题库问题的回答正文（有界截断）；仅主动探测产生，业务观测不保存回答。
    #[serde(default)]
    pub answer: Option<String>,
    /// 回答是否命中题库声明的期望片段；未读正文或题目未声明期望时为空。
    #[serde(default)]
    pub answer_match: Option<bool>,
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
    pub routing_cookies: Vec<super::RoutingCookie>,
    pub cookie_override_pod: Option<String>,
    /// 固定对应的 Cookie 签发时间；与 pod 一起唯一确定池内一条 Cookie。
    pub cookie_override_issued_at: Option<i64>,
    pub current: Option<TurnStateToken>,
    pub current_issued_at: Option<i64>,
    pub current_length: Option<usize>,
    pub candidate: Option<TurnStateToken>,
    pub hunt_attempts: u64,
    pub next_probe_at: Option<i64>,
    pub manual_probe_requested_at: Option<i64>,
    pub manual_override: bool,
    /// 本张已安装票最近一次附着的上游实际模型。空表示还没有观测到。
    pub attached_model: Option<String>,
}

impl TurnStateBucket {
    /// 固定精确到具体一条 Cookie（pod + 签发时间）：续约换新值后旧固定随之失效。
    pub fn cookie_override_matches(&self, cookie: &super::RoutingCookie) -> bool {
        self.cookie_override_pod.as_deref() == Some(cookie.pod.as_str())
            && self.cookie_override_issued_at == Some(cookie.issued_at)
    }

    pub fn cookie_is_selectable(&self, cookie: &super::RoutingCookie) -> bool {
        self.config.allows_cookie_gateway(&cookie.pod) || self.cookie_override_matches(cookie)
    }

    pub fn routing_cookie(&self, now: i64) -> Option<&super::RoutingCookie> {
        self.routing_cookies
            .iter()
            .filter(|cookie| {
                cookie.is_usable(&self.model, now)
                    && self.cookie_is_selectable(cookie)
                    && (self.cookie_override_pod.is_none() || self.cookie_override_matches(cookie))
            })
            .max_by_key(|cookie| (cookie.expires_at, cookie.observed_at))
    }

    pub fn renewal_cookie(&self, now: i64) -> Option<&super::RoutingCookie> {
        self.routing_cookies
            .iter()
            .filter(|cookie| {
                cookie.is_usable(&self.model, now)
                    && self.cookie_is_selectable(cookie)
                    && (self.cookie_override_pod.is_none() || self.cookie_override_matches(cookie))
                    && cookie.expires_at <= now + self.config.cookie_refresh_before_seconds as i64
            })
            .min_by_key(|cookie| cookie.expires_at)
    }

    pub fn cookie_renewal_due(&self, now: i64) -> bool {
        self.renewal_cookie(now).is_some()
    }

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
        if self.config.cookie_lock_enabled {
            let expires_at = self
                .routing_cookie(now)
                .filter(|_| self.matches_account(account, model))
                .and_then(|cookie| {
                    std::time::UNIX_EPOCH
                        .checked_add(std::time::Duration::from_secs(cookie.expires_at as u64))
                });
            return TurnStateAvailability::Required { expires_at };
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
    pub routing_cookie: Option<super::RoutingCookieStatus>,
    pub cookie_pool: Vec<super::RoutingCookieStatus>,
    pub cookie_override_pod: Option<String>,
    /// 固定绑定的 Cookie 签发时间，随 pod 一起对外展示。
    pub cookie_override_issued_at: Option<i64>,
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
