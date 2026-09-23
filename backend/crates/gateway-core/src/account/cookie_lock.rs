//! 路由 Cookie 的有限寿命与跨账号共享合同；不包含账号认证 Cookie。

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct RoutingCookie {
    pub origin: String,
    pub pod: String,
    pub name: String,
    pub value: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub observed_at: i64,
    pub reported_model: String,
}

impl std::fmt::Debug for RoutingCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingCookie")
            .field("pod", &self.pod)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl RoutingCookie {
    pub fn gateway_id_from_pod(pod: &str) -> Option<&str> {
        pod.strip_prefix("chat.gateway.unified-")?
            .strip_suffix(".api.openai.com")
            .filter(|number| {
                !number.is_empty()
                    && number.len() <= 10
                    && number.bytes().all(|byte| byte.is_ascii_digit())
            })
    }

    pub fn gateway_id(&self) -> &str {
        // `parse` validates the host, so this is always present for persisted cookies.
        Self::gateway_id_from_pod(&self.pod).unwrap_or_default()
    }

    /// 只读取上游签发的 JWT 元数据，不验证签名，也不生成或修改 JWT。
    pub fn parse(origin: &str, name: &str, value: &str, now: i64) -> Option<Self> {
        if !matches!(name, "__oailb" | "__oai_lb") || value.len() > 4096 {
            return None;
        }
        let mut parts = value.split('.');
        let parts = [parts.next()?, parts.next()?, parts.next()?];
        if value.bytes().filter(|byte| *byte == b'.').count() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
        {
            return None;
        }
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).ok()?).ok()?;
        if header.get("alg")?.as_str()? != "ES256" {
            return None;
        }
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).ok()?).ok()?;
        let pod = claims.get("host")?.as_str()?;
        Self::gateway_id_from_pod(pod)?;
        let issued_at = claims.get("iat")?.as_i64()?;
        let expires_at = claims.get("exp")?.as_i64()?;
        if issued_at <= 0
            || issued_at > now
            || expires_at <= now
            || expires_at <= issued_at
            || expires_at.checked_sub(issued_at)? > 3600
        {
            return None;
        }
        Some(Self {
            origin: origin.to_owned(),
            pod: pod.to_owned(),
            name: name.to_owned(),
            value: value.to_owned(),
            issued_at,
            expires_at,
            observed_at: 0,
            reported_model: String::new(),
        })
    }

    pub fn is_usable(&self, model: &str, now: i64) -> bool {
        self.issued_at <= now
            && now < self.expires_at
            && self.reported_model.eq_ignore_ascii_case(model)
    }

    pub fn header(&self) -> String {
        format!("{}={}", self.name, self.value)
    }

    pub fn status(&self) -> RoutingCookieStatus {
        RoutingCookieStatus {
            gateway_id: self.gateway_id().to_owned(),
            pod: self.pod.clone(),
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            observed_at: self.observed_at,
            reported_model: self.reported_model.clone(),
            allowed: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingCookieStatus {
    pub gateway_id: String,
    pub pod: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub observed_at: i64,
    pub reported_model: String,
    pub allowed: bool,
}

/// observed_at 是请求开始时间（毫秒），防止晚到的旧响应覆盖更新的降级事实。
pub struct RoutingCookieObservation {
    pub origin: String,
    pub observed_at: i64,
    pub sent: Option<RoutingCookie>,
    pub received: Option<RoutingCookie>,
    pub reported_model: Option<String>,
    pub deleted: bool,
}
