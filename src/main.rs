mod access;
mod bridge;
mod config;
mod jobs;
mod management;
mod models;
mod policy;
mod quota_guard;
mod quota_management;
mod response;
mod state_migration;

use std::sync::Arc;

use gateway_plugin_sdk::{
    CallBudgets, ErrorCode, PluginFault, Stage,
    call::middleware::MiddlewareHeader,
    client::{
        HttpBody, HttpCall, HttpFrame, HttpRequest, HttpResponse, MiddlewareCall,
        MiddlewareResponse, MiddlewareResult, PluginBuilder, PluginSession, RequestCall,
        SessionConfig, methods,
    },
};
use serde_json::{Value, json};

use crate::{
    bridge::{BridgeError, Kind},
    config::Config,
};

const MAX_CATALOG_BYTES: usize = 8 * 1024 * 1024;
const INTERNAL_IMAGE_INTENT_HEADER: &str = "x-cpr-image-bridge-intent";
const MAX_RESPONSES_INPUT_BYTES: usize = 60 * 1024 * 1024;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session_config = SessionConfig::default();
    let mut session =
        PluginSession::accept(tokio::io::stdin(), tokio::io::stdout(), session_config).await?;
    // 独立维护父调用可继续执行长耗时图片模型任务，不与管理页连接寿命绑定。
    session.set_call_budgets(CallBudgets::from([(Stage::Maintenance, 900_000)]))?;
    let config = Arc::new(
        Config::from_configuration(&session.handshake().configuration)
            .map_err(|s| std::io::Error::new(std::io::ErrorKind::InvalidInput, s))?,
    );
    let raw_config = Arc::new(session.handshake().configuration.clone());
    let artifact_sha256 = Arc::new(session.handshake().artifact_sha256.clone());
    let mut management_registration = management::registration();
    let quota_registration = quota_management::registration();
    management_registration
        .routes
        .extend(quota_registration.routes);
    management_registration
        .resources
        .extend(quota_registration.resources);
    management_registration
        .pages
        .extend(quota_registration.pages);
    management_registration
        .callbacks
        .extend(quota_registration.callbacks);

    let registry = Arc::new(policy::ImageRegistry::default());
    let jobs = Arc::new(jobs::BackendJobs::new(Arc::clone(&config)));
    let quota = Arc::new(quota_guard::QuotaGuard::new(Arc::new(
        config.quota_guard.clone(),
    )));
    let handler = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .middleware({
            let config = Arc::clone(&config);
            let registry = Arc::clone(&registry);
            move |call: MiddlewareCall| {
                let config = Arc::clone(&config);
                let registry = Arc::clone(&registry);
                async move {
                    match call {
                        MiddlewareCall::Http(call) => handle_http(*call, &config)
                            .await
                            .map(MiddlewareResult::Http),
                        MiddlewareCall::Request(call) => handle_request(*call, &config, &registry)
                            .await
                            .map(MiddlewareResult::Request),
                        other => other.forward().await,
                    }
                }
            }
        })?
        .on(methods::SCHEDULE_ACCOUNT, {
            let config = Arc::clone(&config);
            let registry = Arc::clone(&registry);
            move |call| {
                let config = Arc::clone(&config);
                let registry = Arc::clone(&registry);
                async move { policy::schedule(call, config, registry).await }
            }
        })?
        .management(management_registration, {
            let config = Arc::clone(&config);
            let jobs = Arc::clone(&jobs);
            let raw_config = Arc::clone(&raw_config);
            let artifact_sha256 = Arc::clone(&artifact_sha256);
            move |call| {
                let config = Arc::clone(&config);
                let jobs = Arc::clone(&jobs);
                let raw_config = Arc::clone(&raw_config);
                let artifact_sha256 = Arc::clone(&artifact_sha256);
                async move {
                    if call.request.path.starts_with("quota/") {
                        quota_management::handle(
                            call,
                            &raw_config,
                            &config.quota_guard,
                            &artifact_sha256,
                        )
                        .await
                    } else {
                        management::handle(call, &config, &jobs).await
                    }
                }
            }
        })?
        // 状态 Schema 哈希变化后必须经宿主受管迁移，不能直接复用 Schema v1。
        .on(methods::STATE_MIGRATE, state_migration::migrate)?
        .on(methods::RECONCILE, {
            let jobs = Arc::clone(&jobs);
            let quota = Arc::clone(&quota);
            move |call| {
                let jobs = Arc::clone(&jobs);
                let quota = Arc::clone(&quota);
                async move {
                    // 图片运行期间继续刷新额度，避免长生图使账号每日费用观测自然过期。
                    // 两者仍属于同一个受管父调用；图片结束后完成当轮刷新再停止。
                    let (quota_result, jobs_result) = quota
                        .reconcile_with_jobs(&call.host, jobs.reconcile(&call.host))
                        .await;
                    quota_result?;
                    jobs_result?;
                    Ok(gateway_plugin_sdk::client::TypedReply::new(
                        gateway_plugin_sdk::client::Empty {},
                    ))
                }
            }
        })?
        .build()?;
    session.run(handler).await?;
    Ok(())
}

