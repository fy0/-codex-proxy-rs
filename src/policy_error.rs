//! 以本实例的拒绝或过滤事实补充诊断，区分额度耗尽与宿主后续策略拒绝。
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const TTL: Duration = Duration::from_secs(12 * 60);
const MAX_ENTRIES: usize = 4096;

#[derive(Clone, Copy)]
pub enum DenialKind {
    Quota,
    QuotaFiltered,
    ImageScope,
    ImageAccess,
}

#[derive(Clone)]
pub struct Denial {
    pub kind: DenialKind,
    pub model: Option<String>,
    pub candidate_count: usize,
    pub scoped_candidate_count: usize,
    pub quota_blocked_count: usize,
}

impl Denial {
    pub fn status(&self) -> u16 {
        match self.kind {
            DenialKind::Quota => 429,
            DenialKind::QuotaFiltered | DenialKind::ImageScope | DenialKind::ImageAccess => 403,
        }
    }

    pub fn code(&self) -> &'static str {
        match self.kind {
            DenialKind::Quota => "cpr_quota_guard_exhausted",
            DenialKind::QuotaFiltered => "cpr_quota_guard_filtered_policy_denied",
            DenialKind::ImageScope => "cpr_image_scope_denied",
            DenialKind::ImageAccess => "cpr_image_access_denied",
        }
    }

    pub fn body(&self) -> Value {
        let message = match self.kind {
            DenialKind::Quota => format!(
                "CPR Image Bridge quota guard blocked all {} account(s) remaining in this request's scope ({} host candidate(s)). Other accounts and models are not globally disabled.",
                self.scoped_candidate_count, self.candidate_count,
            ),
            DenialKind::QuotaFiltered => format!(
                "Gateway policy denied this request after CPR Image Bridge quota guard excluded {} of {} scoped account(s); {} candidate(s) remained. Check session/account restrictions if the session is bound to an excluded account. This is not a global model quota ban.",
                self.quota_blocked_count, self.scoped_candidate_count,
                self.scoped_candidate_count.saturating_sub(self.quota_blocked_count),
            ),
            DenialKind::ImageScope => "CPR Image Bridge found no account in the configured image scope for this request. Requests declaring an image_generation tool also require an allowed image account.".to_owned(),
            DenialKind::ImageAccess => "CPR Image Bridge found no image account allowed by this Client Key.".to_owned(),
        };
        json!({"error": {
            "code": self.code(),
            "type": if matches!(self.kind, DenialKind::Quota) { "rate_limit_error" } else { "invalid_request_error" },
            "message": message,
            "source": "community.codex-image-bridge",
            "details": {
                "model": self.model,
                "candidate_count": self.candidate_count,
                "scoped_candidate_count": self.scoped_candidate_count,
                "quota_blocked_count": self.quota_blocked_count,
            }
        }})
    }
}

#[derive(Default, Clone)]
pub struct DenialRegistry(Arc<Mutex<HashMap<String, (Instant, Denial)>>>);

impl DenialRegistry {
    fn get(&self, request_id: &str) -> Option<Denial> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(request_id)
            .filter(|(at, _)| at.elapsed() < TTL)
            .map(|(_, denial)| denial.clone())
    }

    pub fn record(&self, request_id: &str, denial: Denial) {
        if request_id.is_empty() {
            return;
        }
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, (at, _)| at.elapsed() < TTL);
        if entries.len() < MAX_ENTRIES || entries.contains_key(request_id) {
            entries.insert(request_id.to_owned(), (Instant::now(), denial));
        }
    }

    pub fn remove(&self, request_id: &str) -> Option<Denial> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(request_id)
            .filter(|(at, _)| at.elapsed() < TTL)
            .map(|(_, denial)| denial)
    }
}

/// 只在本请求已有拒绝证据时缓冲完整 SSE 事件，普通流逐帧透传。
pub struct SseFilter {
    registry: DenialRegistry,
    request_id: String,
    pending: Vec<u8>,
    passthrough: bool,
}

impl Drop for SseFilter {
    fn drop(&mut self) {
        self.registry.remove(&self.request_id);
    }
}

