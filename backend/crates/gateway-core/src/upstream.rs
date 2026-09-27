//! 上游调用跨 Engine、Event、Error 与 Provider 共享的中立边界事实。

use std::fmt;

use crate::validation::{IdentifierError, validate_text};

/// 上游是否可能已经收到业务 payload。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpstreamSendState {
    NotSent,
    Sent,
    Ambiguous,
}

impl UpstreamSendState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotSent => "not_sent",
            Self::Sent => "sent",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// 模型级上行通道。
///
/// 普通映射只改上游模型名；专用通道同时决定目标端点与协议翻译，必须由设置
/// 显式指定的公开模型名命中，不能从解析后的上游模型名反推。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UpstreamChannel {
    /// Provider 默认传输。
    #[default]
    Default,
    /// Basis Points（bps.openai.com）白名单 schema 通道。
    BasisPoints,
}

impl UpstreamChannel {
    #[must_use]
    pub const fn is_default(self) -> bool {
        matches!(self, Self::Default)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::BasisPoints => "basispoints",
        }
    }

    /// `as_str` 的逆映射；存储层把通道名编入资源键时需要它还原。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "basispoints" => Some(Self::BasisPoints),
            _ => None,
        }
    }
}

/// 上游 transport 注册名称。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UpstreamTransport(String);

impl UpstreamTransport {
    /// 校验 transport 名称。
    ///
    /// # Errors
    ///
    /// 名称无效时返回错误。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 64, true, None)?;
        Ok(Self(value))
    }

    /// 返回 transport 名称。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Adapter 已明确分类为非 bearer 的不透明上游值。
///
/// Core 不解释或校验内容，只通过自定义 [`Debug`] 避免它意外进入日志。协议 adapter
/// 可在原客户端响应中读取原值。
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OpaqueUpstreamValue(String);

impl OpaqueUpstreamValue {
    /// 原样保存上游值。
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 返回已经由 adapter 安全分类的原值。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OpaqueUpstreamValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueUpstreamValue(<redacted-from-Debug>)")
    }
}
