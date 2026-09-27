use provider_openai::transport::bps::{
    parse_terminal, prepare_request_body, synthetic_sse, transform_response,
};
use serde_json::{Map, Value, json};

fn source(body: Value) -> Map<String, Value> {
    body.as_object().expect("request object").clone()
}

fn dumps(value: &Value) -> String {
    serde_json::to_string(value).expect("serialize")
}

#[test]
fn prepare_should_wrap_tools_into_the_transport_catalog() {
    let request = source(json!({
        "model": "gpt-6-astra-bps",
        "stream": true,
        "instructions": "be helpful",
        "input": [{"type": "message", "role": "user", "content": "hi"}],
        "tools": [{
            "type": "function",
            "name": "exec_command",
            "description": "run a shell command",
            "parameters": {"type": "object", "properties": {"cmd": {"type": "string"}}, "required": ["cmd"]},
        }],
    }));
    let body = prepare_request_body(&request, "gpt-6-astra");

    assert_eq!(
        body.get("model").and_then(Value::as_str),
        Some("gpt-6-astra")
    );
    assert_eq!(
        body.get("model_selection").and_then(Value::as_str),
        Some("explicit")
    );
    assert_eq!(body.get("store"), Some(&Value::Bool(false)));
    assert_eq!(body.get("stream"), Some(&Value::Bool(true)));
    // tools 不直接透传；目录写进 developer 提示。
    assert!(body.get("tools").is_none());
    let input = body.get("input").and_then(Value::as_array).expect("input");
    let roles: Vec<&str> = input
        .iter()
        .filter_map(|item| item.get("role").and_then(Value::as_str))
        .collect();
    assert_eq!(&roles[..2], ["developer", "developer"]);
    assert_eq!(roles.last(), Some(&"user"));
    let catalog = input[1]["content"][0]["text"]
        .as_str()
        .expect("catalog text");
    assert!(catalog.contains("run_officejs"));
    assert!(catalog.contains("exec_command"));
    let metadata = body
        .get("metadata")
        .and_then(Value::as_object)
        .expect("metadata");
    assert!(metadata.contains_key("task_id"));
    assert!(metadata.contains_key("turn_id"));
    assert_eq!(
        metadata.get("agent_iteration").and_then(Value::as_str),
        Some("1")
    );
}

#[test]
fn prepare_should_disable_the_catalog_when_tool_choice_is_none() {
    let request = source(json!({
        "input": "hello",
        "tool_choice": "none",
        "tools": [{"type": "function", "name": "exec_command"}],
    }));
    let body = prepare_request_body(&request, "gpt-6-astra");

    let input = body.get("input").and_then(Value::as_array).expect("input");
    let catalog = input
        .iter()
        .filter_map(|item| item.pointer("/content/0/text").and_then(Value::as_str))
        .find(|text| text.contains("relayed"))
        .expect("catalog message");
    assert!(!catalog.contains("exec_command"));
}

#[test]
fn prepare_should_envelope_client_calls_as_run_officejs() {
    let request = source(json!({
        "input": [{
            "type": "function_call",
            "call_id": "call_1",
            "name": "exec_command",
            "arguments": "{\"cmd\":\"pwd\"}",
        }],
        "tools": [{"type": "function", "name": "exec_command"}],
    }));
    let body = prepare_request_body(&request, "gpt-6-astra");
    let input = body.get("input").and_then(Value::as_array).expect("input");
    let call = input
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .and_then(Value::as_object)
        .expect("enveloped call");

    assert_eq!(
        call.get("name").and_then(Value::as_str),
        Some("run_officejs")
    );
    let outer: Value = serde_json::from_str(
        call.get("arguments")
            .and_then(Value::as_str)
            .expect("outer arguments"),
    )
    .expect("outer JSON");
    let inner: Value =
        serde_json::from_str(outer["code"].as_str().expect("code")).expect("inner JSON");
    assert_eq!(inner["name"], "exec_command");
    assert_eq!(inner["arguments"]["cmd"], "pwd");
}

