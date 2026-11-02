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
    /// 和 Sub2API 一样：已知支持 Codex 原生 Images 的模型不做二次转换。
    Auto,
    /// 所有列出的图片模型都通过 Responses image_generation 工具执行。
    Responses,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub driver: String,
    pub mode: Mode,
    pub timeout_ms: u64,
}

impl Config {
    /// 读取宿主在本次插件会话的握手里冻结的普通配置；配置切换会启动新会话。
    /// 不依赖进程环境变量，以免管理端设置只显示而不生效。
    pub fn from_configuration(value: &Value) -> Result<Self, String> {
        let config = value
            .as_object()
            .ok_or("plugin configuration must be an object")?;
        // 仅在宿主未填充 schema 默认值时回退；不忽略已配置的非法字段。
        let driver = match config.get("driverModel") {
            Some(Value::String(value)) => value.clone(),
            None => "gpt-5.6-luna".to_owned(),
            _ => return Err("driverModel must be a string".into()),
        };
        if driver.is_empty()
            || driver.len() > 128
            // 模型 ID 允许 provider/model，但禁止路径样式的空段和相对段。
            || driver.split('/').any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
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
        // 避免文档声明与运行行为分歧，遇到未知字段立即失败。
        if config
            .keys()
            .any(|key| !matches!(key.as_str(), "driverModel" | "bridgeMode" | "timeoutMs"))
        {
            return Err("plugin configuration contains unknown fields".into());
        }
        Ok(Self {
            driver,
            mode,
            timeout_ms,
        })
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
        // Sub2API 将这些模型走 Codex 原生 Images API，而不是桥接工具。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_mode_prefers_direct_when_codex_has_native_images() {
        let cfg = Config {
            driver: "gpt-5.6-luna".into(),
            mode: Mode::Auto,
            timeout_ms: 300_000,
        };
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
    fn missing_values_use_schema_defaults() {
        let cfg = Config::from_configuration(&serde_json::json!({})).unwrap();
        assert_eq!(cfg.driver, "gpt-5.6-luna");
        assert_eq!(cfg.mode, Mode::Auto);
        assert_eq!(cfg.timeout_ms, 300000);
    }

    #[test]
    fn invalid_driver_and_timeouts_fail_closed() {
        for value in [
            serde_json::json!({"driverModel":"../../bad"}),
            serde_json::json!({"driverModel":""}),
            serde_json::json!({"timeoutMs":100}),
            serde_json::json!({"timeoutMs":null}),
            serde_json::json!({"bridgeMode":"unknown"}),
            serde_json::json!({"unexpected":true}),
        ] {
            assert!(Config::from_configuration(&value).is_err(), "{value}");
        }
    }
}