fn header_value<'a>(headers: &'a [MiddlewareHeader], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .and_then(|h| std::str::from_utf8(&h.value).ok())
}

fn remove_headers(headers: &mut Vec<MiddlewareHeader>, names: &[&str]) {
    headers.retain(|h| !names.iter().any(|n| h.name.eq_ignore_ascii_case(n)));
}

fn add_header(headers: &mut Vec<MiddlewareHeader>, name: &str, value: &str) {
    headers.push(MiddlewareHeader {
        name: name.to_owned(),
        value: value.as_bytes().to_vec(),
    });
}

fn replace_body(response: &mut HttpResponse, payload: Vec<u8>, content_type: &str) {
    remove_headers(
        &mut response.headers,
        &[
            "content-length",
            "content-encoding",
            "transfer-encoding",
            "content-type",
            "etag",
            "content-md5",
        ],
    );
    add_header(&mut response.headers, "content-type", content_type);
    add_header(
        &mut response.headers,
        "content-length",
        &payload.len().to_string(),
    );
    response.body = HttpBody::from_bytes(payload);
}

fn error_response(status: u16, code: &str, message: &str) -> HttpResponse {
    let payload = serde_json::to_vec(&json!({
        "error": {
            "type": if status >= 500 { "api_error" } else { "invalid_request_error" },
            "code": code,
            "message": message
        }
    }))
    .unwrap_or_default();
    let mut response = HttpResponse::new(status, HttpBody::from_bytes(payload.clone()));
    add_header(&mut response.headers, "content-type", "application/json");
    add_header(
        &mut response.headers,
        "content-length",
        &payload.len().to_string(),
    );
    add_header(&mut response.headers, "cache-control", "no-store");
    response
}

async fn read_body(body: &mut HttpBody, limit: usize) -> Result<Option<Vec<u8>>, PluginFault> {
    let mut bytes = Vec::new();
    loop {
        match body.read().await? {
            None => return Ok(Some(bytes)),
            Some(HttpFrame::Trailers(_)) => {}
            Some(HttpFrame::Data(chunk)) => {
                if chunk.len() > limit.saturating_sub(bytes.len()) {
                    body.close().await?;
                    return Ok(None);
                }
                bytes.extend_from_slice(&chunk);
            }
        }
    }
}

fn image_kind(method: &str, path: &str) -> Option<Kind> {
    if method != "POST" {
        return None;
    }
    match path {
        "/v1/images/generations" => Some(Kind::Generation),
        "/v1/images/edits" => Some(Kind::Edit),
        _ => None,
    }
}

fn path(uri: &str) -> &str {
    uri.split('?').next().unwrap_or(uri)
}

fn codex_model_list(uri: &str) -> bool {
    uri.split_once('?').is_some_and(|(_, query)| {
        query.split('&').any(|item| {
            item.strip_prefix("client_version=")
                .is_some_and(|value| !value.is_empty())
        })
    })
}

fn json_content_type(content_type: &str) -> bool {
    let kind = content_type.split(';').next().unwrap_or("").trim();
    kind.is_empty() || kind.eq_ignore_ascii_case("application/json")
}

fn root_model(json_body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(json_body).ok()?;
    let obj = v.as_object()?;
    match obj.get("model") {
        None => Some("gpt-image-1".to_owned()),
        Some(Value::String(s)) => Some(s.to_owned()),
        _ => None,
    }
}

