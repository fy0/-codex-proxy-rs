//! 在官方 Provider 与路由实现上验证后台生图使用的合同。
//!
//! 本模块由同目录 install.py 注入官方 contract 测试模块，复用原有账号、
//! RuntimeSnapshot、Provider 和 wiremock fixture，不替换宿主解析或执行实现。

use super::*;
use gateway_core::operation::{ImageRequest, ImageRequestKind, RawJsonPayload};

const IMAGE_MODELS: [&str; 2] = ["gpt-image-2.5-sunburst", "gpt-image-2.6-sunburst"];
const IMAGE_SIZES: [&str; 3] = ["1024x1024", "2048x2048", "2160x3840"];
const CONTRACT_ACCOUNT: &str = "acct_provider_contract";
const DRIVER_MODEL: &str = "gpt-5.4";

fn native_image_body(model: &str, size: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model": model,
        "prompt": "A solid blue circle on a white background.",
        "size": size,
        "quality": "auto",
        "n": 1,
        "output_format": "webp",
        "output_compression": 85
    }))
    .expect("native Images request JSON")
}

fn native_image_operation(body: Vec<u8>) -> Operation {
    Operation::GenerateImage(ImageRequest::from_raw_json(
        ImageRequestKind::Generation,
        RawJsonPayload::new("openai", Bytes::from(body)).expect("native Images payload"),
    ))
}

#[tokio::test]
async fn native_images_model_target_is_invalid_before_any_upstream_send() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, CONTRACT_ACCOUNT).await;
    let provider = provider_with_base_url(&store, server.uri());

    for model in IMAGE_MODELS {
        for size in IMAGE_SIZES {
            // 真实 RuntimeSnapshot::plan 生成 Some(upstream_model)，与模型回调一致。
            let request = planned_request_for_model(
                "openai",
                native_image_operation(native_image_body(model, size)),
                model,
            );
            let error = provider
                .clone()
                .execute(
                    request,
                    context("req_bridge_native_model", CancellationToken::new()),
                )
                .await
                .err()
                .expect("the pinned provider rejects a model-target native Images request");
            assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
            assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        }
    }

    assert!(
        server
            .received_requests()
            .await
            .expect("captured requests")
            .is_empty(),
        "native Images model-target rejection must happen before HTTP"
    );
}

#[tokio::test]
async fn native_images_provider_endpoint_preserves_original_parameters() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .expect((IMAGE_MODELS.len() * IMAGE_SIZES.len()) as u64)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, CONTRACT_ACCOUNT).await;
    let provider = provider_with_base_url(&store, server.uri());
    let mut expected_bodies = Vec::new();

    for model in IMAGE_MODELS {
        for size in IMAGE_SIZES {
            let body = native_image_body(model, size);
            expected_bodies.push(body.clone());
            // 真实 plan_provider_endpoint 保留 None，不伪造 ProviderCandidate。
            let mut stream = provider
                .clone()
                .execute(
                    planned_provider_endpoint_request("openai", native_image_operation(body)),
                    context("req_bridge_native_endpoint", CancellationToken::new()),
                )
                .await
                .expect("provider endpoint accepts a native Images payload");
            while let Some(event) = stream.next().await {
                event.expect("native Images response stream");
            }
        }
    }

    let requests = server.received_requests().await.expect("captured requests");
    assert_eq!(requests.len(), expected_bodies.len());
    for (request, expected) in requests.iter().zip(expected_bodies) {
        assert_eq!(
            request.body, expected,
            "native payload must stay byte-exact"
        );
    }
}

fn responses_image_body(image_model: &str, size: &str) -> Value {
    json!({
        "model": DRIVER_MODEL,
        "instructions": "When invoking the image_generation tool, use the user's image prompt verbatim. Do not rewrite, expand, summarize, embellish, translate, normalize punctuation, or add or remove visual details or constraints. Preserve the original language, wording, capitalization, quotes, and punctuation exactly.",
        "parallel_tool_calls": false,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "A solid blue circle on a white background."
            }]
        }],
        "tools": [{
            "type": "image_generation",
            "model": image_model,
            "action": "generate",
            "size": size,
            "quality": "auto",
            "output_format": "webp",
            "output_compression": 85
        }],
        "tool_choice": {"type": "image_generation"},
        "store": false,
        "stream": true
    })
}

#[tokio::test]
async fn responses_image_tool_crosses_real_parser_and_http_transport() {
    let server = MockServer::start().await;
    // 这里只验证协议透传；模拟结果不是对上游模型或图片像素的验收。
    let image_result = "Y29udHJhY3QtaW1hZ2UtZGF0YQ==";
    let item = json!({
        "id": "ig_bridge_contract",
        "type": "image_generation_call",
        "status": "completed",
        "result": image_result,
        "output_format": "webp"
    });
    let done = json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": item.clone()
    });
    let completed = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_bridge_contract",
            "model": DRIVER_MODEL,
            "status": "completed",
            "output": [item],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        }
    });
    let sse = format!(
        "event: response.output_item.done\ndata: {done}\n\nevent: response.completed\ndata: {completed}\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .expect((IMAGE_MODELS.len() * IMAGE_SIZES.len()) as u64)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, CONTRACT_ACCOUNT).await;
    let provider = provider_with_base_url(&store, server.uri());
    let mut expected_bodies = Vec::new();

    for (image_model, size) in IMAGE_MODELS
        .into_iter()
        .flat_map(|model| IMAGE_SIZES.into_iter().map(move |size| (model, size)))
    {
        let body = responses_image_body(image_model, size);
        expected_bodies.push(body.clone());
        let payload = ProtocolPayload::json_object(
            "openai",
            body.as_object().expect("Responses body object").clone(),
        )
        .expect("Responses protocol payload")
        .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))]));
        let request = planned_request_for_model(
            "openai",
            Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
            DRIVER_MODEL,
        );
        let mut stream = provider
            .clone()
            .execute(
                request,
                context("req_bridge_responses", CancellationToken::new()),
            )
            .await
            .expect("Responses image tool request crosses the real provider parser");
        let mut wire_events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.expect("Responses image tool stream");
            if let Some(wire) = event.wire_event() {
                wire_events.push(wire.data().clone());
            }
        }
        assert!(
            wire_events
                .iter()
                .any(|event| event["item"]["result"] == image_result),
            "output_item.done must retain the image tool result"
        );
        assert!(
            wire_events
                .iter()
                .any(|event| { event["response"]["output"][0]["result"] == image_result }),
            "response.completed must retain the image tool result"
        );
    }

    let requests = server.received_requests().await.expect("captured requests");
    assert_eq!(requests.len(), expected_bodies.len());
    for (request, expected) in requests.iter().zip(expected_bodies) {
        let actual = captured_request_body(request);
        // 后台生图不强制要求 driver 提供推理能力或加密推理内容。
        assert!(
            actual.get("reasoning").is_none(),
            "reasoning must stay absent"
        );
        assert!(actual.get("include").is_none(), "include must stay absent");
        for field in [
            "model",
            "instructions",
            "parallel_tool_calls",
            "input",
            "tools",
            "tool_choice",
            "store",
            "stream",
        ] {
            assert_eq!(actual[field], expected[field], "Responses field {field}");
        }
    }
}
