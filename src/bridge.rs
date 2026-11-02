use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::stream;
use multer::Multipart;
use serde_json::{Value, json};

use crate::config::Config;

pub const MAX_REQUEST_BYTES: usize = 55 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 160 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 50 * 1024 * 1024;
const MAX_IMAGES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Generation,
    Edit,
}

#[derive(Debug)]
pub struct Request {
    pub model: String,
    pub upstream: Value,
    pub stream: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeError {
    pub status: u16,
    pub message: String,
}

impl BridgeError {
    fn bad(msg: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: msg.into(),
        }
    }
}

fn string_value(value: &Value, name: &str) -> Result<Option<String>, BridgeError> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(BridgeError::bad(format!("{name} must be a string"))),
    }
}

fn image_reference(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.to_owned()),
        Value::Object(o) => o
            .get("image_url")
            .or_else(|| o.get("url"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        _ => None,
    }
}

fn image_refs(value: &Value) -> Result<Vec<String>, BridgeError> {
    let inputs = if let Value::Array(items) = value {
        items
            .iter()
            .map(image_reference)
            .collect::<Option<Vec<_>>>()
    } else {
        image_reference(value).map(|item| vec![item])
    }
    .ok_or_else(|| {
        BridgeError::bad("image must be a URL, data URL, or an array of image references")
    })?;
    if inputs.is_empty()
        || inputs.len() > MAX_IMAGES
        || inputs
            .iter()
            .any(|s| s.is_empty() || s.len() > MAX_REQUEST_BYTES)
    {
        return Err(BridgeError::bad(
            "invalid number or size of image references",
        ));
    }
    // 仅向上游转发 HTTPS 和 data URL；不读取用户指定的本地/内网资源。
    if !inputs
        .iter()
        .all(|s| s.starts_with("https://") || s.starts_with("data:image/"))
    {
        return Err(BridgeError::bad(
            "image references must use HTTPS or data:image URLs",
        ));
    }
    Ok(inputs)
}

pub fn parse_json(body: &[u8], kind: Kind, driver: &str) -> Result<Request, BridgeError> {
    let v: Value =
        serde_json::from_slice(body).map_err(|_| BridgeError::bad("invalid JSON body"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| BridgeError::bad("request body must be a JSON object"))?;
    let model = string_value(&v, "model")?.unwrap_or_else(|| "gpt-image-1".to_owned());
    if !Config::is_image_model(&model) {
        return Err(BridgeError::bad(
            "unsupported image model for the Responses bridge",
        ));
    }
    let prompt = string_value(&v, "prompt")?.unwrap_or_default();
    if prompt.trim().is_empty() {
        return Err(BridgeError::bad("prompt is required"));
    }
    if prompt.len() > 2 * 1024 * 1024 {
        return Err(BridgeError::bad("prompt is too long"));
    }
    let streamed = v.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if v.get("stream").is_some_and(|s| !s.is_boolean()) {
        return Err(BridgeError::bad("stream must be a boolean"));
    }
    if let Some(format) = string_value(&v, "response_format")? {
        if format != "b64_json" {
            return Err(BridgeError::bad(
                "response_format=url requires image storage and is not supported by this plugin",
            ));
        }
    }
    let n = match v.get("n") {
        None => 1_u64,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| BridgeError::bad("n must be an integer"))?,
    };
    if n == 0 || n > 10 {
        return Err(BridgeError::bad("n must be between 1 and 10"));
    }
    let mut contents = vec![json!({"type":"input_text", "text":prompt})];
    let images = obj.get("image").or_else(|| obj.get("images"));
    if kind == Kind::Edit {
        let input = images.ok_or_else(|| BridgeError::bad("image is required for edits"))?;
        for img in image_refs(input)? {
            contents.push(json!({"type":"input_image", "image_url":img}));
        }
    } else if images.is_some() {
        return Err(BridgeError::bad("image inputs require /v1/images/edits"));
    }
    let mut tool = json!({
        "type": "image_generation",
        "action": if kind == Kind::Edit { "edit" } else { "generate" },
        "model": model
    });
    if n > 1 {
        tool["n"] = json!(n);
    }
    for key in [
        "size",
        "quality",
        "background",
        "output_format",
        "moderation",
        "style",
    ] {
        if let Some(s) = string_value(&v, key)? {
            if !s.trim().is_empty() {
                tool[key] = json!(s);
            }
        }
    }
    for key in ["output_compression", "partial_images"] {
        if let Some(value) = v.get(key) {
            if !value.is_u64() {
                return Err(BridgeError::bad(format!(
                    "{key} must be a non-negative integer"
                )));
            }
            tool[key] = value.clone();
        }
    }
    if let Some(mask) = v.get("mask") {
        let url = image_refs(mask)?;
        if url.len() != 1 || kind != Kind::Edit {
            return Err(BridgeError::bad(
                "mask is only valid for edits and must contain one image",
            ));
        }
        tool["input_image_mask"] = json!({"image_url":url[0]});
    }
    let upstream = json!({
        "model":driver,
        "instructions":"When invoking the image_generation tool, use the user's image prompt verbatim. Do not rewrite, expand, summarize, embellish, translate, normalize punctuation, or add or remove visual details or constraints. Preserve the original language, wording, capitalization, quotes, and punctuation exactly.",
        "stream":true,
        "store":false,
        "parallel_tool_calls":true,
        "reasoning":{"effort":"medium","summary":"auto"},
        "include":["reasoning.encrypted_content"],
        "tool_choice":{"type":"image_generation"},
        "input":[{"type":"message","role":"user","content":contents}],
        "tools":[tool]
    });
    Ok(Request {
        model,
        upstream,
        stream: streamed,
    })
}