// 在选择桥接路径前判断 multipart 的模型；不发起外网请求或依赖用户指定 URL。
// 只读受限长度的已接收正文，原生透传时仍使用完全相同的原始字节。
fn multipart_model(body: &[u8], content_type: &str) -> Option<String> {
    let boundary = multer::parse_boundary(content_type).ok()?;
    let needle = format!("--{boundary}");
    let text = String::from_utf8_lossy(body);
    // Multipart 文件可能很大；这里的检查只是选择分支，完整解析仍交给 parse_multipart。
    // 使用字段解析器更可靠，此处仅在找不到有效 model 时默认生成型别名。
    for part in text.split(&needle) {
        let Some(header_end) = part.find("\r\n\r\n") else {
            continue;
        };
        let headers = &part[..header_end];
        if headers
            .lines()
            .any(|line| line.to_ascii_lowercase().contains("name=\"model\""))
        {
            return Some(
                part[header_end + 4..]
                    .trim_end_matches(|c: char| matches!(c, '\r' | '\n' | '-'))
                    .trim()
                    .to_owned(),
            );
        }
    }
    Some("gpt-image-1".to_owned())
}

fn update_for_responses(request: &mut HttpRequest, body: Vec<u8>, timeout_ms: u64) {
    request.uri = "/v1/responses".to_owned();
    request.body = HttpBody::from_bytes(body);
    request.timeout_ms = Some(timeout_ms);
    remove_headers(
        &mut request.headers,
        &[
            "content-type",
            "content-length",
            "content-encoding",
            "transfer-encoding",
            "accept-encoding",
            "accept",
        ],
    );
    add_header(&mut request.headers, "content-type", "application/json");
    add_header(&mut request.headers, "accept", "text/event-stream");
}

// 内部意图头只由 HTTP Images 入口写入。普通客户端不能伪造此标记来绕过策略。
async fn handle_request(
    call: RequestCall,
    config: &Config,
    registry: &policy::ImageRegistry,
) -> Result<MiddlewareResponse, PluginFault> {
    let RequestCall {
        mut request, next, ..
    } = call;
    // 大型图片编辑请求可能超过纯正文解析上限，内部意图标记由 HTTP
    // 中间件在身份/模型许可检查后设置，在出站前即刻移除。
    let marked_image = request
        .head
        .headers
        .iter()
        .find(|header| {
            header
                .name
                .eq_ignore_ascii_case(INTERNAL_IMAGE_INTENT_HEADER)
        })
        .and_then(|header| std::str::from_utf8(&header.value).ok())
        .map(str::to_owned);
    request.remove_header(INTERNAL_IMAGE_INTENT_HEADER);
    let image_model =
        marked_image.or_else(|| policy::image_intent(&request.head.operation, &request.body));
    if let Some(model) = image_model {
        if !config.has_image_scope() || !config.enabled_image_model(&model) {
            return Err(PluginFault::new(
                ErrorCode::PermissionDenied,
                "image model or account scope is disabled",
            ));
        }
        registry.mark(
            &request.head.request_id,
            &request.head.client_key_id,
            &model,
        );
    }
    next.run(request).await
}

fn prepare_codex_image_headers(request: &mut HttpRequest, call_id: &str) {
    // CodexImagesBackend 为每次独立 Images 请求附加 x-codex-image-turn-id。
    // 真实 Codex turn ID 已经存在时保留；其它客户端使用宿主创建的唯一 HTTP call ID。
    // originator / Authorization 仍由选中账号的 CPR Provider 生成，绝不伪造。
    let turn_id = header_value(&request.headers, "x-codex-image-turn-id")
        .filter(|v| {
            !v.is_empty() && v.len() <= 256 && v.as_bytes().iter().all(|b| *b >= 0x20 && *b <= 0x7e)
        })
        .map(str::to_owned);
    // 官方 CodexImagesBackend 可以携带可选 x-codex-turn-metadata；不能删除。
    // 由 CPR Provider 层按照现有安全规则处理该 Header。
    remove_headers(&mut request.headers, &["x-codex-image-turn-id"]);
    add_header(
        &mut request.headers,
        "x-codex-image-turn-id",
        turn_id.as_deref().unwrap_or(call_id),
    );
}

