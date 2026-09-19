//! 响应头观测回调；传输层只提供事实，不解释令牌。

use futures::future::BoxFuture;
use std::sync::Arc;

pub(crate) struct TurnStateResponse {
    pub status: Option<u16>,
    pub value: Option<Vec<u8>>,
    /// 上游在同一观测位置声明的实际模型；缺失不代表请求模型生效。
    pub reported_model: Option<String>,
    pub elapsed_ms: u64,
    pub transport_error: bool,
    pub source: &'static str,
}

impl TurnStateResponse {
    pub(super) fn websocket_failure(
        error: &super::websocket::CodexWebSocketExchangeError,
        elapsed_ms: u64,
    ) -> Option<Self> {
        use super::websocket::CodexWebSocketExchangeError as Error;
        match error {
            Error::ConnectionObserved { source, .. }
            | Error::PostSendAmbiguous {
                source: Some(source),
                ..
            }
            | Error::ReusedConnectionDiedBeforeFirstEvent {
                source: Some(source),
                ..
            } => Self::websocket_failure(source, elapsed_ms),
            Error::Connect(tungstenite::Error::Http(response))
            | Error::Transport(tungstenite::Error::Http(response)) => Some(Self {
                status: Some(response.status().as_u16()),
                value: response
                    .headers()
                    .get("x-codex-turn-state")
                    .map(|value| value.as_bytes().to_vec()),
                reported_model: super::response_meta::reported_model(response.headers()),
                elapsed_ms,
                transport_error: false,
                source: "websocket_start",
            }),
            Error::Connect(_)
            | Error::Transport(_)
            | Error::ConnectTimeout { .. }
            | Error::SendTimeout { .. } => Some(Self {
                status: None,
                value: None,
                reported_model: None,
                elapsed_ms,
                transport_error: true,
                source: "websocket_start",
            }),
            // 本地熔断、排队和续接策略不是一次已发出的上游响应。
            _ => None,
        }
    }
}

pub(crate) type TurnStateObserver =
    Arc<dyn Fn(TurnStateResponse) -> BoxFuture<'static, ()> + Send + Sync>;
