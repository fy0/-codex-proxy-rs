use serde_json::{Value, json};
use std::{
    collections::HashSet,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub b64_json: String,
    pub revised_prompt: Option<String>,
}

#[derive(Debug)]
pub struct ResultImages {
    pub created: u64,
    pub images: Vec<Image>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn gather(value: &Value, results: &mut Vec<Image>, ids: &mut HashSet<String>) {
    let items: Vec<&Value> = if let Some(output) = value.as_array() {
        output.iter().collect()
    } else {
        vec![value]
    };
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("image_generation_call") {
            continue;
        }
        let Some(b64) = item.get("result").and_then(Value::as_str) else {
            continue;
        };
        if b64.is_empty() {
            continue;
        }
        let id = item.get("id").and_then(Value::as_str).unwrap_or("");
        if !id.is_empty() && !ids.insert(id.to_owned()) {
            continue;
        }
        results.push(Image {
            b64_json: b64.to_owned(),
            revised_prompt: item
                .get("revised_prompt")
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    }
}

#[derive(Debug)]
pub struct ParseError {
    pub status: u16,
    pub message: &'static str,
}

fn status_for(error: &Value) -> u16 {
    let kind = error.get("type").and_then(Value::as_str).unwrap_or("");
    let code = error.get("code").and_then(Value::as_str).unwrap_or("");
    if kind.contains("rate_limit") || code.contains("rate_limit") {
        429
    } else if kind.contains("permission") || code == "forbidden" {
        403
    } else if kind.contains("authentication") || code == "unauthorized" {
        401
    } else if kind.contains("invalid_request")
        || code.contains("policy")
        || code == "moderation_blocked"
    {
        400
    } else {
        502
    }
}

/// 仅接受已完成的 Responses，避免把 partial image 或普通文字当作成功生图。
/// SSE 必须逐事件解析，不能依赖 TCP/HTTP chunk 与事件边界对齐。
pub fn parse_images(input: &[u8]) -> Result<ResultImages, ParseError> {
    let mut staged = Vec::new();
    let mut final_images = Vec::new();
    let mut staged_ids = HashSet::new();
    let mut final_ids = HashSet::new();
    let mut created = now();
    let mut completed = false;
    let mut failure = None;

    let mut process = |event: &Value| {
        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
        if event_type == "error" || event_type == "response.failed" {
            let err = event
                .get("error")
                .or_else(|| event.get("response").and_then(|v| v.get("error")))
                .unwrap_or(&Value::Null);
            failure = Some(ParseError {
                status: status_for(err),
                message: "upstream image generation failed",
            });
            return;
        }
        if event_type == "response.output_item.done" {
            if let Some(item) = event.get("item") {
                gather(item, &mut staged, &mut staged_ids);
            }
        }
        if event_type == "response.completed"
            || (event_type.is_empty() && event.get("output").is_some())
        {
            let response = event.get("response").unwrap_or(event);
            if let Some(status) = response.get("status").and_then(Value::as_str) {
                if status == "failed" || status == "incomplete" {
                    failure = Some(ParseError {
                        status: 502,
                        message: "upstream image generation was not completed",
                    });
                    return;
                }
            }
            if let Some(err) = response.get("error").filter(|v| !v.is_null()) {
                failure = Some(ParseError {
                    status: status_for(err),
                    message: "upstream image generation failed",
                });
                return;
            }
            if let Some(ts) = response.get("created_at").and_then(Value::as_u64) {
                created = ts;
            }
            if let Some(output) = response.get("output") {
                gather(output, &mut final_images, &mut final_ids);
            }
            completed = true;
        }
    };

    let mut got_event = false;
    if let Ok(json) = serde_json::from_slice::<Value>(input) {
        process(&json);
        got_event = true;
    } else {
        // SSE 在 '\n\n' / '\r\n\r\n' 处界定事件；data: 可以跨多行。
        let mut data = Vec::<u8>::new();
        for raw_line in input.split(|b| *b == b'\n') {
            let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
            if line.is_empty() {
                if !data.is_empty() {
                    if data == b"[DONE]" {
                        data.clear();
                        continue;
                    }
                    if let Ok(value) = serde_json::from_slice::<Value>(&data) {
                        process(&value);
                        got_event = true;
                    }
                    data.clear();
                }
            } else if let Some(rest) = line.strip_prefix(b"data:") {
                if !data.is_empty() {
                    data.push(b'\n');
                }
                data.extend_from_slice(rest.strip_prefix(b" ").unwrap_or(rest));
            }
        }
        if !data.is_empty() && data != b"[DONE]" {
            if let Ok(value) = serde_json::from_slice::<Value>(&data) {
                process(&value);
                got_event = true;
            }
        }
    }

    if let Some(error) = failure {
        return Err(error);
    }
    if !got_event || !completed {
        return Err(ParseError {
            status: 502,
            message: "upstream did not complete the image generation response",
        });
    }
    let images = if final_images.is_empty() {
        staged
    } else {
        final_images
    };
    if images.is_empty() {
        return Err(ParseError {
            status: 502,
            message: "image_generation returned no image",
        });
    }
    Ok(ResultImages { created, images })
}

pub fn encode_response(output: ResultImages, streaming: bool) -> (Vec<u8>, &'static str) {
    if !streaming {
        let data = output
            .images
            .into_iter()
            .map(|image| {
                let mut result = json!({"b64_json":image.b64_json});
                if let Some(prompt) = image.revised_prompt {
                    result["revised_prompt"] = json!(prompt);
                }
                result
            })
            .collect::<Vec<_>>();
        let encoded =
            serde_json::to_vec(&json!({"created":output.created,"data":data})).unwrap_or_default();
        return (encoded, "application/json");
    }
    // 这里只提供最终事件，不提供伪造的实时 partial_image 进度。
    let mut buf = Vec::new();
    for (index, image) in output.images.into_iter().enumerate() {
        let event = json!({
            "type":"image_generation.completed",
            "created_at":output.created,
            "image_index":index,
            "b64_json":image.b64_json,
        });
        buf.extend_from_slice(b"event: image_generation.completed\ndata: ");
        buf.extend_from_slice(&serde_json::to_vec(&event).unwrap_or_default());
        buf.extend_from_slice(b"\n\n");
    }
    buf.extend_from_slice(b"data: [DONE]\n\n");
    (buf, "text/event-stream")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_completed_outputs_and_keeps_fallback_when_empty() {
        let response = b"data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"image_generation_call\",\"id\":\"a\",\"result\":\"YWJj\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"created_at\":123,\"output\":[]}}\n\ndata: [DONE]\n\n";
        let out = parse_images(response).unwrap();
        assert_eq!(out.created, 123);
        assert_eq!(out.images[0].b64_json, "YWJj");
    }

    #[test]
    fn can_parse_final_json_and_stream_result() {
        let response = br#"{"type":"response.completed","response":{"created_at":124,"output":[{"id":"im-1","type":"image_generation_call","result":"eHl6"}]}}"#;
        let images = parse_images(response).unwrap();
        let (data, mime) = encode_response(images, true);
        assert_eq!(mime, "text/event-stream");
        assert!(
            String::from_utf8(data)
                .unwrap()
                .contains("image_generation.completed")
        );
    }

    #[test]
    fn incomplete_and_error_are_not_false_successes() {
        assert!(parse_images(b"data: [DONE]\n\n").is_err());
        let response =
            br#"{"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded"}}}"#;
        assert_eq!(parse_images(response).unwrap_err().status, 429);
    }
}