impl SseFilter {
    pub fn new(registry: DenialRegistry, request_id: String) -> Self {
        Self {
            registry,
            request_id,
            pending: Vec::new(),
            passthrough: false,
        }
    }

    pub fn push(&mut self, bytes: Vec<u8>) -> Option<Vec<u8>> {
        if self.passthrough {
            return Some(bytes);
        }
        let Some(denial) = self.registry.get(&self.request_id) else {
            self.pending.extend(bytes);
            return Some(std::mem::take(&mut self.pending));
        };
        self.pending.extend(bytes);
        // 调度错误事件很小；异常大事件不继续缓存或改写。
        if self.pending.len() > 64 * 1024 {
            self.passthrough = true;
            self.registry.remove(&self.request_id);
            return Some(std::mem::take(&mut self.pending));
        }
        let mut output = Vec::new();
        while let Some(end) = event_end(&self.pending) {
            let event = self.pending.drain(..end).collect::<Vec<_>>();
            match rewrite_event(&event, &denial) {
                Some(rewritten) => {
                    output.extend(rewritten);
                    output.append(&mut self.pending);
                    self.registry.remove(&self.request_id);
                    self.passthrough = true;
                    break;
                }
                None => output.extend(event),
            }
        }
        (!output.is_empty()).then_some(output)
    }
}

fn event_end(bytes: &[u8]) -> Option<usize> {
    let lf = bytes
        .windows(2)
        .position(|part| part == b"\n\n")
        .map(|i| i + 2);
    let crlf = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .map(|i| i + 4);
    lf.into_iter().chain(crlf).min()
}

fn rewrite_event(event: &[u8], denial: &Denial) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(event).ok()?;
    let data = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    let mut value: Value = serde_json::from_str(&data).ok()?;
    let error_path = match value.get("type").and_then(Value::as_str)? {
        "response.failed" => "/response/error",
        "error" => "/error",
        _ => return None,
    };
    let error = value.pointer_mut(error_path)?.as_object_mut()?;
    if error.get("code").and_then(Value::as_str) != Some("policy_denied") {
        return None;
    }
    error.extend(denial.body()["error"].as_object()?.clone());
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut output = String::new();
    let mut inserted = false;
    for line in text.lines() {
        if line.starts_with("data:") {
            if !inserted {
                output.push_str("data: ");
                output.push_str(&serde_json::to_string(&value).ok()?);
                output.push_str(newline);
                inserted = true;
            }
        } else {
            output.push_str(line);
            output.push_str(newline);
        }
    }
    Some(output.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_error_rewrite_handles_every_byte_boundary_and_preserves_event_metadata() {
        for newline in ["\n", "\r\n"] {
            let event = format!(
                "event: response.failed{newline}id: 7{newline}data: {{\"type\":\"response.failed\",\"sequence_number\":7,\"response\":{{\"id\":\"resp_1\",\"error\":{{\"code\":\"policy_denied\",\"message\":\"Request blocked by policy.\"}}}}}}{newline}{newline}"
            );
            for split in 1..event.len() {
                let registry = DenialRegistry::default();
                registry.record(
                    "r",
                    Denial {
                        kind: DenialKind::Quota,
                        model: Some("gpt-6.1-sol".into()),
                        candidate_count: 3,
                        scoped_candidate_count: 1,
                        quota_blocked_count: 1,
                    },
                );
                let mut filter = SseFilter::new(registry, "r".into());
                let mut output = filter
                    .push(event.as_bytes()[..split].to_vec())
                    .unwrap_or_default();
                output.extend(
                    filter
                        .push(event.as_bytes()[split..].to_vec())
                        .unwrap_or_default(),
                );
                let output = String::from_utf8(output).unwrap();
                assert!(
                    output.starts_with(&format!("event: response.failed{newline}id: 7{newline}"))
                );
                let value: Value = serde_json::from_str(
                    output
                        .lines()
                        .find_map(|line| line.strip_prefix("data: "))
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(value["sequence_number"], 7);
                assert_eq!(value["response"]["id"], "resp_1");
                assert_eq!(
                    value["response"]["error"]["code"],
                    "cpr_quota_guard_exhausted"
                );
            }
        }
    }
}
