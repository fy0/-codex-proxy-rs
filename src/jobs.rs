//! 旧后台图片任务的历史兼容层，只读取状态与已落盘图片，不再发起模型请求。
//! 保留 image-jobs v1 数据，允许取消旧记录，并恢复已完成原子落盘的结果。
use crate::config::Config;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::host::{self, StateGetRequest, StateGetResult, StatePutRequest, StatePutResult},
    client::{HostClient, SessionError},
};
use image::{ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::Mutex,
    time::Instant,
};
use uuid::Uuid;

const NAMESPACE: &str = "image-jobs";
const STATE_KEY: &str = "queue";
const CHUNK_BYTES: usize = 256 * 1024;
const MAX_IMAGE_BYTES: usize = 120 * 1024 * 1024;
const MAX_RECOVERY_CHECKS: usize = 4;
const RECOVERY_BUDGET: Duration = Duration::from_secs(2);

fn invalid(message: &str) -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, message)
}

fn fault(message: &str) -> PluginFault {
    PluginFault::new(ErrorCode::Fault, message)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 以原始 JSON 对象保留每条任务和诊断中的未知字段；不补写默认字段或清空旧数据。
#[derive(Default, Serialize, Deserialize)]
struct JobQueue {
    jobs: Vec<Map<String, Value>>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

pub struct BackendJobs {
    config: Arc<Config>,
    // 同时串行化状态 CAS 写入并保存轮转位置；它不代表后台执行器。
    recovery_cursor: Mutex<usize>,
}

impl BackendJobs {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            recovery_cursor: Mutex::new(0),
        }
    }

    pub const fn enabled(&self) -> bool {
        false
    }

    fn storage_path(&self) -> Result<PathBuf, PluginFault> {
        let path = self
            .config
            .legacy_image_storage_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("cpr-image-bridge").join("images"));
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            || path == Path::new("/")
        {
            return Err(invalid("invalid image history directory"));
        }
        Ok(path)
    }

    fn result_path(&self, id: &str) -> Result<PathBuf, PluginFault> {
        let id = Uuid::parse_str(id).map_err(|_| invalid("invalid job ID"))?;
        Ok(self.storage_path()?.join(format!("{id}.img")))
    }

    async fn existing_result_path(&self, id: &str) -> Result<PathBuf, PluginFault> {
        let current = self.result_path(id)?;
        let legacy = current.with_extension("png");
        for path in [&current, &legacy] {
            match tokio::fs::metadata(path).await {
                Ok(metadata) if metadata.is_file() => return Ok(path.clone()),
                Ok(_) => return Err(fault("image history path is not a file")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(fault("cannot read image history file metadata")),
            }
        }
        Err(fault("image history file missing"))
    }

    async fn load(&self, host: &HostClient) -> Result<(JobQueue, Option<u64>), PluginFault> {
        let reply = host
            .call(
                host::STATE_GET,
                serde_json::to_value(StateGetRequest {
                    namespace: NAMESPACE.into(),
                    key: STATE_KEY.into(),
                })
                .map_err(|_| fault("encode image history state query"))?,
                Vec::new(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        if !reply.payload.is_empty() {
            return Err(fault("unexpected image history state payload"));
        }
        let answer: StateGetResult = serde_json::from_value(reply.result)
            .map_err(|_| fault("invalid image history state response"))?;
        match answer.record {
            Some(record) => {
                if record.schema_version != 1 {
                    return Err(fault("unsupported image history state schema"));
                }
                let queue = serde_json::from_value(record.value)
                    .map_err(|_| fault("invalid image history queue record"))?;
                Ok((queue, Some(record.version)))
            }
            // 只有宿主明确报告记录不存在，才显示空历史；错误或损坏数据绝不按空队列处理。
            None => Ok((JobQueue::default(), None)),
        }
    }

    async fn save(
        &self,
        host: &HostClient,
        queue: &JobQueue,
        version: Option<u64>,
    ) -> Result<(), PluginFault> {
        let reply = host
            .call(
                host::STATE_PUT,
                serde_json::to_value(StatePutRequest {
                    namespace: NAMESPACE.into(),
                    key: STATE_KEY.into(),
                    expected_version: version,
                    value: serde_json::to_value(queue)
                        .map_err(|_| fault("encode image history queue"))?,
                })
                .map_err(|_| fault("encode image history state update"))?,
                Vec::new(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        if !reply.payload.is_empty() {
            return Err(fault("unexpected image history save payload"));
        }
        let _: StatePutResult = serde_json::from_value(reply.result)
            .map_err(|_| fault("invalid image history save response"))?;
        Ok(())
    }

    async fn committed_result(
        &self,
        job: &Map<String, Value>,
    ) -> Option<(u64, &'static str, u32, u32)> {
        let id = job.get("id")?.as_str()?;
        let requested_size = job.get("size")?.as_str()?;
        let path = self.existing_result_path(id).await.ok()?;
        let file = tokio::fs::File::open(path).await.ok()?;
        let length = file.metadata().await.ok()?.len();
        if !(16..=MAX_IMAGE_BYTES as u64).contains(&length) {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_IMAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        let (mime, width, height) = inspect_image(&bytes, requested_size).ok()?;
        Some((bytes.len() as u64, mime, width, height))
    }

    async fn recover_committed(&self, queue: &mut JobQueue, cursor: &mut usize) -> bool {
        let count = queue.jobs.len();
        if count == 0 {
            return false;
        }
        let deadline = Instant::now() + RECOVERY_BUDGET;
        let mut checks = 0;
        let mut changed = false;
        for _ in 0..count {
            if checks >= MAX_RECOVERY_CHECKS || Instant::now() >= deadline {
                break;
            }
            let index = *cursor % count;
            *cursor = (index + 1) % count;
            let job = &mut queue.jobs[index];
            if job.get("status").and_then(Value::as_str) != Some("running") {
                continue;
            }
            checks += 1;
            let result = tokio::time::timeout_at(deadline, self.committed_result(job)).await;
            let Ok(Some((bytes, mime, width, height))) = result else {
                // 未完成、文件缺失或损坏时保留原状态，不推断失败、不重新生成。
                continue;
            };
            job.insert("status".into(), json!("succeeded"));
            job.insert("completedMs".into(), json!(now_ms()));
            job.insert("resultBytes".into(), json!(bytes));
            job.insert("resultMime".into(), json!(mime));
            job.insert("actualWidth".into(), json!(width));
            job.insert("actualHeight".into(), json!(height));
            job.insert("error".into(), Value::Null);
            changed = true;
        }
        changed
    }

    pub async fn list(&self, host: &HostClient) -> Result<Value, PluginFault> {
        let mut cursor = self.recovery_cursor.lock().await;
        let (mut queue, version) = self.load(host).await?;
        if self.recover_committed(&mut queue, &mut cursor).await {
            self.save(host, &queue, version).await?;
        }
        Ok(json!({
            "jobs": queue.jobs,
            "legacyOnly": true,
            "backendEnabled": self.enabled(),
            "workerActive": false,
            "executionTransport": "legacy_archive",
            "pluginVersion": env!("CARGO_PKG_VERSION"),
        }))
    }

    pub async fn cancel(&self, host: &HostClient, input: &Value) -> Result<Value, PluginFault> {
        let id = input
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing job ID"))?;
        Uuid::parse_str(id).map_err(|_| invalid("invalid job ID"))?;
        let _guard = self.recovery_cursor.lock().await;
        let (mut queue, version) = self.load(host).await?;
        let job = queue
            .jobs
            .iter_mut()
            .find(|job| job.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| invalid("unknown historical job"))?;
        let current = job
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| fault("historical job has no status"))?
            .to_owned();
        let changed = matches!(current.as_str(), "queued" | "running");
        let status = if changed {
            job.insert("status".into(), json!("canceled"));
            job.insert("completedMs".into(), json!(now_ms()));
            "canceled".to_owned()
        } else {
            current.to_owned()
        };
        // 这里只取消历史记录，不声称停止旧上游请求，也不删除任务或已保存图片。
        if changed {
            self.save(host, &queue, version).await?;
        }
        Ok(json!({"id":id,"status":status,"legacyOnly":true}))
    }

    pub async fn image_chunk(
        &self,
        host: &HostClient,
        input: &Value,
    ) -> Result<(Vec<u8>, bool, u64), PluginFault> {
        let id = input
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing job ID"))?;
        Uuid::parse_str(id).map_err(|_| invalid("invalid job ID"))?;
        let offset = match input.get("offset") {
            None => 0,
            Some(value) => value
                .as_u64()
                .ok_or_else(|| invalid("invalid image offset"))?,
        };
        let (queue, _) = self.load(host).await?;
        let job = queue
            .jobs
            .iter()
            .find(|job| job.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| invalid("unknown historical job"))?;
        if job.get("status").and_then(Value::as_str) != Some("succeeded") {
            return Err(invalid("historical image is not ready"));
        }
        let path = self.existing_result_path(id).await?;
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|_| fault("historical image file missing"))?;
        let size = file
            .metadata()
            .await
            .map_err(|_| fault("historical image metadata unavailable"))?
            .len();
        if size > MAX_IMAGE_BYTES as u64 || offset > size {
            return Err(invalid("invalid historical image offset or size"));
        }
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|_| fault("historical image seek failed"))?;
        let mut bytes = vec![0; CHUNK_BYTES.min((size - offset) as usize)];
        file.read_exact(&mut bytes)
            .await
            .map_err(|_| fault("historical image read failed"))?;
        let end = offset + bytes.len() as u64 == size;
        Ok((bytes, end, size))
    }
}

fn inspect_image(
    bytes: &[u8],
    requested_size: &str,
) -> Result<(&'static str, u32, u32), PluginFault> {
    if !(16..=MAX_IMAGE_BYTES).contains(&bytes.len()) {
        return Err(invalid("historical image size outside allowed range"));
    }
    let format =
        image::guess_format(bytes).map_err(|_| invalid("historical image has unknown format"))?;
    let mime = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        _ => return Err(invalid("unsupported historical image format")),
    };
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| invalid("historical image dimensions unavailable"))?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 12_000_000 {
        return Err(invalid("historical image dimensions exceed limit"));
    }
    if requested_size != "auto" {
        let expected = requested_size
            .split_once('x')
            .and_then(|(width, height)| {
                Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?))
            })
            .ok_or_else(|| invalid("invalid historical image size"))?;
        if (width, height) != expected {
            return Err(invalid(
                "historical image dimensions do not match requested size",
            ));
        }
    }
    // 仅有合法文件头不算完整落盘，恢复前验证图片主体能够解码。
    image::load_from_memory_with_format(bytes, format)
        .map_err(|_| invalid("historical image contents are incomplete"))?;
    Ok((mime, width, height))
}

#[cfg(test)]
#[path = "jobs/integration_tests.rs"]
mod integration_tests;
