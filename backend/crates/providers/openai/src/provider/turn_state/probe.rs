//! 探测题不再携带 Codex 系统提示词；题库声明期望片段，回答正文随观测留存。

use chrono::{DateTime, Utc};
use futures::StreamExt;
use gateway_core::account::TurnStateConfig;
use gateway_protocol::openai::events::ResponseModelObservation;
use gateway_protocol::openai::sse::SseEventDecoder;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::transport::profile::CodexWireProfileState;

/// 探测题库：(shape 标识, 问题正文, 回答必须包含的片段)。
/// 题目只依赖模型自身知识、不要求联网；片段按 ASCII 大小写不敏感匹配。
const QUESTIONS: &[(&str, &str, &str)] = &[
    (
        "x_handle",
        "Don't search the internet. Who is Thibault Sottiaux on X? Your answer must include the @ handle.",
        "@thsottiaux",
    ),
    (
        "japan_pm",
        "不要搜索，只凭记忆，告诉我日本首相是谁。",
        "高市早苗",
    ),
];

/// 回答正文只用于命中判定与管理端展示，按字节上限截断。
const ANSWER_LIMIT: usize = 8 * 1024;

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

/// 有界读取 SSE 的停止结果：reason 维持原语义，reported_model 记录上游在
/// 已读事件里声明的实际模型（头块声明优先于 response.model）。
pub(super) struct ProbeOutput {
    pub reason: &'static str,
    pub reported_model: Option<String>,
    pub created_model: Option<String>,
    /// 题库问题的回答正文（按字节截断）；只有读取正文的探测才会产生。
    pub answer: Option<String>,
    /// 回答是否包含题库声明的期望片段；题目未声明期望时为空。
    pub answer_match: Option<bool>,
}

fn push_bounded(answer: &mut String, delta: &str) {
    let mut end = delta.len().min(ANSWER_LIMIT.saturating_sub(answer.len()));
    while !delta.is_char_boundary(end) {
        end -= 1;
    }
    answer.push_str(&delta[..end]);
}

fn contains_ignore_ascii_case(answer: &str, expected: &str) -> bool {
    answer
        .to_ascii_lowercase()
        .contains(&expected.to_ascii_lowercase())
}

pub(super) async fn wait_for_output(
    response: reqwest::Response,
    stop_at_model: bool,
    expect: Option<&str>,
) -> ProbeOutput {
    let mut stream = response.bytes_stream();
    let mut decoder = SseEventDecoder::default();
    let mut received = 0_usize;
    // 事件头里的声明优先，response.model 兜底，与业务观测的合并规则一致。
    let mut header_model: Option<String> = None;
    let mut body_model = ResponseModelObservation::default();
    let mut created_model = None;
    let mut answer = String::new();
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
                if event_type == Some("response.created") {
                    created_model = value
                        .get("response")
                        .and_then(|response| response.get("model"))
                        .and_then(Value::as_str)
                        .filter(|model| !model.is_empty() && model.len() <= 256)
                        .map(str::to_owned);
                    if stop_at_model && expect.is_none() && created_model.is_some() {
                        break 'read "response_created";
                    }
                }
                match event_type {
                    Some("response.output_text.delta") => {
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                            push_bounded(&mut answer, delta);
                        }
                        if let Some(expected) = expect {
                            if contains_ignore_ascii_case(&answer, expected) {
                                break 'read "answer_matched";
                            }
                            if answer.len() >= ANSWER_LIMIT {
                                break 'read "answer_limit";
                            }
                        } else {
                            break 'read "first_output";
                        }
                    }
                    Some(
                        "response.refusal.delta"
                        | "response.reasoning_text.delta"
                        | "response.reasoning_summary_text.delta"
                        | "response.audio.delta"
                        | "response.output_audio.delta",
                    ) => {
                        if expect.is_none() {
                            break 'read "first_output";
                        }
                    }
                    Some(
                        "response.completed" | "response.failed" | "response.incomplete" | "error",
                    ) => break 'read "terminal",
                    _ => {}
                }
            }
        }
    };
    let answer_match = expect.map(|expected| contains_ignore_ascii_case(&answer, expected));
    ProbeOutput {
        reason,
        created_model,
        reported_model: header_model.or_else(|| body_model.model().map(str::to_owned)),
        answer: (!answer.is_empty()).then_some(answer),
        answer_match,
    }
}

pub(super) struct ProbeRequest {
    pub headers: HeaderMap,
    pub body: Value,
    pub shape: &'static str,
    pub effort: &'static str,
    pub expect: &'static str,
    pub id: String,
}

pub(super) fn request(
    model: &str,
    config: &TurnStateConfig,
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
    let (shape, prompt, expect) = QUESTIONS[random_index(QUESTIONS.len())];
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
        expect,
        id: id.clone(),
        body: json!({
            "model": model, "stream": true, "store": false,
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
