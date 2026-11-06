//! 独立于管理页面连接的持久生图任务。状态保存在 host.state，PNG 写入管理员指定持久卷。
//! maintenance 每 30 秒对队列进行串行认领；网页关闭只终止轮询，不终止后端执行。
use crate::{access, config::Config, quota_guard};
use base64::Engine as _;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::AccessQuery,
        host::{
            self, ModelEventBatch, ModelExecuteRequest, ModelOperation, ModelStreamReadRequest,
            ModelStreamReadResult, ModelStreamResult, StateGetRequest, StateGetResult,
            StatePutRequest, StatePutResult,
        },
        model::WirePayload,
    },
    client::{HostClient, SessionError},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::Mutex,
};
use uuid::Uuid;

const NAMESPACE: &str = "image-jobs";
const STATE_KEY: &str = "queue";
const MAX_PENDING: usize = 30;
const CHUNK_BYTES: usize = 256 * 1024;
const MAX_PNG_BYTES: usize = 120 * 1024 * 1024;

fn invalid(msg: &str) -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, msg)
}
fn fault(msg: &str) -> PluginFault {
    PluginFault::new(ErrorCode::Fault, msg)
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageJob {
    pub id: String,
    pub client_key_id: String,
    pub image_model: String,
    pub prompt: String,
    pub size: String,
    pub quality: String,
    pub status: String,
    pub created_ms: u64,
    pub started_ms: Option<u64>,
    pub completed_ms: Option<u64>,
    pub request_id: Option<String>,
    pub account_id: Option<String>,
    pub result_bytes: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct JobQueue {
    pub jobs: Vec<ImageJob>,
}

impl ImageJob {
    fn new(
        client_key_id: String,
        image_model: String,
        prompt: String,
        size: String,
        quality: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            client_key_id,
            image_model,
            prompt,
            size,
            quality,
            status: "queued".to_owned(),
            created_ms: now_ms(),
            started_ms: None,
            completed_ms: None,
            request_id: None,
            account_id: None,
            result_bytes: None,
            error: None,
        }
    }
}
pub struct BackendJobs {
    config: Arc<Config>,
    lock: Mutex<()>,
    next_account: AtomicUsize,
}

impl BackendJobs {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            lock: Mutex::new(()),
            next_account: AtomicUsize::new(0),
        }
    }
    pub fn enabled(&self) -> bool {
        !self.config.image_storage_dir.is_empty()
    }

    fn storage_path(&self) -> Result<PathBuf, PluginFault> {
        if !self.enabled() {
            return Err(invalid(
                "Backend image jobs require imageStorageDir (a persistent absolute directory)",
            ));
        }
        let path = PathBuf::from(&self.config.image_storage_dir);
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            || path == Path::new("/")
        {
            return Err(invalid("invalid imageStorageDir"));
        }
        Ok(path)
    }
    async fn ensure_storage(&self) -> Result<PathBuf, PluginFault> {
        let path = self.storage_path()?;
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|_| fault("cannot create imageStorageDir"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // 显式配置的目录可能由管理员预先挂载。缩小访问权而不扩展访问权。
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|_| fault("cannot restrict imageStorageDir permissions"))?;
        }
        Ok(path)
    }
    fn result_path(&self, id: &str) -> Result<PathBuf, PluginFault> {
        let job = Uuid::parse_str(id).map_err(|_| invalid("invalid job ID"))?;
        Ok(self.storage_path()?.join(format!("{job}.png")))
    }
    async fn load(&self, host: &HostClient) -> Result<(JobQueue, Option<u64>), PluginFault> {
        let reply = host
            .call(
                host::STATE_GET,
                serde_json::to_value(StateGetRequest {
                    namespace: NAMESPACE.into(),
                    key: STATE_KEY.into(),
                })
                .map_err(|_| fault("encode state get"))?,
                Vec::new(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let answer: StateGetResult =
            serde_json::from_value(reply.result).map_err(|_| fault("invalid image job state"))?;
        match answer.record {
            Some(record) => Ok((
                serde_json::from_value(record.value)
                    .map_err(|_| fault("invalid image queue record"))?,
                Some(record.version),
            )),
            None => Ok((JobQueue::default(), None)),
        }
    }
    async fn save(
        &self,
        host: &HostClient,
        queue: &JobQueue,
        version: Option<u64>,
    ) -> Result<u64, PluginFault> {
        let reply = host
            .call(
                host::STATE_PUT,
                serde_json::to_value(StatePutRequest {
                    namespace: NAMESPACE.into(),
                    key: STATE_KEY.into(),
                    expected_version: version,
                    value: serde_json::to_value(queue).map_err(|_| fault("encode queue"))?,
                })
                .map_err(|_| fault("encode state put"))?,
                Vec::new(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let answer: StatePutResult = serde_json::from_value(reply.result)
            .map_err(|_| fault("invalid image queue save result"))?;
        Ok(answer.version)
    }
    pub async fn list(&self, host: &HostClient) -> Result<Value, PluginFault> {
        let (queue, _) = self.load(host).await?;
        Ok(json!({"jobs":queue.jobs,"backendEnabled":self.enabled()}))
    }

    pub async fn submit(&self, host: &HostClient, input: &Value) -> Result<Value, PluginFault> {
        self.ensure_storage().await?;
        let fields = input
            .as_object()
            .ok_or_else(|| invalid("invalid job request"))?;
        let str_field = |field: &str, max: usize| -> Result<String, PluginFault> {
            let value = fields
                .get(field)
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing job field"))?
                .trim();
            if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
                return Err(invalid("invalid job field"));
            }
            Ok(value.to_owned())
        };
        let key = str_field("clientKeyId", 256)?;
        let model = str_field("imageModel", 128)?;
        let prompt = str_field("prompt", 4096)?;
        let size = str_field("size", 32)?;
        let quality = str_field("quality", 16)?;
        let count = fields.get("count").and_then(Value::as_u64).unwrap_or(1);
        if !(1..=10).contains(&count)
            || ![
                "auto",
                "1024x1024",
                "1536x1024",
                "1024x1536",
                "2048x2048",
                "2048x1152",
                "1152x2048",
                "3840x2160",
                "2160x3840",
            ]
            .contains(&size.as_str())
            || !["auto", "low", "medium", "high", "xhigh", "max"].contains(&quality.as_str())
            || !self.config.enabled_image_model(&model)
        {
            return Err(invalid("unsupported image job parameters"));
        }
        // 先从插件官方 ACL 按 Key 与允许账号范围计算交集。不能只信任管理页的模型下拉框。
        if !access::allowed_models_for_key(host, &self.config, &key)
            .await?
            .contains(&model)
        {
            return Err(PluginFault::new(
                ErrorCode::PermissionDenied,
                "client key has no eligible image generation account",
            ));
        }
        let _guard = self.lock.lock().await;
        let (mut queue, version) = self.load(host).await?;
        let new_count = count as usize;
        if queue
            .jobs
            .iter()
            .filter(|job| job.status == "queued" || job.status == "running")
            .count()
            + new_count
            > MAX_PENDING
        {
            return Err(PluginFault::new(ErrorCode::Capacity, "image queue is full"));
        }
        // 仅逐出已结束、状态保留时间最长的任务，且不删除仍需要的结果文件。
        let mut dropped = Vec::new();
        while queue.jobs.len() + new_count > MAX_PENDING {
            let Some(index) = queue
                .jobs
                .iter()
                .position(|j| j.status != "queued" && j.status != "running")
            else {
                break;
            };
            dropped.push(queue.jobs.remove(index).id);
        }
        let mut ids = Vec::new();
        for _ in 0..new_count {
            let job = ImageJob::new(
                key.clone(),
                model.clone(),
                prompt.clone(),
                size.clone(),
                quality.clone(),
            );
            ids.push(job.id.clone());
            queue.jobs.push(job);
        }
        self.save(host, &queue, version).await?;
        for id in dropped {
            if let Ok(path) = self.result_path(&id) {
                let _ = tokio::fs::remove_file(path).await;
            }
        }
        Ok(json!({"ids":ids,"status":"queued","pollAfterMs":1500}))
    }

    pub async fn cancel(&self, host: &HostClient, input: &Value) -> Result<Value, PluginFault> {
        let id = input
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing job id"))?;
        Uuid::parse_str(id).map_err(|_| invalid("bad job id"))?;
        let _guard = self.lock.lock().await;
        let (mut queue, version) = self.load(host).await?;
        let job = queue
            .jobs
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or_else(|| invalid("unknown job"))?;
        if job.status == "queued" {
            job.status = "canceled".into();
            job.completed_ms = Some(now_ms());
        } else if job.status == "running" {
            // 请求已经由维护父调用启动，显式标记取消；执行完成后丢弃结果。
            // 后台不会因浏览器断开自动取消。
            job.status = "canceled".into();
            job.completed_ms = Some(now_ms());
        }
        self.save(host, &queue, version).await?;
        Ok(json!({"id":id,"status":job_status(&queue,id)}))
    }
    pub async fn image_chunk(
        &self,
        host: &HostClient,
        input: &Value,
    ) -> Result<(Vec<u8>, bool, u64), PluginFault> {
        let id = input
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing job id"))?;
        let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let (queue, _) = self.load(host).await?;
        let job = queue
            .jobs
            .iter()
            .find(|j| j.id == id)
            .ok_or_else(|| invalid("unknown job"))?;
        if job.status != "succeeded" {
            return Err(invalid("image not ready"));
        }
        let path = self.result_path(id)?;
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|_| fault("image file missing"))?;
        use tokio::io::AsyncSeekExt;
        let size = file
            .metadata()
            .await
            .map_err(|_| fault("image stat failed"))?
            .len();
        if size > MAX_PNG_BYTES as u64 || offset > size {
            return Err(invalid("invalid image offset or size"));
        }
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|_| fault("image seek failed"))?;
        let mut chunk = vec![0; CHUNK_BYTES.min((size - offset) as usize)];
        file.read_exact(&mut chunk)
            .await
            .map_err(|_| fault("image read failed"))?;
        let next = offset + chunk.len() as u64;
        Ok((chunk, next == size, size))
    }
}
fn job_status(queue: &JobQueue, id: &str) -> String {
    queue
        .jobs
        .iter()
        .find(|j| j.id == id)
        .map(|j| j.status.clone())
        .unwrap_or_default()
}

