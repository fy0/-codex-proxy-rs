//! 探测模板只继承 instructions，身份、环境和正文在每次发送前重新生成。

use std::{io::Read, path::Path};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use gateway_core::account::TurnStateConfig;
use gateway_protocol::openai::events::ResponseModelObservation;
use gateway_protocol::openai::sse::SseEventDecoder;
use reqwest::header::{HeaderMap, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

use crate::credential::{CodexCookiePolicy, RuntimeCodexCookie};

use crate::transport::profile::CodexWireProfileState;

const SHAPES: [(&str, &str); 20] = [
    ("greeting", "Hello. Reply with a short greeting."),
    ("chat", "How is your day? Answer in one sentence."),
    (
        "python",
        "Write a Python expression that adds two integers.",
    ),
    ("rust", "Name the Rust type for a boolean."),
    (
        "javascript",
        "Write a JavaScript expression that doubles 4.",
    ),
    ("sql", "Write a SQL query that selects the number one."),
    (
        "shell",
        "Name the shell command that prints the current directory.",
    ),
    ("math", "What is seven plus five?"),
    ("capital", "What is the capital of France?"),
    ("science", "Name the chemical symbol for oxygen."),
    ("definition", "Define recursion in one short sentence."),
    ("synonym", "Give one synonym for quick."),
    ("opposite", "What is the opposite of north?"),
    ("sort", "Sort these integers: 3, 1, 2."),
    ("json", "Return a JSON object with ready set to true."),
    ("list", "List two primary colors."),
    ("boolean", "Is eight an even number?"),
    ("thanks", "Thank you for your help. Reply briefly."),
    ("naming", "Suggest a short name for a counter variable."),
    ("unit", "How many seconds are in one minute?"),
];

pub(super) fn random_index(count: usize) -> usize {
    // 拒绝采样使 N+1 出口保持均匀，不把取模偏差带入实验分布。
    let bound = count as u64;
    let limit = u64::MAX - u64::MAX % bound;
    loop {
        let mut bytes = [0; 8];
        if getrandom::fill(&mut bytes).is_err() {
            bytes.copy_from_slice(&Uuid::now_v7().as_bytes()[8..]);
        }
        let value = u64::from_be_bytes(bytes);
        if value < limit {
            return (value % bound) as usize;
        }
    }
}

pub(super) fn load_template(path: Option<&Path>) -> Result<String, ()> {
    let Some(path) = path else {
        return Ok("You are a helpful coding assistant. Keep the response brief.".to_owned());
    };
    let file = std::fs::File::open(path).map_err(|_| ())?;
    if file.metadata().map_err(|_| ())?.len() > 1024 * 1024 {
        return Err(());
    }
    let decoder = zstd::stream::read::Decoder::new(file).map_err(|_| ())?;
    let mut bytes = Vec::new();
    decoder
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > 1024 * 1024 {
        return Err(());
    }
    let body: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    body.get("instructions")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or(())
}

/// 有界读取 SSE 的停止结果：reason 维持原语义，reported_model 记录上游在
/// 已读事件里声明的实际模型（头块声明优先于 response.model）。
pub(super) struct ProbeOutput {
    pub reason: &'static str,
    pub reported_model: Option<String>,
}

pub(super) async fn wait_for_output(response: reqwest::Response) -> ProbeOutput {
    let mut stream = response.bytes_stream();
    let mut decoder = SseEventDecoder::default();
    let mut received = 0_usize;
    // 事件头里的声明优先，response.model 兜底，与业务观测的合并规则一致。
    let mut header_model: Option<String> = None;
    let mut body_model = ResponseModelObservation::default();
    let reason = 'read: loop {
        let Some(chunk) = stream.next().await else {
            break 'read "body_ended";
        };
        let Ok(chunk) = chunk else {
            break 'read "body_transport_error";
        };
        received = received.saturating_add(chunk.len());
        if received > 1024 * 1024 {
            break 'read "body_limit";
        }
        for frame in decoder.push_frames(&chunk) {
            for event in frame.events() {
                let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
                    continue;
                };
                let event_type = value.get("type").and_then(Value::as_str);
                if header_model.is_none() {
                    header_model =
                        crate::transport::response_meta::reported_model_from_event(&value)
                            .map(str::to_owned);
                }
                body_model.observe(event_type, &value);
                match event_type {
                    Some(
                        "response.output_text.delta"
                        | "response.refusal.delta"
                        | "response.reasoning_text.delta"
                        | "response.reasoning_summary_text.delta"
                        | "response.audio.delta"
                        | "response.output_audio.delta",
                    ) => break 'read "first_output",
                    Some(
                        "response.completed" | "response.failed" | "response.incomplete" | "error",
                    ) => break 'read "terminal",
                    _ => {}
                }
            }
        }
    };
    ProbeOutput {
        reason,
        reported_model: header_model.or_else(|| body_model.model().map(str::to_owned)),
    }
}

pub(super) struct ProbeRequest {
    pub headers: HeaderMap,
    pub body: Value,
    pub shape: &'static str,
    pub effort: &'static str,
    pub id: String,
}