async fn handle_http(call: HttpCall, config: &Config) -> Result<HttpResponse, PluginFault> {
    let HttpCall {
        mut request,
        next,
        host,
        call_id,
        ..
    } = call;
    let current_path = path(&request.uri).to_owned();
    // HTTP middleware 位于原生鉴权之前，因此先清除来自客户端的内部标记。
    remove_headers(&mut request.headers, &[INTERNAL_IMAGE_INTENT_HEADER]);
    // 官方 Key 身份与授权账号事实，不用 model_catalog v2 将独立 Images 冒充 Responses 模型。
    let authorization = header_value(&request.headers, "authorization")
        .unwrap_or("")
        .to_owned();
    if request.method == "GET" && current_path == "/v1/models" && !codex_model_list(&request.uri) {
        let mut response = next.run(request).await?;
        if response.status != 200 {
            return Ok(response);
        }
        let raw = match read_body(&mut response.body, MAX_CATALOG_BYTES).await? {
            Some(raw) => raw,
            None => {
                return Ok(error_response(
                    502,
                    "catalog_too_large",
                    "model catalog response is too large",
                ));
            }
        };
        let allowed = match access::allowed_models(&host, config, &authorization).await {
            Ok(allowed) => allowed,
            Err(_) => {
                return Ok(error_response(
                    503,
                    "scope_check_failed",
                    "cannot verify image model access for this Client Key",
                ));
            }
        };
        if let Some(updated) = models::merge(&raw, &allowed) {
            replace_body(&mut response, updated, "application/json");
        } else {
            response.body = HttpBody::from_bytes(raw);
        }
        return Ok(response);
    }
    if request.method == "GET" {
        if let Some(model) = current_path.strip_prefix("/v1/models/") {
            if Config::is_image_model(model) {
                request.uri = "/v1/models".to_owned();
                let mut response = next.run(request).await?;
                if response.status != 200 {
                    return Ok(response);
                }
                let raw = match read_body(&mut response.body, MAX_CATALOG_BYTES).await? {
                    Some(raw) => raw,
                    None => {
                        return Ok(error_response(
                            502,
                            "catalog_too_large",
                            "model catalog response is too large",
                        ));
                    }
                };
                let allowed = match access::allowed_models(&host, config, &authorization).await {
                    Ok(allowed) => allowed,
                    Err(_) => {
                        return Ok(error_response(
                            503,
                            "scope_check_failed",
                            "cannot verify image model access for this Client Key",
                        ));
                    }
                };
                return Ok(match models::detail(&raw, &allowed, model) {
                    Some(detail) => {
                        replace_body(&mut response, detail, "application/json");
                        response
                    }
                    None => error_response(
                        404,
                        "model_not_found",
                        "image model is unavailable for this Client Key",
                    ),
                });
            }
        }
    }

    let Some(kind) = image_kind(&request.method, &current_path) else {
        return next.run(request).await;
    };
    // 禁止范围为空时通过原生 Images 或 Responses 桥接绕过选定的账号池。
    if !config.has_image_scope() {
        return Ok(error_response(
            403,
            "image_account_scope_empty",
            "image generation is disabled until account or group IDs are configured",
        ));
    }
    let content_type = header_value(&request.headers, "content-type")
        .unwrap_or("")
        .to_owned();
    let content_encoding = header_value(&request.headers, "content-encoding").unwrap_or("");
    if !content_encoding.is_empty() && !content_encoding.eq_ignore_ascii_case("identity") {
        // 这里不是解压端点，压缩请求仍交还宿主的原生 Images 处理器。
        return Ok(error_response(
            415,
            "unsupported_content_encoding",
            "compressed image requests are not supported by this plugin",
        ));
    }
    let json_body = json_content_type(&content_type);
    let multipart = content_type
        .to_ascii_lowercase()
        .starts_with("multipart/form-data");
    if !json_body && !multipart {
        return Ok(error_response(
            415,
            "unsupported_content_type",
            "image requests must contain JSON or multipart/form-data",
        ));
    }

    let raw = match read_body(&mut request.body, bridge::MAX_REQUEST_BYTES).await? {
        Some(raw) => raw,
        None => {
            return Ok(error_response(
                413,
                "request_too_large",
                "image request exceeds the 55 MiB plugin limit",
            ));
        }
    };
    let target_model = if json_body {
        root_model(&raw)
    } else {
        multipart_model(&raw, &content_type)
    };
    let Some(model) = target_model else {
        return Ok(error_response(
            400,
            "invalid_image_model",
            "image model is missing or invalid",
        ));
    };
    if !config.enabled_image_model(&model) {
        // 当前配置未启用的图片模型不允许绕过白名单。
        return Ok(error_response(
            400,
            "unsupported_image_model",
            "image model is not enabled by this plugin",
        ));
    }
    let allowed = match access::allowed_models(&host, config, &authorization).await {
        Ok(allowed) => allowed,
        Err(_) => {
            return Ok(error_response(
                503,
                "scope_check_failed",
                "cannot verify image model access for this Client Key",
            ));
        }
    };
    if !allowed.contains(&model) {
        return Ok(error_response(
            403,
            "image_model_forbidden",
            "this Client Key has no permitted image-generation account",
        ));
    }
    if !config.use_responses(&model) {
        add_header(&mut request.headers, INTERNAL_IMAGE_INTENT_HEADER, &model);
        prepare_codex_image_headers(&mut request, &format!("cpr-image-{call_id}"));
        if multipart && kind == Kind::Edit && Config::is_image_model(&model) {
            // 原生 Images 端点接受 JSON 结构；把标准 multipart 图片转换成 data:image URL。
            let parsed =
                match bridge::parse_multipart(raw, &content_type, kind, &config.driver).await {
                    Ok(parsed) => parsed,
                    Err(BridgeError { status, message }) => {
                        return Ok(error_response(status, "invalid_image_request", &message));
                    }
                };
            let payload = bridge::direct_images_payload(&parsed);
            match serde_json::to_vec(&payload) {
                Ok(body) if body.len() <= MAX_RESPONSES_INPUT_BYTES => {
                    request.body = HttpBody::from_bytes(body);
                    remove_headers(
                        &mut request.headers,
                        &[
                            "content-type",
                            "content-length",
                            "transfer-encoding",
                            "content-encoding",
                        ],
                    );
                    add_header(&mut request.headers, "content-type", "application/json");
                }
                Ok(_) => {
                    return Ok(error_response(
                        413,
                        "image_request_too_large",
                        "encoded image inputs exceed the 60 MiB limit",
                    ));
                }
                Err(_) => {
                    return Ok(error_response(
                        500,
                        "serialization_error",
                        "unable to encode image edits",
                    ));
                }
            }
        } else {
            request.body = HttpBody::from_bytes(raw);
        }
        return next.run(request).await;
    }

    let converted = if json_body {
        bridge::parse_json(&raw, kind, &config.driver)
    } else {
        bridge::parse_multipart(raw, &content_type, kind, &config.driver).await
    };
    let converted = match converted {
        Ok(result) => result,
        Err(BridgeError { status, message }) => {
            return Ok(error_response(status, "invalid_image_request", &message));
        }
    };
    debug_assert_eq!(converted.model, model);
    let outbound = match serde_json::to_vec(&converted.upstream) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(error_response(
                500,
                "serialization_error",
                "unable to encode image generation request",
            ));
        }
    };
    if outbound.len() > MAX_RESPONSES_INPUT_BYTES {
        return Ok(error_response(
            413,
            "image_request_too_large",
            "encoded image inputs exceed the 60 MiB Responses limit",
        ));
    }
    remove_headers(&mut request.headers, &["x-codex-image-turn-id"]);
    add_header(&mut request.headers, INTERNAL_IMAGE_INTENT_HEADER, &model);
    update_for_responses(&mut request, outbound, config.timeout_ms);
    let mut upstream = next.run(request).await?;
    if !(200..300).contains(&upstream.status) {
        return Ok(upstream);
    }
    if header_value(&upstream.headers, "content-encoding")
        .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("identity"))
    {
        return Ok(error_response(
            502,
            "unsupported_upstream_encoding",
            "unexpected compressed Responses body",
        ));
    }
    let raw = match read_body(&mut upstream.body, bridge::MAX_RESPONSE_BYTES).await? {
        Some(raw) => raw,
        None => {
            return Ok(error_response(
                502,
                "image_result_too_large",
                "upstream image response exceeds the 160 MiB limit",
            ));
        }
    };
    let images = match response::parse_images(&raw) {
        Ok(images) => images,
        Err(err) => {
            return Ok(error_response(
                err.status,
                "image_generation_failed",
                err.message,
            ));
        }
    };
    let (payload, content_type) = response::encode_response(images, converted.stream);
    let mut result = HttpResponse::new(200, HttpBody::from_bytes(payload.clone()));
    add_header(&mut result.headers, "content-type", content_type);
    add_header(
        &mut result.headers,
        "content-length",
        &payload.len().to_string(),
    );
    add_header(&mut result.headers, "cache-control", "no-store");
    for name in [
        "x-request-id",
        "x-gateway-request-id",
        "x-codex-imagegen-request-id",
    ] {
        if let Some(value) = header_value(&upstream.headers, name) {
            add_header(&mut result.headers, name, value);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_only_images_and_skips_codex_native_catalog() {
        assert_eq!(
            image_kind("POST", "/v1/images/generations"),
            Some(Kind::Generation)
        );
        assert_eq!(image_kind("GET", "/v1/images/generations"), None);
        assert_eq!(image_kind("POST", "/v1/images/edits"), Some(Kind::Edit));
        assert!(!codex_model_list("/v1/models"));
        assert!(codex_model_list("/v1/models?client_version=0.154.0"));
        assert!(!codex_model_list("/v1/models?client_version="));
    }

    #[test]
    fn detects_multipart_model_after_an_uploaded_image() {
        let body = b"--TEST\r\nContent-Disposition: form-data; name=\"image\"; filename=\"input.png\"\r\nContent-Type: image/png\r\n\r\n\x89PNG\r\n--TEST\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngpt-image-2\r\n--TEST--\r\n";
        assert_eq!(
            multipart_model(body, "multipart/form-data; boundary=TEST"),
            Some("gpt-image-2".to_owned())
        );
    }

    fn image_request_for_header_tests() -> HttpRequest {
        HttpRequest {
            settings: Value::Null,
            method: "POST".into(),
            uri: "/v1/images/generations".into(),
            version: gateway_plugin_sdk::call::middleware::http::Version::Http11,
            headers: Vec::new(),
            timeout_ms: Some(300_000),
            body: HttpBody::empty(),
        }
    }

    #[test]
    fn codex_native_header_parity_preserves_valid_turn_and_originator() {
        let mut req = image_request_for_header_tests();
        add_header(
            &mut req.headers,
            "x-codex-image-turn-id",
            "codex-turn-from-client",
        );
        add_header(&mut req.headers, "originator", "codex_cli_rs");
        add_header(
            &mut req.headers,
            "x-codex-turn-metadata",
            "official-turn-metadata",
        );
        prepare_codex_image_headers(&mut req, "cpr-image-8");
        assert_eq!(
            header_value(&req.headers, "x-codex-image-turn-id"),
            Some("codex-turn-from-client")
        );
        assert_eq!(
            header_value(&req.headers, "originator"),
            Some("codex_cli_rs")
        );
        assert_eq!(
            header_value(&req.headers, "x-codex-turn-metadata"),
            Some("official-turn-metadata")
        );
        assert_eq!(
            req.headers
                .iter()
                .filter(|h| h.name == "x-codex-image-turn-id")
                .count(),
            1
        );
    }

    #[test]
    fn codex_native_header_parity_generates_safe_turn_id() {
        let mut req = image_request_for_header_tests();
        prepare_codex_image_headers(&mut req, "cpr-image-generated-call-1");
        assert_eq!(
            header_value(&req.headers, "x-codex-image-turn-id"),
            Some("cpr-image-generated-call-1")
        );
        let mut unsafe_req = image_request_for_header_tests();
        add_header(
            &mut unsafe_req.headers,
            "x-codex-image-turn-id",
            "bad\r\nvalue",
        );
        prepare_codex_image_headers(&mut unsafe_req, "cpr-image-generated-call-2");
        assert_eq!(
            header_value(&unsafe_req.headers, "x-codex-image-turn-id"),
            Some("cpr-image-generated-call-2")
        );
    }

    #[test]
    fn image_model_registry_does_not_accept_unrecognized_models() {
        assert_eq!(
            root_model(br#"{"model":"gpt-image-2"}"#),
            Some("gpt-image-2".to_owned())
        );
        assert_eq!(
            root_model(br#"{"model":"gpt-image-3"}"#),
            Some("gpt-image-3".to_owned())
        );
        assert!(Config::is_image_model("gpt-image-3"));
        assert!(Config::is_image_model("gpt-image-2"));
        let config = Config::from_configuration(&serde_json::json!({})).unwrap();
        assert!(!config.enabled_image_model("gpt-image-3"));
        assert!(config.enabled_image_model("gpt-image-2"));
        assert_eq!(root_model(br#"{"model":123}"#), None);
    }

    #[test]
    fn response_header_replacement_drops_stale_lengths_and_compression() {
        let mut result = HttpResponse::new(200, HttpBody::empty());
        add_header(&mut result.headers, "content-length", "100");
        add_header(&mut result.headers, "content-encoding", "gzip");
        replace_body(&mut result, b"{}".to_vec(), "application/json");
        assert_eq!(header_value(&result.headers, "content-length"), Some("2"));
        assert!(header_value(&result.headers, "content-encoding").is_none());
    }
}