impl BackendJobs {
    /// 宿主独立维护回调领取任务，而不是复用浏览器管理请求的父调用。
    pub async fn reconcile(&self, host: &HostClient) -> Result<(), PluginFault> {
        if !self.enabled() {
            return Ok(());
        }
        self.ensure_storage().await?;
        let active = {
            let _guard = self.lock.lock().await;
            let (mut queue, version) = self.load(host).await?;
            let mut dirty = false;
            // 上次维护中断后不能盲目重试：上游可能已受理并计费。
            // 文件若已原子写入，说明结果实际上完成，直接恢复而不是误报中断。
            for job in &mut queue.jobs {
                if job.status != "running" {
                    continue;
                }
                let recovered = match self.result_path(&job.id) {
                    Ok(path) => match tokio::fs::metadata(&path).await {
                        Ok(meta) if (8..=MAX_PNG_BYTES as u64).contains(&meta.len()) => {
                            match tokio::fs::File::open(&path).await {
                                Ok(mut file) => {
                                    let mut header = [0_u8; 8];
                                    if file.read_exact(&mut header).await.is_ok()
                                        && header == *b"\x89PNG\r\n\x1a\n"
                                    {
                                        Some(meta.len())
                                    } else {
                                        None
                                    }
                                }
                                Err(_) => None,
                            }
                        }
                        _ => None,
                    },
                    Err(_) => None,
                };
                job.completed_ms = Some(now_ms());
                if let Some(bytes) = recovered {
                    job.status = "succeeded".into();
                    job.result_bytes = Some(bytes);
                    job.error = None;
                } else {
                    job.status = "interrupted".into();
                    job.error=Some("backend worker interrupted; manual retry required to avoid duplicate billing".into());
                }
                dirty = true;
            }
            let mut started = Vec::new();
            for job in queue
                .jobs
                .iter_mut()
                .filter(|j| j.status == "queued")
                .take(self.config.background_concurrency)
            {
                job.status = "running".into();
                job.started_ms = Some(now_ms());
                started.push(job.clone());
                dirty = true;
            }
            if dirty {
                self.save(host, &queue, version).await?;
            }
            started
        };
        // 几个请求属于维护父调用；即使前端断连，宿主也不会取消此父调用。
        let outcomes = futures_util::future::join_all(active.into_iter().map(|job| async move {
            let id = job.id.clone();
            let done = self.execute(host, &job).await;
            (id, done)
        }))
        .await;
        for (id, result) in outcomes {
            let _guard = self.lock.lock().await;
            let (mut queue, version) = self.load(host).await?;
            let Some(job) = queue.jobs.iter_mut().find(|j| j.id == id) else {
                continue;
            };
            if job.status == "canceled" {
                // 用户主动停止的结果不能再交付。
                if let Ok(path) = self.result_path(&id) {
                    let _ = tokio::fs::remove_file(path).await;
                }
                continue;
            }
            if job.status != "running" {
                continue;
            }
            job.completed_ms = Some(now_ms());
            match result {
                Ok((account_id, request_id, bytes)) => {
                    job.status = "succeeded".into();
                    job.account_id = Some(account_id);
                    job.request_id = Some(request_id);
                    job.result_bytes = Some(bytes);
                    job.error = None;
                }
                Err(e) => {
                    job.status = "failed".into();
                    // 不把账号私有错误正文或 Token 写入公共管理状态。
                    job.error = Some(format!("backend image job failed: {:?}", e.code));
                }
            }
            self.save(host, &queue, version).await?;
        }
        Ok(())
    }

