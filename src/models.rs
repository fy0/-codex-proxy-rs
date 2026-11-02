//! 在原生模型列表上，仅追加当前 Key 与生图账号范围有交集的 Images 模型。
//! 独立 Images 接口不属于 upstream_adapter v5 的生成模型执行器。
//! 因此这里不把 gpt-image 的外部 JSON 生图协议伪装成 Responses 适配模型。
use serde_json::{Value, json};

use crate::config::Config;

pub fn merge(raw: &[u8], allowed: &[String]) -> Option<Vec<u8>> {
    let mut document: Value = serde_json::from_slice(raw).ok()?;
    if document.get("object").and_then(Value::as_str) != Some("list") {
        return None;
    }
    let models = document.get_mut("data")?.as_array_mut()?;
    // 不改动普通模型及其元数据；移除本插件覆盖、但 Key 无权使用的图片模型。
    models.retain(|entry| {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            return true;
        };
        !Config::is_image_model(id) || allowed.iter().any(|choice| choice == id)
    });
    for id in allowed {
        if !Config::is_image_model(id) {
            continue;
        }
        if !models
            .iter()
            .any(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
        {
            models.push(json!({
                "id": id,
                "object": "model",
                "created": 0,
                "owned_by": "openai",
            }));
        }
    }
    serde_json::to_vec(&document).ok()
}

pub fn detail(raw: &[u8], allowed: &[String], id: &str) -> Option<Vec<u8>> {
    if !Config::is_image_model(id) || !allowed.iter().any(|name| name == id) {
        return None;
    }
    let merged = merge(raw, allowed)?;
    let document: Value = serde_json::from_slice(&merged).ok()?;
    let model = document
        .get("data")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))?;
    serde_json::to_vec(model).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_qualified_images_added_and_normal_models_preserved() {
        let raw = br#"{"object":"list","data":[{"id":"gpt-6","owned_by":"openai","custom":7},{"id":"gpt-image-1"}]}"#;
        let result: Value =
            serde_json::from_slice(&merge(raw, &["gpt-image-2".to_owned()]).unwrap()).unwrap();
        assert_eq!(result["data"].as_array().unwrap().len(), 2);
        assert_eq!(result["data"][0]["custom"], 7);
        assert_eq!(result["data"][1]["id"], "gpt-image-2");
        assert!(detail(raw, &["gpt-image-2".into()], "gpt-image-1").is_none());
        assert!(detail(raw, &["gpt-image-2".into()], "gpt-image-2").is_some());
    }
    #[test]
    fn no_scope_does_not_add_any_image_or_distort_regular_models() {
        let raw = br#"{"object":"list","data":[{"id":"gpt-6"},{"id":"gpt-image-2"}]}"#;
        let result: Value = serde_json::from_slice(&merge(raw, &[]).unwrap()).unwrap();
        assert_eq!(result["data"].as_array().unwrap().len(), 1);
        assert_eq!(result["data"][0]["id"], "gpt-6");
    }
}
