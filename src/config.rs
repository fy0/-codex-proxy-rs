use serde_json::Value;

pub const IMAGE_MODELS: &[&str] = &[
    "gpt-image-1",
    "gpt-image-1.5",
    "gpt-image-2",
    "gpt-image-2.5-flare",
    "gpt-image-2.5-sunburst",
    "gpt-image-2.5-flare-2026-09-08",
    "gpt-image-2.5-sunburst-2026-09-08",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 参照官方 Codex：已知支持原生 Images 的模型使用原生协议。
    Auto,
    /// 兼容旧图像服务时通过 Responses image_generation 工具执行。
    Responses,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub driver: String,
    pub mode: Mode,
    pub timeout_ms: u64,
    pub image_account_ids: Vec<String>,
    pub image_group_ids: Vec<String>,
    /// 旧 SDK 无法在 HTTP 中间件中读取已鉴权 Key，虚拟模型展示默认关闭。
    pub synthetic_catalog: bool,
}

impl Config {
    pub fn from_configuration(value: &Value) -> Result<Self, String> {
        let config = value
            .as_object()
            .ok_or("plugin configuration must be an object")?;
        let driver = match config.get("driverModel") {
            Some(Value::String(value)) => value.clone(),
            None => "gpt-5.6-luna".to_owned(),
            _ => return Err("driverModel must be a string".into()),
        };
        if driver.is_empty()
            || driver.len() > 128
            || driver
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
            || !driver.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/')
            })
        {
            return Err("driverModel is not a valid Responses model ID".into());
        }
        let mode = match config.get("bridgeMode").and_then(Value::as_str) {
            None | Some("auto") => Mode::Auto,
            Some("responses") => Mode::Responses,
            _ => return Err("bridgeMode must be auto or responses".into()),
        };
        let timeout_ms = match config.get("timeoutMs") {
            None => 300_000,
            Some(value) => value.as_u64().ok_or("timeoutMs must be an integer")?,
        };
        if !(30_000..=600_000).contains(&timeout_ms) {
            return Err("timeoutMs must be within 30000..600000".into());
        }
        let image_account_ids = id_list(config.get("imageAccountIds"), "imageAccountIds")?;
        let image_group_ids = id_list(config.get("imageGroupIds"), "imageGroupIds")?;
        let synthetic_catalog = match config.get("syntheticCatalog") {
            None | Some(Value::Bool(false)) => false,
            Some(Value::Bool(true)) => true,
            _ => return Err("syntheticCatalog must be a boolean".into()),
        };
        if config.keys().any(|key| {
            !matches!(
                key.as_str(),
                "driverModel"
                    | "bridgeMode"
                    | "timeoutMs"
                    | "imageAccountIds"
                    | "imageGroupIds"
                    | "syntheticCatalog"
            )
        }) {
            return Err("plugin configuration contains unknown fields".into());
        }
        Ok(Self {
            driver,
            mode,
            timeout_ms,
            image_account_ids,
            image_group_ids,
            synthetic_catalog,
        })
    }

    pub fn has_image_scope(&self) -> bool {
        !self.image_account_ids.is_empty() || !self.image_group_ids.is_empty()
    }

    pub fn is_image_model(model: &str) -> bool {
        IMAGE_MODELS.contains(&model)
    }

    pub fn use_responses(&self, model: &str) -> bool {
        if !Self::is_image_model(model) {
            return false;
        }
        if self.mode == Mode::Responses {
            return true;
        }
        // 官方 Codex imagegen 使用原生 Images；保留旧模型 Responses 兼容行为。
        !matches!(
            model,
            "gpt-image-1.5"
                | "gpt-image-2"
                | "gpt-image-2.5-flare"
                | "gpt-image-2.5-sunburst"
                | "gpt-image-2.5-flare-2026-09-08"
                | "gpt-image-2.5-sunburst-2026-09-08"
        )
    }
}

fn id_list(value: Option<&Value>, name: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?;
    if items.len() > 256 {
        return Err(format!("{name} cannot contain more than 256 IDs"));
    }
    let mut parsed = Vec::with_capacity(items.len());
    for item in items {
        let id = item
            .as_str()
            .ok_or_else(|| format!("{name} must contain only strings"))?;
        if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
            return Err(format!("{name} contains an invalid ID"));
        }
        if parsed.iter().any(|previous| previous == id) {
            return Err(format!("{name} contains a duplicate ID"));
        }
        parsed.push(id.to_owned());
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_mode_prefers_codex_native_images() {
        let cfg = Config::from_configuration(&serde_json::json!({})).unwrap();
        assert!(cfg.use_responses("gpt-image-1"));
        assert!(!cfg.use_responses("gpt-image-2"));
        assert!(!cfg.use_responses("random-chat-model"));
    }

    #[test]
    fn configured_driver_takes_effect() {
        let cfg = Config::from_configuration(&serde_json::json!({
            "driverModel":"gpt-6", "bridgeMode":"responses", "timeoutMs":480000
        }))
        .unwrap();
        assert_eq!(cfg.driver, "gpt-6");
        assert_eq!(cfg.mode, Mode::Responses);
        assert_eq!(cfg.timeout_ms, 480000);
        assert!(cfg.use_responses("gpt-image-2"));
    }

    #[test]
    fn image_pool_is_explicit_and_no_scope_defaults_fail_closed() {
        let empty = Config::from_configuration(&serde_json::json!({})).unwrap();
        assert!(!empty.has_image_scope());
        assert!(!empty.synthetic_catalog);
        let chosen = Config::from_configuration(&serde_json::json!({
            "imageAccountIds":["acc-A","acc-B"],"imageGroupIds":["grp-images"]
        }))
        .unwrap();
        assert!(chosen.has_image_scope());
        assert_eq!(chosen.image_account_ids, vec!["acc-A", "acc-B"]);
    }

    #[test]
    fn rejects_invalid_configuration() {
        for value in [
            serde_json::json!({"driverModel":"../../bad"}),
            serde_json::json!({"driverModel":""}),
            serde_json::json!({"timeoutMs":100}),
            serde_json::json!({"timeoutMs":null}),
            serde_json::json!({"bridgeMode":"unknown"}),
            serde_json::json!({"imageGroupIds":"wrong"}),
            serde_json::json!({"imageAccountIds":["same","same"]}),
            serde_json::json!({"syntheticCatalog":"true"}),
            serde_json::json!({"unexpected":true}),
        ] {
            assert!(Config::from_configuration(&value).is_err(), "{value}");
        }
    }
}