    async fn choose_account(
        &self,
        host: &HostClient,
        job: &ImageJob,
    ) -> Result<String, PluginFault> {
        let ids = access::account_ids(host, &self.config).await?;
        if ids.is_empty() {
            return Err(PluginFault::new(
                ErrorCode::Rejected,
                "no eligible image accounts",
            ));
        }
        let decision = host
            .check_access(AccessQuery {
                client_key_id: job.client_key_id.clone(),
                account_ids: ids,
                upstream_model: None,
            })
            .await?;
        let mut eligible = decision
            .accounts
            .into_iter()
            .filter(|account| account.allowed)
            .map(|account| account.account_id)
            .collect::<Vec<_>>();
        // Responses 桥接必须确认同一账号也允许对应的对话主控模型。
        if self.config.use_responses(&job.image_model) && !eligible.is_empty() {
            let checked = host
                .check_access(AccessQuery {
                    client_key_id: job.client_key_id.clone(),
                    account_ids: eligible.clone(),
                    upstream_model: Some(self.config.driver.clone()),
                })
                .await?;
            let approved = checked
                .accounts
                .into_iter()
                .filter(|account| account.allowed)
                .map(|account| account.account_id)
                .collect::<std::collections::HashSet<_>>();
            eligible.retain(|id| approved.contains(id));
        }
        eligible.sort();
        let blocked = quota_guard::protected_ids(host, &eligible, &self.config.quota_guard).await?;
        eligible.retain(|id| !blocked.contains(id));
        if eligible.is_empty() {
            return Err(PluginFault::new(
                ErrorCode::Rejected,
                "no quota-safe image accounts",
            ));
        }
        // 管理调用启动的模型子请求会跳过发起者插件的 scheduler，
        // 因此必须显式传递经过 Key+生图白名单+额度保护筛选的账号 ID。
        let position = self.next_account.fetch_add(1, Ordering::AcqRel);
        Ok(eligible[position % eligible.len()].clone())
    }

