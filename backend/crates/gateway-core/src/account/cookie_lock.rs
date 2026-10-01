//! 路由 Cookie 的有限寿命与跨账号共享合同；不包含账号认证 Cookie。

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

/// 完整路由 pair：`__oailb`/`__oai_lb` JWT 加上同一响应的 `__cflb`。任何一半缺失都
/// 不能单独回放，旧格式记录（没有 cflb 字段）反序列化后自动判为不完整。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingCookie {
    pub origin: String,
    pub pod: String,
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub cflb_name: String,
    #[serde(default)]
    pub cflb_value: String,
    /// `__cflb` 的 HTTP 属性到期；有效到期取 JWT exp 与该属性的较小者。
    #[serde(default)]
    pub cflb_expires_at: Option<i64>,
    /// `__oailb` JWT 自己声明的到期；`expires_at` 可能已被 `__cflb` 属性收窄。
    #[serde(default)]
    pub oailb_expires_at: Option<i64>,
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
            .field("has_pair", &self.has_pair())
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

    /// pod 按 `unified-N` 归一化；非法 pod 返回 `None`。
    pub fn gateway_label_from_pod(pod: &str) -> Option<String> {
        Self::gateway_id_from_pod(pod).map(|id| format!("unified-{id}"))
    }

    /// 端点/响应里 `unified-N`（大小写、下划线写法归一）或纯数字归一为统一标签。
    pub fn normalize_gateway_label(value: &str) -> Option<String> {
        let value = value.trim().to_ascii_lowercase();
        let digits = value
            .strip_prefix("unified-")
            .or_else(|| value.strip_prefix("unified_"))
            .unwrap_or(&value);
        (!digits.is_empty()
            && digits.len() <= 10
            && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| format!("unified-{digits}"))
    }

    pub fn gateway_id(&self) -> &str {
        // `parse` validates the host, so this is always present for persisted cookies.
        Self::gateway_id_from_pod(&self.pod).unwrap_or_default()
    }

    /// pod 的 `unified-N` 标签；解析过的 Cookie 必然有值。
    pub fn gateway_label(&self) -> String {
        Self::gateway_label_from_pod(&self.pod).unwrap_or_default()
    }

    /// 只读取上游签发的 JWT 元数据，不验证签名，也不生成或修改 JWT。
    ///
    /// 结果是不含 `__cflb` 的半成品，必须再经 `with_cflb` 配对后才能回放。
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
        if issued_at <= 0 || issued_at > now || expires_at <= now || expires_at <= issued_at {
            return None;
        }
        Some(Self {
            origin: origin.to_owned(),
            pod: pod.to_owned(),
            name: name.to_owned(),
            value: value.to_owned(),
            cflb_name: String::new(),
            cflb_value: String::new(),
            cflb_expires_at: None,
            // JWT exp 是网关真正执行的死线；HTTP Max-Age/Expires 只是声明，不缩短它。
            oailb_expires_at: Some(expires_at),
            issued_at,
            expires_at,
            observed_at: 0,
            reported_model: String::new(),
        })
    }

    /// 给已解析的 `__oailb` 配上 `__cflb`；半截 pair 或非法值一律不制造。
    ///
    /// `cflb_expires_at` 比 JWT 到期早时收窄有效到期；更长的 `__cflb` 不延长 JWT 死线。
    pub fn with_cflb(
        mut self,
        name: &str,
        value: &str,
        cflb_expires_at: Option<i64>,
    ) -> Option<Self> {
        if name != "__cflb"
            || value.is_empty()
            || value.len() > 4096
            || value.contains(';')
            || value.contains(',')
            || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return None;
        }
        if let Some(expiry) = cflb_expires_at {
            self.expires_at = self.expires_at.min(expiry);
        }
        self.cflb_expires_at = cflb_expires_at;
        self.cflb_name = name.to_owned();
        self.cflb_value = value.to_owned();
        Some(self)
    }

    /// `__cflb` + `__oailb` 两个半边都齐才算完整 pair。
    pub fn has_pair(&self) -> bool {
        !self.cflb_name.is_empty() && !self.cflb_value.is_empty()
    }

    /// 精确 pair 同一性：两半的名字与值全部相等。人工固定值没有 origin，
    /// 与带 origin 的池记录比对时 origin 为空的一侧不参与判定。
    pub fn same_pair(&self, other: &Self) -> bool {
        self.name == other.name
            && self.value == other.value
            && self.cflb_name == other.cflb_name
            && self.cflb_value == other.cflb_value
            && (self.origin.is_empty() || other.origin.is_empty() || self.origin == other.origin)
    }

    /// 路由有效性：完整 pair、合法 pod、未到期。不检查模型——种子复用跨模型。
    pub fn is_route_valid(&self, now: i64) -> bool {
        self.has_pair()
            && Self::gateway_id_from_pod(&self.pod).is_some()
            && self.issued_at <= now
            && now < self.expires_at
    }

    /// 可回放的完整 pair：路由有效且声明模型与请求一致。
    pub fn is_usable(&self, model: &str, now: i64) -> bool {
        self.is_route_valid(now) && self.reported_model.eq_ignore_ascii_case(model)
    }

    pub fn header(&self) -> String {
        if self.has_pair() {
            format!(
                "{}={}; {}={}",
                self.cflb_name, self.cflb_value, self.name, self.value
            )
        } else {
            format!("{}={}", self.name, self.value)
        }
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