pub(super) fn request(
    model: &str,
    config: &TurnStateConfig,
    instructions: &str,
    profile: &CodexWireProfileState,
    now: DateTime<Utc>,
) -> ProbeRequest {
    let session = Uuid::now_v7().to_string();
    let thread = Uuid::now_v7().to_string();
    let id = Uuid::now_v7().to_string();
    let window = Uuid::now_v7().to_string();
    let turn = Uuid::now_v7().to_string();
    let context = Uuid::now_v7().to_string();
    let metadata = json!({
        "session_id": session, "thread_id": thread, "window_id": window,
        "turn_id": turn, "root_turn_id": turn,
        "context_window_id": context,
        "turn_started_at_unix_ms": now.timestamp_millis(),
    })
    .to_string();
    let (shape, prompt) = SHAPES[random_index(SHAPES.len())];
    let effort = ["medium", "high", "xhigh"][random_index(3)];
    let profile = profile.snapshot();
    let originator = &config.originator;
    let ua = profile.turn_state_user_agent(config);
    let mut headers = HeaderMap::new();
    for (name, value) in [
        ("session-id", session.as_str()),
        ("thread-id", thread.as_str()),
        ("x-client-request-id", id.as_str()),
        ("x-codex-window-id", window.as_str()),
        ("x-codex-turn-metadata", metadata.as_str()),
        ("originator", originator.as_str()),
        ("version", config.client_version.as_str()),
        ("user-agent", ua.as_str()),
        ("accept", "text/event-stream"),
        ("content-type", "application/json"),
    ] {
        if let Ok(value) = HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    }
    let timezone = config.timezone;
    let environment = format!(
        "<environment_context>\n  <cwd>/workspace</cwd>\n  <shell>bash</shell>\n  <current_date>{}</current_date>\n  <timezone>{timezone}</timezone>\n</environment_context>",
        now.with_timezone(&timezone).format("%Y-%m-%d")
    );
    ProbeRequest {
        headers,
        shape,
        effort,
        id: id.clone(),
        body: json!({
            "model": model, "instructions": instructions, "stream": true, "store": false,
            "tools": [], "parallel_tool_calls": true,
            "reasoning": {"effort": effort, "summary": "auto"},
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": session,
            "client_metadata": {
                "session_id": session, "thread_id": thread, "turn_id": turn,
                "root_turn_id": turn, "window_id": window, "context_window_id": context,
                "session-id": session, "thread-id": thread, "x-client-request-id": id,
                "x-codex-window-id": window, "x-codex-turn-metadata": metadata,
            },
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": environment}]},
                {"role": "user", "content": [{"type": "input_text", "text": format!("{}: {prompt}", random_index(1_000_000_000))}]},
            ],
        }),
    }
}

pub(super) enum SessionCookie {
    Ready(SecretString),
    Optional,
    Required,
    Invalid,
}

/// 官方主机上的 292 必须和仍有效的账号 Cookie 一起发送才会被持续接受。
/// 本机联调地址不在 Cookie 允许域内，没有 Cookie 时仍按原探测继续。
pub(super) fn session_cookie(
    endpoint: &str,
    cookies: &[RuntimeCodexCookie],
    now: DateTime<Utc>,
) -> SessionCookie {
    let Ok(policy) = CodexCookiePolicy::official() else {
        return SessionCookie::Invalid;
    };
    let Ok(endpoint) = Url::parse(endpoint) else {
        return SessionCookie::Invalid;
    };
    let header = super::super::observation::build_cookie_header(cookies.iter().filter(|cookie| {
        cookie.expires_at.is_none_or(|expires| expires > now)
            && policy.may_replay(
                &endpoint,
                &cookie.domain,
                &cookie.path,
                cookie.host_only,
                cookie.secure,
            )
    }));
    let official = policy.official_https_host(&endpoint);
    match header {
        Ok(Some(header)) if HeaderValue::from_str(header.expose_secret()).is_ok() => {
            SessionCookie::Ready(header)
        }
        Ok(Some(_)) | Err(_) => SessionCookie::Invalid,
        Ok(None) if official => SessionCookie::Required,
        Ok(None) => SessionCookie::Optional,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(
        name: &str,
        value: &str,
        domain: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> RuntimeCodexCookie {
        RuntimeCodexCookie {
            name: name.to_owned(),
            value: SecretString::from(value),
            domain: domain.to_owned(),
            path: "/".to_owned(),
            host_only: false,
            secure: true,
            expires_at,
        }
    }

    #[test]
    fn official_probe_requires_a_replayable_cookie_and_loopback_does_not() {
        let now = Utc::now();
        let official = "https://chatgpt.com/backend-api/codex/responses";
        let loopback = "http://127.0.0.1:9/codex/responses";
        assert!(matches!(
            session_cookie(official, &[], now),
            SessionCookie::Required
        ));
        assert!(matches!(
            session_cookie(loopback, &[], now),
            SessionCookie::Optional
        ));
        let expired = cookie(
            "oai-did",
            "expired-cookie",
            "chatgpt.com",
            Some(now - chrono::Duration::seconds(1)),
        );
        assert!(matches!(
            session_cookie(official, &[expired], now),
            SessionCookie::Required
        ));
        let foreign = cookie("oai-did", "other-site", "example.com", None);
        assert!(matches!(
            session_cookie(official, &[foreign], now),
            SessionCookie::Required
        ));
        let session = cookie(
            "__Secure-next-auth.session-token",
            "session-value",
            "chatgpt.com",
            None,
        );
        let device = cookie("oai-did", "device-value", "chatgpt.com", None);
        match session_cookie(official, &[session, device], now) {
            SessionCookie::Ready(header) => {
                assert_eq!(
                    header.expose_secret(),
                    "__Secure-next-auth.session-token=session-value; oai-did=device-value"
                );
            }
            _ => panic!("official cookies should be replayed"),
        }
        let broken = cookie("oai-did", "a;b", "chatgpt.com", None);
        assert!(matches!(
            session_cookie(official, &[broken], now),
            SessionCookie::Invalid
        ));
        assert!(matches!(
            session_cookie("not a url", &[], now),
            SessionCookie::Invalid
        ));
    }
}