#[test]
fn prepare_should_replay_remembered_native_calls() {
    let first = source(json!({
        "input": [{
            "type": "function_call",
            "call_id": "call_native_bps_test",
            "name": "run_officejs",
            "arguments": "{\"code\":\"{}\"}",
        }],
    }));
    prepare_request_body(&first, "gpt-6-astra");
    let second = source(json!({
        "input": [{
            "type": "function_call_output",
            "call_id": "call_native_bps_test",
            "output": "ok",
        }],
    }));
    let body = prepare_request_body(&second, "gpt-6-astra");
    let input = body.get("input").and_then(Value::as_array).expect("input");

    // 回放身份的输出按 function_call_output 归一化。
    let output = input
        .iter()
        .find(|item| item.get("call_id").and_then(Value::as_str) == Some("call_native_bps_test"))
        .expect("tool output item");
    assert_eq!(
        output.get("type").and_then(Value::as_str),
        Some("function_call_output")
    );
}

#[test]
fn transform_should_restore_the_client_tool_call() {
    let source = source(json!({
        "tools": [{
            "type": "function",
            "name": "exec_command",
            "parameters": {"type": "object", "properties": {"cmd": {"type": "string"}}, "required": ["cmd"]},
        }],
    }));
    let code = dumps(&json!({"name": "exec_command", "arguments": {"cmd": "pwd"}}));
    let mut response = json!({
        "id": "resp_1",
        "status": "completed",
        "output": [{
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "run_officejs",
            "arguments": dumps(&json!({
                "summary": "x", "code": code, "destructive": false, "references": [],
            })),
        }],
    })
    .as_object()
    .expect("response")
    .clone();

    assert!(transform_response(&mut response, &source));
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .expect("output");
    let call = output[0].as_object().expect("call");
    assert_eq!(
        call.get("type").and_then(Value::as_str),
        Some("function_call")
    );
    assert_eq!(
        call.get("name").and_then(Value::as_str),
        Some("exec_command")
    );
    assert_eq!(
        call.get("arguments").and_then(Value::as_str),
        Some("{\"cmd\":\"pwd\"}")
    );
}

#[test]
fn transform_should_pass_through_without_a_single_transport_call() {
    let source = source(json!({
        "tools": [{"type": "function", "name": "exec_command"}],
    }));
    let mut response = json!({
        "status": "completed",
        "output": [
            {"type": "message", "role": "assistant"},
            {"type": "function_call", "call_id": "c1", "name": "run_officejs", "arguments": "{}"},
            {"type": "function_call", "call_id": "c2", "name": "run_officejs", "arguments": "{}"},
        ],
    })
    .as_object()
    .expect("response")
    .clone();

    // 两个 transport 调用无法唯一还原，响应原样透传。
    assert!(!transform_response(&mut response, &source));
    assert_eq!(
        response
            .get("output")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(3)
    );
}

#[test]
fn synthetic_sse_should_emit_a_canonical_event_sequence() {
    let response = json!({
        "id": "resp_1",
        "status": "completed",
        "output": [{
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "exec_command",
            "arguments": "{\"cmd\":\"pwd\"}",
        }],
    });
    let bytes = synthetic_sse(response.as_object().expect("response"));
    let text = String::from_utf8(bytes).expect("utf8");

    let created = text.find("event: response.created");
    let in_progress = text.find("event: response.in_progress");
    let added = text.find("event: response.output_item.added");
    let args_done = text.find("event: response.function_call_arguments.done");
    let item_done = text.find("event: response.output_item.done");
    let completed = text.find("event: response.completed");
    assert!(
        created < in_progress
            && in_progress < added
            && added < args_done
            && args_done < item_done
            && item_done < completed,
        "unexpected synthetic SSE order: {text}"
    );
    assert!(text.ends_with("data: [DONE]\n\n"));
}

#[test]
fn parse_terminal_should_extract_completed_response_from_sse() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"id\":\"resp_9\"}}\n\n",
        "data: [DONE]\n\n",
    );
    let response = parse_terminal(body, true).expect("terminal response");
    assert_eq!(response.get("id").and_then(Value::as_str), Some("resp_9"));
}

#[test]
fn parse_terminal_should_accept_a_plain_json_response() {
    let response = parse_terminal("{\"status\":\"completed\",\"id\":\"resp_7\"}", false)
        .expect("terminal response");
    assert_eq!(response.get("id").and_then(Value::as_str), Some("resp_7"));
}