    async fn execute(
        &self,
        host: &HostClient,
        job: &ImageJob,
    ) -> Result<(String, String, u64), PluginFault> {
        let account = self.choose_account(host, job).await?;
        let via_responses = self.config.use_responses(&job.image_model);
        let request = ModelExecuteRequest {
            client_key_id: Some(job.client_key_id.clone()),
            model: if via_responses {
                self.config.driver.clone()
            } else {
                job.image_model.clone()
            },
            protocol: "openai".into(),
            operation: if via_responses {
                ModelOperation::Generate
            } else {
                ModelOperation::GenerateImage
            },
            provider: Some("openai".into()),
            account_id: Some(account.clone()),
            previous_response_id: None,
        };
        let request_body = if via_responses {
            json!({
                "model":self.config.driver,
                "input":[{"role":"user","content":[{"type":"input_text","text":job.prompt}]}],
                "tools":[{
                    "type":"image_generation",
                    "model":job.image_model,
                    "size":job.size,
                    "quality":job.quality,
                    "action":"generate"
                }],
                "tool_choice":"auto",
                "store":false,
                "stream":false
            })
        } else {
            json!({
                "model":job.image_model,
                "prompt":job.prompt,
                "size":job.size,
                "quality":job.quality,
                "n":1
            })
        };
        let body =
            serde_json::to_vec(&request_body).map_err(|_| fault("encode image model request"))?;
        let reply = host
            .call(
                host::MODEL_EXECUTE_STREAM,
                serde_json::to_value(request).map_err(|_| fault("encode model execution"))?,
                body,
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let stream: ModelStreamResult = serde_json::from_value(reply.result)
            .map_err(|_| fault("invalid model stream response"))?;
        let request_id = stream.request_id.clone();
        let mut buffer = Vec::new();
        let mut image: Option<Vec<u8>> = None;
        let mut iteration = 0;
        loop {
            iteration += 1;
            if iteration > 10_000 {
                return Err(PluginFault::new(
                    ErrorCode::Capacity,
                    "too many model output chunks",
                ));
            }
            let reply = host
                .call(
                    host::MODEL_STREAM_READ,
                    serde_json::to_value(ModelStreamReadRequest {
                        stream: stream.stream.clone(),
                        maximum_bytes: (8 * 1024 * 1024) as u32,
                    })
                    .map_err(|_| fault("encode stream read"))?,
                    Vec::new(),
                )
                .await
                .map_err(SessionError::into_plugin_fault)?;
            let state: ModelStreamReadResult =
                serde_json::from_value(reply.result).map_err(|_| fault("invalid stream read"))?;
            if !reply.payload.is_empty() {
                let batch = ModelEventBatch::decode(&reply.payload)
                    .map_err(|_| fault("invalid model event batch"))?;
                for event in batch.events {
                    if let Some(wire) = event.wire {
                        match wire.payload {
                            WirePayload::RawJson { body } => {
                                let data: Value = serde_json::from_slice(&body)
                                    .map_err(|_| fault("invalid image response JSON"))?;
                                if let Some(b64) = extract_base64(&data) {
                                    image = Some(decode_png(b64)?);
                                }
                            }
                            WirePayload::Json { data, .. } => {
                                if let Some(b64) = extract_base64(&data) {
                                    image = Some(decode_png(b64)?);
                                }
                            }
                            WirePayload::RawBody { body } => {
                                if buffer.len().saturating_add(body.len()) > MAX_PNG_BYTES * 2 {
                                    return Err(PluginFault::new(
                                        ErrorCode::Capacity,
                                        "model response too large",
                                    ));
                                }
                                buffer.extend_from_slice(&body);
                            }
                            _ => {}
                        }
                    }
                }
            }
            if state.end {
                break;
            }
        }
        let png = match image {
            Some(png) => png,
            None => {
                let data: Value = serde_json::from_slice(&buffer)
                    .map_err(|_| fault("image model did not return JSON image data"))?;
                decode_png(extract_base64(&data).ok_or_else(|| fault("no image in response"))?)?
            }
        };
        let path = self.result_path(&job.id)?;
        let staging = path.with_extension("part");
        tokio::fs::write(&staging, &png)
            .await
            .map_err(|_| fault("cannot write image result"))?;
        tokio::fs::rename(&staging, &path)
            .await
            .map_err(|_| fault("cannot commit image result"))?;
        Ok((account, request_id, png.len() as u64))
    }
}

// 只接受明确的图片生成响应格式，不递归读取非图片字段或日志。
fn extract_base64(data: &Value) -> Option<&str> {
    if let Some(value) = data.get("b64_json").and_then(Value::as_str) {
        return Some(value);
    }
    if let Some(value) = data.get("result").and_then(Value::as_str) {
        if data.get("type").and_then(Value::as_str) == Some("image_generation_call") {
            return Some(value);
        }
    }
    if let Some(items) = data.get("data").and_then(Value::as_array) {
        for item in items {
            if let Some(value) = extract_base64(item) {
                return Some(value);
            }
        }
    }
    if let Some(items) = data.get("output").and_then(Value::as_array) {
        for item in items {
            if let Some(value) = extract_base64(item) {
                return Some(value);
            }
        }
    }
    if let Some(response) = data.get("response") {
        if let Some(value) = extract_base64(response) {
            return Some(value);
        }
    }
    None
}
fn decode_png(text: &str) -> Result<Vec<u8>, PluginFault> {
    if text.len() > MAX_PNG_BYTES * 2 {
        return Err(PluginFault::new(
            ErrorCode::Capacity,
            "image result too large",
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| fault("invalid base64 image"))?;
    if bytes.len() < 8 || bytes.len() > MAX_PNG_BYTES || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err(invalid("upstream image is not PNG"));
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_payload_extracts_native_and_tool_results() {
        let native = json!({"data":[{"b64_json":"YWJj"}]});
        assert_eq!(extract_base64(&native), Some("YWJj"));
        let tool =
            json!({"response":{"output":[{"type":"image_generation_call","result":"ZGVm"}]}});
        assert_eq!(extract_base64(&tool), Some("ZGVm"));
        assert!(extract_base64(&json!({"error":"secret"})).is_none());
    }
    #[test]
    fn durable_job_ids_are_random_uuids_and_path_safe() {
        let j = ImageJob::new(
            "key".into(),
            "gpt-image-2.6-flare".into(),
            "prompt".into(),
            "1024x1024".into(),
            "high".into(),
        );
        assert!(Uuid::parse_str(&j.id).is_ok());
        assert_eq!(j.status, "queued");
    }
}
