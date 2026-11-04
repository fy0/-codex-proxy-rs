//! Lightweight in-process Rust ModelTrace engine.
//! The old implementation unpacked and spawned a private CPython/NumPy distribution.
//! No interpreter, dynamic linker or executable temporary files are required.

use crate::{host::fault, native_score};
use gateway_plugin_sdk::PluginFault;
use serde::{Serialize, de::DeserializeOwned};
use std::path::PathBuf;

pub struct Engine {
    directory: Option<tempfile::TempDir>,
}
impl Engine {
    pub fn new() -> Self {
        Self { directory: None }
    }
    pub fn cleanup(&mut self) -> Result<(), PluginFault> {
        if let Some(directory) = self.directory.take() {
            directory.close().map_err(|_| fault("评分缓存临时目录清理失败"))?;
        }
        Ok(())
    }
    pub fn prepare(&mut self) -> Result<(), PluginFault> {
        // No native runtime to unpack. Keep this for the existing manager API.
        Ok(())
    }
    pub async fn write_bank(&mut self, hash: &str, data: &[u8]) -> Result<PathBuf, PluginFault> {
        if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(fault("指纹库摘要无效"));
        }
        if data.len() > 20 * 1024 * 1024 {
            return Err(fault("指纹库超过20MiB"));
        }
        if self.directory.is_none() {
            self.directory = Some(
                tempfile::Builder::new().prefix("modeltrace-bank-").tempdir()
                    .map_err(|_| fault("指纹库缓存目录创建失败"))?
            );
        }
        let path = self.directory.as_ref().unwrap().path().join(format!("bank-{hash}.json"));
        if !path.is_file() {
            std::fs::write(&path, data).map_err(|_| fault("指纹库缓存写入失败"))?;
        }
        Ok(path)
    }
    pub async fn call<I: Serialize, O: DeserializeOwned>(
        &mut self,
        input: I,
    ) -> Result<O, PluginFault> {
        let request = serde_json::to_value(input).map_err(|_| fault("评分输入编码失败"))?;
        // Each batch is pure numeric work; do not block plugin protocol processing.
        let response = tokio::task::spawn_blocking(move || native_score::handle(request))
            .await
            .map_err(|_| fault("Rust 评分任务异常终止"))?
            .map_err(fault)?;
        serde_json::from_value(response).map_err(|_| fault("Rust 评分输出格式错误"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn native_engine_needs_no_python_runtime() {
        let mut engine = Engine::new();
        engine.prepare().unwrap();
        let challenges: serde_json::Value = engine.call(json!({"op":"challenges"})).await.unwrap();
        assert_eq!(challenges.as_array().unwrap().len(), 1);
        engine.cleanup().unwrap();
    }
}
