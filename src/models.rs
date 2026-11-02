use crate::config::{Config, IMAGE_MODELS};
use serde_json::{Value, json};

fn get_models(body: &[u8]) -> Result<Value, ()> {
    let list: Value = serde_json::from_slice(body).map_err(|_| ())?;
    if list.get("object").and_then(Value::as_str) != Some("list") {
        return Err(());
    }
    if list.get("data").and_then(Value::as_array).is_none() {
        return Err(());
    }
    Ok(list)
}

pub fn add_models(body: &[u8], config: &Config) -> Option<Vec<u8>> {
    let mut root = get_models(body).ok()?;
    let list = root.get_mut("data")?.as_array_mut()?;
    // 不对 Codex client_version 原生目录加假的文本模型元数据。
    // 只有本次已认证 Key 确实看得到 Responses driver，才公布桥接模型。
    let driver = list
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(config.driver.as_str()))?;
    let owned = driver
        .get("owned_by")
        .and_then(Value::as_str)
        .unwrap_or("openai")
        .to_owned();
    for id in IMAGE_MODELS {
        if !list
            .iter()
            .any(|entry| entry.get("id").and_then(Value::as_str) == Some(*id))
        {
            list.push(json!({"id":id,"object":"model","created":0,"owned_by":owned.clone()}));
        }
    }
    serde_json::to_vec(&root).ok()
}

pub fn detail_for(body: &[u8], config: &Config, id: &str) -> Option<Vec<u8>> {
    if !Config::is_image_model(id) {
        return None;
    }
    let modified = add_models(body, config)?;
    let list: Value = serde_json::from_slice(&modified).ok()?;
    let image = list
        .get("data")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))?;
    serde_json::to_vec(image).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_add_when_driver_visible_and_never_duplicate() {
        let cfg = Config::from_configuration(&serde_json::json!({
            "driverModel": "gpt-5.6-luna"
        }))
        .unwrap();
        assert!(add_models(br#"{"object":"list","data":[]}"#, &cfg).is_none());
        let root = br#"{"object":"list","data":[{"id":"gpt-5.6-luna","owned_by":"openai"},{"id":"gpt-image-2"}]}"#;
        let out = add_models(root, &cfg).unwrap();
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        let ids = parsed["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["id"] == "gpt-image-2")
            .count();
        assert_eq!(ids, 1);
        assert_eq!(
            serde_json::from_slice::<Value>(&detail_for(root, &cfg, "gpt-image-1").unwrap())
                .unwrap()["id"],
            "gpt-image-1"
        );
    }
}