/// 将 multipart 的统一中间结构转换为 Codex 原生 JSON Images 编辑协议。
/// 只用于已经确认该图片模型由 Codex 原生 Images 端点承担的请求。
pub fn direct_images_payload(request: &Request) -> Value {
    let content = &request.upstream["input"][0]["content"];
    let prompt = content[0]["text"].as_str().unwrap_or_default();
    let mut body = json!({
        "model": request.model.clone(),
        "prompt": prompt,
        "images": content.as_array().into_iter().flatten().skip(1).filter_map(|part| {
            part.get("image_url").and_then(Value::as_str)
                .map(|url| json!({"image_url":url}))
        }).collect::<Vec<_>>()
    });
    let tool = &request.upstream["tools"][0];
    for key in [
        "n",
        "size",
        "quality",
        "background",
        "output_format",
        "moderation",
        "style",
        "output_compression",
        "partial_images",
    ] {
        if let Some(value) = tool.get(key) {
            body[key] = value.clone();
        }
    }
    if let Some(mask) = tool.get("input_image_mask") {
        body["mask"] = mask.clone();
    }
    if request.stream {
        body["stream"] = json!(true);
    }
    body
}

/// 把 multipart/form-data 的上传图片转换为 Images JSON，随后复用同一套参数校验。
/// 不把上传文件发送到宿主以外的第三方存储，也不触碰本地文件系统。
pub async fn parse_multipart(
    body: Vec<u8>,
    content_type: &str,
    kind: Kind,
    driver: &str,
) -> Result<Request, BridgeError> {
    if kind != Kind::Edit {
        return Err(BridgeError::bad(
            "multipart is only supported for image edits",
        ));
    }
    let boundary = multer::parse_boundary(content_type)
        .map_err(|_| BridgeError::bad("invalid multipart boundary"))?;
    let chunks = stream::once(async move { Ok::<_, std::io::Error>(bytes::Bytes::from(body)) });
    let mut mp = Multipart::new(chunks, boundary);
    let mut form = serde_json::Map::new();
    let mut images = Vec::<Value>::new();
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|_| BridgeError::bad("invalid multipart part"))?
    {
        let name = field.name().unwrap_or("").to_owned();
        if name == "image" || name == "image[]" || name == "mask" {
            let mime = field
                .content_type()
                .map(|value| value.essence_str().to_owned())
                .unwrap_or_else(|| "application/octet-stream".into());
            if !matches!(
                mime.as_str(),
                "image/png" | "image/jpeg" | "image/webp" | "image/gif"
            ) {
                return Err(BridgeError::bad(
                    "uploaded images must be PNG, JPEG, WebP, or GIF",
                ));
            }
            let data = field
                .bytes()
                .await
                .map_err(|_| BridgeError::bad("unable to read image upload"))?;
            if data.is_empty() || data.len() > MAX_IMAGE_BYTES {
                return Err(BridgeError::bad("uploaded image must be 1..50 MiB"));
            }
            let url = format!("data:{mime};base64,{}", STANDARD.encode(&data));
            if name == "mask" {
                if form.insert("mask".into(), json!(url)).is_some() {
                    return Err(BridgeError::bad("only one mask is allowed"));
                }
            } else {
                images.push(json!(url));
                if images.len() > MAX_IMAGES {
                    return Err(BridgeError::bad("too many uploaded images"));
                }
            }
        } else if matches!(
            name.as_str(),
            "model"
                | "prompt"
                | "size"
                | "quality"
                | "background"
                | "output_format"
                | "moderation"
                | "style"
                | "n"
                | "output_compression"
                | "partial_images"
                | "response_format"
                | "stream"
        ) {
            let text = field
                .text()
                .await
                .map_err(|_| BridgeError::bad("invalid multipart text field"))?;
            if text.len() > 2 * 1024 * 1024 {
                return Err(BridgeError::bad("multipart text field too large"));
            }
            let value = match name.as_str() {
                "stream" => json!(text == "true"),
                "n" | "output_compression" | "partial_images" => {
                    json!(
                        text.parse::<u64>()
                            .map_err(|_| BridgeError::bad("invalid numeric form value"))?
                    )
                }
                _ => json!(text),
            };
            form.insert(name, value);
        }
    }
    if images.is_empty() {
        return Err(BridgeError::bad("image upload is required"));
    }
    form.insert("image".into(), Value::Array(images));
    let body =
        serde_json::to_vec(&form).map_err(|_| BridgeError::bad("invalid multipart values"))?;
    parse_json(&body, kind, driver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_prompt_and_uses_explicit_image_model() {
        let req = parse_json(
            r#"{"model":"gpt-image-1","prompt":" 猫 · blue  ","n":2,"size":"1024x1024"}"#
                .as_bytes(),
            Kind::Generation,
            "gpt-5.6-luna",
        )
        .unwrap();
        assert_eq!(
            req.upstream["input"][0]["content"][0]["text"],
            " 猫 · blue  "
        );
        assert_eq!(req.upstream["tools"][0]["model"], "gpt-image-1");
        assert_eq!(req.upstream["tools"][0]["n"], 2);
        assert_eq!(req.upstream["stream"], true);
    }

    #[test]
    fn accepts_data_url_edits_and_mask() {
        let req = parse_json(br#"{"prompt":"remove object","image":"data:image/png;base64,AA==","mask":"data:image/png;base64,AA=="}"#, Kind::Edit, "gpt-5.6-luna").unwrap();
        assert_eq!(
            req.upstream["input"][0]["content"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(req.upstream["tools"][0]["action"], "edit");
        assert_eq!(
            req.upstream["tools"][0]["input_image_mask"]["image_url"],
            "data:image/png;base64,AA=="
        );
    }

    #[tokio::test]
    async fn parses_standard_multipart_edit() {
        let body = b"--XyZ\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngpt-image-1\r\n--XyZ\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\nreplace the sky\r\n--XyZ\r\nContent-Disposition: form-data; name=\"image\"; filename=\"input.png\"\r\nContent-Type: image/png\r\n\r\nPNGDATA\r\n--XyZ--\r\n";
        let req = parse_multipart(
            body.to_vec(),
            "multipart/form-data; boundary=XyZ",
            Kind::Edit,
            "gpt-5.6-luna",
        )
        .await
        .unwrap();
        assert_eq!(
            req.upstream["input"][0]["content"][0]["text"],
            "replace the sky"
        );
        assert!(
            req.upstream["input"][0]["content"][1]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
    }

    #[test]
    fn direct_edits_use_native_images_wire_shape() {
        let req = parse_json(
            br#"{"model":"gpt-image-2","prompt":"improve","image":"data:image/png;base64,AA=="}"#,
            Kind::Edit,
            "gpt-5.6-luna",
        )
        .unwrap();
        let direct = direct_images_payload(&req);
        assert_eq!(direct["model"], "gpt-image-2");
        assert_eq!(
            direct["images"][0]["image_url"],
            "data:image/png;base64,AA=="
        );
        assert!(direct.get("tools").is_none());
    }

    #[test]
    fn rejects_missing_edit_images_and_unsupported_url_result() {
        assert_eq!(
            parse_json(br#"{"prompt":"x"}"#, Kind::Edit, "gpt-5.6-luna")
                .unwrap_err()
                .status,
            400
        );
        assert_eq!(
            parse_json(
                br#"{"prompt":"x","response_format":"url"}"#,
                Kind::Generation,
                "gpt-5.6-luna"
            )
            .unwrap_err()
            .status,
            400
        );
    }
}
