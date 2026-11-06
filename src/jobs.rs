//! 独立于管理页面连接的异步生图任务。任务状态在 host.state，图片写入本地临时目录。
//! maintenance 每 30 秒对队列进行串行认领；网页关闭只终止轮询，不终止后端执行。
use crate::{access, config::Config, quota_guard};
use base64::Engine as _;
use futures_util::StreamExt;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault, SendState,
    call::{
        data::AccessQuery,
        host::{
            self, LogLevel, LogRequest, ModelEventBatch, ModelExecuteRequest, ModelOperation,
            ModelStreamCloseRequest, ModelStreamReadRequest, ModelStreamReadResult,
            ModelStreamResult, StateGetRequest, StateGetResult, StatePutRequest, StatePutResult,
        },
        model::WirePayload,
    },
    client::{HostClient, SessionError},
};
use image::{ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use std::{
    io::Cursor,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::{Mutex, Semaphore},
};
use uuid::Uuid;

const NAMESPACE: &str = "image-jobs";
const STATE_KEY: &str = "queue";
const MAX_PENDING: usize = 128;
// 给额度维护、状态写入和管理请求预留宿主回调容量，避免整池任务同时挤占 RPC。
const MAX_CONCURRENT_IMAGE_JOBS: usize = 16;
// 固定宿主每个父调用最多缓存 16 个 Key 身份，后续 Key 留给下一轮重新授权。
const MAX_KEYS_PER_MAINTENANCE: usize = 16;
const CHUNK_BYTES: usize = 256 * 1024;
const MAX_PNG_BYTES: usize = 120 * 1024 * 1024;
// 固定 SDK 对应宿主的默认回调预算是 1 MiB；事件编解码的 8 MiB 上限不是读取额度。
const MODEL_READ_BYTES: u32 = 1024 * 1024;

fn dispatch_capacity(accounts: usize, per_account: usize) -> usize {
    accounts
        .saturating_mul(per_account)
        .min(MAX_CONCURRENT_IMAGE_JOBS)
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_model: Option<String>,
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
    #[serde(default)]
    pub result_mime: Option<String>,
    #[serde(default)]
    pub actual_width: Option<u32>,
    #[serde(default)]
    pub actual_height: Option<u32>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<JobDiagnostic>,
}

/// 仅保存本地固定阶段、经过校验的请求参数和 SDK 元数据，不保存错误正文或账号信息。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDiagnostic {
    // 仅用于解析当前流，不进入诊断、日志或持久状态。
    #[serde(skip)]
    image_call_id: Option<String>,
    #[serde(default)]
    plugin_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    safe_reason: Option<String>,
    stage: String,
    operation: String,
    model_id: String,
    execution_model: String,
    size: String,
    output_format: String,
    maximum_read_bytes: u32,
    execution_accepted: bool,
    // null 表示无法确认；流读取错误的 NotSent 不能覆盖已开始执行的发送不确定性。
    upstream_sent: Option<bool>,
    request_id: Option<String>,
    sdk_error_code: Option<ErrorCode>,
    sdk_send_state: Option<SendState>,
    http_status: Option<u16>,
}

impl JobDiagnostic {
    fn new(job: &ImageJob, config: &Config) -> Self {
        Self {
            image_call_id: None,
            plugin_version: env!("CARGO_PKG_VERSION").into(),
            error_origin: None,
            safe_reason: None,
            stage: "account_facts".into(),
            operation: "generate".into(),
            model_id: job.image_model.clone(),
            execution_model: job.driver(config).to_owned(),
            size: job.size.clone(),
            output_format: preferred_upstream_format(&job.image_model).into(),
            maximum_read_bytes: MODEL_READ_BYTES,
            execution_accepted: false,
            upstream_sent: Some(false),
            request_id: None,
            sdk_error_code: None,
            sdk_send_state: None,
            http_status: None,
        }
    }

    fn record_session_error(&mut self, error: SessionError, execution_entry: bool) -> PluginFault {
        let remote = matches!(&error, SessionError::Remote(_));
        let error = error.into_plugin_fault();
        self.error_origin = Some(if remote { "host_remote" } else { "sdk_local" }.into());
        let reason = if !remote {
            "sdk_local_failure"
        } else if error.message == "callback input is invalid" {
            "callback_input_invalid"
        } else if error.message
            == "Image generation is not allowed by the API key\'s account groups."
        {
            "image_generation_disabled"
        } else {
            // 只匹配宿主定义的枚举值，不记录任意 message/details 或上游正文。
            let details = error.details.as_ref();
            let provider_kind = details
                .and_then(|v| v.pointer("/provider/kind"))
                .and_then(Value::as_str);
            let kind = details.and_then(|v| v.get("kind")).and_then(Value::as_str);
            match (provider_kind, kind) {
                (Some("invalid_request"), _) => "provider_invalid_request",
                (Some("message_too_big"), _) => "provider_message_too_big",
                (_, Some("invalid_request")) => "core_invalid_request",
                (_, Some("message_too_big")) => "core_message_too_big",
                _ => "host_error_unclassified",
            }
        };
        self.safe_reason = Some(reason.into());
        // 只有执行入口或被明确分类的执行器错误能够证明未发送。
        // 读取回调自身的 NotSent 只描述本次读取，不能覆盖此前的上游事实。
        if remote
            && error.send_state == SendState::NotSent
            && self.upstream_sent != Some(true)
            && (execution_entry
                || matches!(
                    reason,
                    "provider_invalid_request"
                        | "provider_message_too_big"
                        | "core_invalid_request"
                        | "core_message_too_big"
                ))
        {
            self.upstream_sent = Some(false);
        }
        self.record_failure(&error);
        error
    }

    fn record_failure(&mut self, error: &PluginFault) {
        self.sdk_error_code = Some(error.code);
        self.sdk_send_state = Some(error.send_state);
        if self.error_origin.is_none() {
            self.error_origin = Some("plugin_local".into());
        }
        if self.safe_reason.is_none() {
            self.safe_reason = Some(
                match error.message.as_str() {
                    "no eligible image accounts" => "no_image_accounts",
                    "no quota-safe image accounts" => "quota_accounts_exhausted",
                    _ => "plugin_stage_failure",
                }
                .into(),
            );
        }
        if error.http_status.is_some() {
            self.http_status = error.http_status;
        }
        // HTTP 状态也可能来自宿主输入校验，例如未发送上游的 413；只信任发送事实。
        if error.send_state == SendState::Sent {
            self.upstream_sent = Some(true);
        }
    }

    fn error_message(&self, error: &PluginFault) -> String {
        let sent = match self.upstream_sent {
            Some(true) => "true",
            Some(false) => "false",
            None => "unknown",
        };
        let mut message = format!(
            "backend image job failed: {:?}; stage={}; operation={}; model={}; size={}; output={}; request_id={}; upstream_sent={}",
            error.code,
            self.stage,
            self.operation,
            self.model_id,
            self.size,
            self.output_format,
            self.request_id.as_deref().unwrap_or("unavailable"),
            sent,
        );
        message.push_str(&format!(
            "; execution_model={}; reason={}; sdk_origin={}; sdk_send_state={:?}; plugin_version={}",
            self.execution_model,
            self.safe_reason.as_deref().unwrap_or("unclassified"),
            self.error_origin.as_deref().unwrap_or("unclassified"),
            error.send_state,
            self.plugin_version,
        ));
        // 此阶段的错误仅来自本地像素验证，其他阶段绝不复用任意上游错误正文。
        if self.stage == "image_validation" && error.code == ErrorCode::Rejected {
            message.push_str("; ");
            message.push_str(&error.message);
        }
        if let Some(status) = self.http_status {
            message.push_str(&format!("; http_status={status}"));
        }
        message
    }

    async fn log(&self, host: &HostClient, job_id: &str, failed: bool) {
        let Ok(Value::Object(mut fields)) = serde_json::to_value(self) else {
            return;
        };
        fields.insert("jobId".into(), json!(job_id));
        let request = LogRequest {
            event: if failed {
                "image_job_failed"
            } else {
                "image_job_succeeded"
            }
            .into(),
            level: if failed {
                LogLevel::Warn
            } else {
                LogLevel::Info
            },
            fields: fields.into_iter().collect(),
        };
        if let Ok(params) = serde_json::to_value(request) {
            // 日志只做一次尽力投递；日志预算或通道失败不能覆盖原始任务结果。
            let _ = host.call(host::LOG, params, Vec::new()).await;
        }
    }
}

struct WorkerActivity<'a>(&'a AtomicBool);

impl Drop for WorkerActivity<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct JobExecution {
    result: Result<(String, String, u64, String, u32, u32), PluginFault>,
    diagnostic: JobDiagnostic,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct JobQueue {
    pub jobs: Vec<ImageJob>,
}

impl ImageJob {
    fn driver<'a>(&'a self, config: &'a Config) -> &'a str {
        self.driver_model.as_deref().unwrap_or(&config.driver)
    }

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
            driver_model: None,
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
            result_mime: None,
            actual_width: None,
            actual_height: None,
            error: None,
            diagnostic: None,
        }
    }
}
pub struct BackendJobs {
    config: Arc<Config>,
    lock: Mutex<()>,
    worker_active: AtomicBool,
    next_account: AtomicUsize,
    account_semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl BackendJobs {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            lock: Mutex::new(()),
            worker_active: AtomicBool::new(false),
            next_account: AtomicUsize::new(0),
            account_semaphores: Mutex::new(HashMap::new()),
        }
    }
    pub fn enabled(&self) -> bool {
        // 不再依赖 imageStorageDir 手工配置。临时路径也能执行生图。
        true
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
            return Err(invalid("invalid automatic image result directory"));
        }
        Ok(path)
    }
    async fn ensure_storage(&self) -> Result<PathBuf, PluginFault> {
        let path = self.storage_path()?;
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|_| fault("cannot create image result directory"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // 显式配置的目录可能由管理员预先挂载。缩小访问权而不扩展访问权。
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|_| fault("cannot restrict image result directory permissions"))?;
        }
        Ok(path)
    }
    fn result_path(&self, id: &str) -> Result<PathBuf, PluginFault> {
        let job = Uuid::parse_str(id).map_err(|_| invalid("invalid job ID"))?;
        Ok(self.storage_path()?.join(format!("{job}.img")))
    }
    async fn existing_result_path(&self, id: &str) -> Result<PathBuf, PluginFault> {
        let current = self.result_path(id)?;
        if tokio::fs::try_exists(&current).await.unwrap_or(false) {
            return Ok(current);
        }
        let legacy = current.with_extension("png");
        if tokio::fs::try_exists(&legacy).await.unwrap_or(false) {
            return Ok(legacy);
        }
        Ok(current)
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
        Ok(json!({
            "jobs":queue.jobs,
            "backendEnabled":self.enabled(),
            "workerActive":self.worker_active.load(Ordering::Acquire),
            "dispatchIntervalMs":30_000,
            "aggregateConcurrencyLimit":MAX_CONCURRENT_IMAGE_JOBS,
            "executionTransport":"responses_image_generation",
            "pluginVersion":env!("CARGO_PKG_VERSION")
        }))
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
        let driver = if fields.contains_key("driverModel") {
            str_field("driverModel", 128)?
        } else {
            self.config.driver.clone()
        };
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
        if !access::allowed_models_for_driver(host, &self.config, &key, &driver)
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
            let mut job = ImageJob::new(
                key.clone(),
                model.clone(),
                prompt.clone(),
                size.clone(),
                quality.clone(),
            );
            job.driver_model = Some(driver.clone());
            ids.push(job.id.clone());
            queue.jobs.push(job);
        }
        self.save(host, &queue, version).await?;
        for id in dropped {
            if let Ok(path) = self.result_path(&id) {
                let _ = tokio::fs::remove_file(&path).await;
                let _ = tokio::fs::remove_file(path.with_extension("png")).await;
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
        let path = self.existing_result_path(id).await?;
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
        if self
            .worker_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let _activity = WorkerActivity(&self.worker_active);
        self.ensure_storage().await?;
        // 并发根据实际生图账号池大小乘以“每账号并发”计算，不再固定整池 3 个。
        // 真实账号租约仍由 CPR 宿主维护，不能在插件内绕过其更严格的限额。
        let eligible_accounts = access::account_ids(host, &self.config).await?;
        let tick_capacity =
            dispatch_capacity(eligible_accounts.len(), self.config.concurrency_per_account);
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
                let recovered = match self.existing_result_path(&job.id).await {
                    Ok(path) => match tokio::fs::metadata(&path).await {
                        Ok(meta) if (16..=MAX_PNG_BYTES as u64).contains(&meta.len()) => {
                            // 恢复后的文件可能是 JPEG/WebP 或老版 PNG。
                            match tokio::fs::read(&path).await {
                                Ok(contents) => match inspect_image(&contents, &job.size) {
                                    Ok((mime, width, height)) => {
                                        job.result_mime = Some(mime.to_owned());
                                        job.actual_width = Some(width);
                                        job.actual_height = Some(height);
                                        Some(meta.len())
                                    }
                                    Err(_) => None,
                                },
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
            let mut client_keys = HashSet::new();
            for job in queue.jobs.iter_mut().filter(|job| job.status == "queued") {
                if started.len() >= tick_capacity {
                    break;
                }
                if !client_keys.contains(&job.client_key_id)
                    && client_keys.len() >= MAX_KEYS_PER_MAINTENANCE
                {
                    continue;
                }
                client_keys.insert(job.client_key_id.clone());
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
        // 每张完成后立即持久化并交付，不能被同批的慢任务拖住完成状态。
        let mut outcomes = active
            .into_iter()
            .map(|job| async move {
                let id = job.id.clone();
                let done = self.execute(host, &job).await;
                (id, done)
            })
            .collect::<futures_util::stream::FuturesUnordered<_>>();
        while let Some((id, result)) = outcomes.next().await {
            let _guard = self.lock.lock().await;
            let (mut queue, version) = self.load(host).await?;
            let Some(job) = queue.jobs.iter_mut().find(|j| j.id == id) else {
                continue;
            };
            if job.status == "canceled" {
                // 用户主动停止的结果不能再交付。
                if let Ok(path) = self.result_path(&id) {
                    let _ = tokio::fs::remove_file(&path).await;
                    let _ = tokio::fs::remove_file(path.with_extension("png")).await;
                }
                continue;
            }
            if job.status != "running" {
                continue;
            }
            job.completed_ms = Some(now_ms());
            let JobExecution { result, diagnostic } = result;
            job.request_id = diagnostic.request_id.clone();
            job.diagnostic = Some(diagnostic.clone());
            match result {
                Ok((account_id, request_id, bytes, mime, width, height)) => {
                    job.status = "succeeded".into();
                    job.account_id = Some(account_id);
                    job.request_id = Some(request_id);
                    job.result_bytes = Some(bytes);
                    job.result_mime = Some(mime);
                    job.actual_width = Some(width);
                    job.actual_height = Some(height);
                    job.error = None;
                }
                Err(e) => {
                    job.status = "failed".into();
                    job.error = Some(diagnostic.error_message(&e));
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
        diagnostic: &mut JobDiagnostic,
    ) -> Result<String, PluginFault> {
        diagnostic.stage = "account_facts".into();
        let ids = access::account_ids(host, &self.config).await?;
        if ids.is_empty() {
            return Err(PluginFault::new(
                ErrorCode::Rejected,
                "no eligible image accounts",
            ));
        }
        diagnostic.stage = "check_access".into();
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
        if !eligible.is_empty() {
            diagnostic.stage = "driver_check_access".into();
            let checked = host
                .check_access(AccessQuery {
                    client_key_id: job.client_key_id.clone(),
                    account_ids: eligible.clone(),
                    upstream_model: Some(job.driver(&self.config).to_owned()),
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
        diagnostic.stage = "quota_guard".into();
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

    async fn execute(&self, host: &HostClient, job: &ImageJob) -> JobExecution {
        let mut diagnostic = JobDiagnostic::new(job, &self.config);
        let mut stream_handle = None;
        let result = self
            .execute_inner(host, job, &mut diagnostic, &mut stream_handle)
            .await;
        if let Err(error) = &result {
            diagnostic.record_failure(error);
            // 读取或解析失败后只关闭原流，绝不重新发起可能重复计费的生图。
            if let Some(stream) = stream_handle {
                if let Ok(params) = serde_json::to_value(ModelStreamCloseRequest { stream }) {
                    let _ = host
                        .call(host::MODEL_STREAM_CLOSE, params, Vec::new())
                        .await;
                }
            }
        } else {
            diagnostic.stage = "completed".into();
        }
        diagnostic.log(host, &job.id, result.is_err()).await;
        JobExecution { result, diagnostic }
    }

    async fn execute_inner(
        &self,
        host: &HostClient,
        job: &ImageJob,
        diagnostic: &mut JobDiagnostic,
        stream_handle: &mut Option<String>,
    ) -> Result<(String, String, u64, String, u32, u32), PluginFault> {
        let account = self.choose_account(host, job, diagnostic).await?;
        // 共享账号信号量限制同账号请求；多账号数目自然叠加。维护任务创建的
        // 子请求仍由宿主最终执行账号并发租约与额度限制。
        let semaphore =
            {
                let mut current = self.account_semaphores.lock().await;
                Arc::clone(current.entry(account.clone()).or_insert_with(|| {
                    Arc::new(Semaphore::new(self.config.concurrency_per_account))
                }))
            };
        diagnostic.stage = "account_slot".into();
        let _account_permit = semaphore
            .acquire_owned()
            .await
            .map_err(|_| fault("image account concurrency limiter closed"))?;
        diagnostic.stage = "request_encoding".into();
        // 固定宿主的 model callback 使用文本模型路由；原生 GenerateImage 会被
        // Images Provider 在发送前拒绝。后台在发起前使用已支持的 Responses 工具链，
        // 外部原生 Images HTTP 路由仍由宿主原本的 provider endpoint 承担。
        let driver = job.driver(&self.config);
        let request = ModelExecuteRequest {
            client_key_id: Some(job.client_key_id.clone()),
            model: driver.to_owned(),
            protocol: "openai".into(),
            operation: ModelOperation::Generate,
            provider: Some("openai".into()),
            account_id: Some(account.clone()),
            previous_response_id: None,
        };
        let output_format = preferred_upstream_format(&job.image_model);
        let mut image_request = json!({
            "model":job.image_model,
            "prompt":job.prompt,
            "size":job.size,
            "quality":job.quality,
            "n":1,
            "output_format":output_format
        });
        if output_format == "webp" {
            image_request["output_compression"] = json!(85);
        }
        let encoded =
            serde_json::to_vec(&image_request).map_err(|_| fault("encode image tool request"))?;
        let mut request_body =
            crate::bridge::parse_json(&encoded, crate::bridge::Kind::Generation, driver)
                .map_err(|_| invalid("invalid background image tool request"))?
                .upstream;
        // 一个后台任务只请求一张，禁止驱动模型并行发起多个图片工具调用。
        request_body["parallel_tool_calls"] = json!(false);
        // 生图只要求驱动模型具备工具能力，不强制额外的推理能力或推理输出。
        if let Some(object) = request_body.as_object_mut() {
            object.remove("reasoning");
            object.remove("include");
        }
        let body =
            serde_json::to_vec(&request_body).map_err(|_| fault("encode image model request"))?;
        diagnostic.stage = "model_execute".into();
        diagnostic.upstream_sent = None;
        let reply = host
            .call(
                host::MODEL_EXECUTE_STREAM,
                serde_json::to_value(request).map_err(|_| fault("encode model execution"))?,
                body,
            )
            .await
            .map_err(|error| diagnostic.record_session_error(error, true))?;
        diagnostic.execution_accepted = true;
        diagnostic.stage = "model_stream_open".into();
        let stream: ModelStreamResult = serde_json::from_value(reply.result)
            .map_err(|_| fault("invalid model stream response"))?;
        *stream_handle = Some(stream.stream.clone());
        let request_id = stream.request_id.clone();
        diagnostic.request_id = Some(request_id.clone());
        let mut buffer = Vec::new();
        let mut image: Option<Vec<u8>> = None;
        let mut image_url: Option<String> = None;
        let mut completed = false;
        let mut iteration = 0;
        loop {
            iteration += 1;
            if iteration > 10_000 {
                return Err(PluginFault::new(
                    ErrorCode::Capacity,
                    "too many model output chunks",
                ));
            }
            diagnostic.stage = "model_stream_read".into();
            let reply = host
                .call(
                    host::MODEL_STREAM_READ,
                    serde_json::to_value(ModelStreamReadRequest {
                        stream: stream.stream.clone(),
                        maximum_bytes: MODEL_READ_BYTES,
                    })
                    .map_err(|_| fault("encode stream read"))?,
                    Vec::new(),
                )
                .await
                .map_err(|error| diagnostic.record_session_error(error, false))?;
            diagnostic.stage = "model_event_decode".into();
            let state: ModelStreamReadResult =
                serde_json::from_value(reply.result).map_err(|_| fault("invalid stream read"))?;
            if state.end {
                *stream_handle = None;
            }
            if !reply.payload.is_empty() {
                let batch = ModelEventBatch::decode(&reply.payload)
                    .map_err(|_| fault("invalid model event batch"))?;
                for event in batch.events {
                    if let Some(observation) = event
                        .host
                        .as_ref()
                        .and_then(|host| host.observation.as_ref())
                    {
                        if observation.status_code.is_some() {
                            diagnostic.upstream_sent = Some(true);
                            diagnostic.http_status = observation.status_code;
                        }
                    }
                    diagnostic.stage = "image_response_decode".into();
                    if let Some(wire) = event.wire {
                        match wire.payload {
                            WirePayload::RawJson { body } => {
                                let data: Value = serde_json::from_slice(&body)
                                    .map_err(|_| fault("invalid image response JSON"))?;
                                record_image_event(
                                    &data,
                                    &mut image,
                                    &mut image_url,
                                    &mut completed,
                                    diagnostic,
                                )?;
                            }
                            WirePayload::Json { data, .. }
                            | WirePayload::WebSocket {
                                data: Some(data), ..
                            } => {
                                record_image_event(
                                    &data,
                                    &mut image,
                                    &mut image_url,
                                    &mut completed,
                                    diagnostic,
                                )?;
                            }
                            WirePayload::RawBody { body } => {
                                append_output_bytes(&mut buffer, &body)?
                            }
                            WirePayload::RawSse { frame } => {
                                record_sse_frame(
                                    &frame,
                                    &mut image,
                                    &mut image_url,
                                    &mut completed,
                                    diagnostic,
                                )?;
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
        diagnostic.stage = "image_response_decode".into();
        if !buffer.is_empty() {
            record_response_body(
                &buffer,
                &mut image,
                &mut image_url,
                &mut completed,
                diagnostic,
            )?;
        }
        if !completed {
            diagnostic.safe_reason = Some("response_not_completed".into());
            return Err(fault("Responses image generation ended before completion"));
        }
        let image_bytes = match (image, image_url) {
            (Some(bytes), _) => bytes,
            (None, Some(url)) => {
                diagnostic.stage = "image_download".into();
                download_upstream_image(&url).await?
            }
            (None, None) => return Err(fault("completed response contains no image")),
        };
        diagnostic.stage = "image_validation".into();
        let (mime, width, height) = inspect_image(&image_bytes, &job.size)?;
        diagnostic.stage = "image_storage".into();
        let path = self.result_path(&job.id)?;
        let staging = path.with_extension("part");
        diagnostic.stage = "image_write".into();
        tokio::fs::write(&staging, &image_bytes)
            .await
            .map_err(|_| fault("cannot write image result"))?;
        diagnostic.stage = "image_commit".into();
        tokio::fs::rename(&staging, &path)
            .await
            .map_err(|_| fault("cannot commit image result"))?;
        Ok((
            account,
            request_id,
            image_bytes.len() as u64,
            mime.to_owned(),
            width,
            height,
        ))
    }
}

// Images API and Responses image_generation support WebP and PNG.
// Older/unknown custom IDs use PNG proactively; no paid retries after errors.
fn preferred_upstream_format(model: &str) -> &'static str {
    if matches!(
        model,
        "gpt-image-1.5"
            | "gpt-image-2"
            | "gpt-image-2.5-sunburst"
            | "gpt-image-2.5-flare"
            | "gpt-image-2.5-sunburst-2026-09-08"
            | "gpt-image-2.5-flare-2026-09-08"
            | "gpt-image-2.6-sunburst"
            | "gpt-image-2.6-flare"
    ) {
        "webp"
    } else {
        "png"
    }
}

fn expected_dimensions(size: &str) -> Option<(u32, u32)> {
    let (width, height) = size.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn append_output_bytes(buffer: &mut Vec<u8>, bytes: &[u8]) -> Result<(), PluginFault> {
    if buffer.len().saturating_add(bytes.len()) > MAX_PNG_BYTES * 2 {
        return Err(PluginFault::new(
            ErrorCode::Capacity,
            "model response too large",
        ));
    }
    buffer.extend_from_slice(bytes);
    Ok(())
}

// RawBody 可分多次到达，但完整正文必须复用结构化事件的完成与单图校验。
fn record_response_body(
    body: &[u8],
    image: &mut Option<Vec<u8>>,
    image_url: &mut Option<String>,
    completed: &mut bool,
    diagnostic: &mut JobDiagnostic,
) -> Result<(), PluginFault> {
    if let Ok(data) = serde_json::from_slice::<Value>(body) {
        return record_image_event(&data, image, image_url, completed, diagnostic);
    }
    let text = std::str::from_utf8(body).map_err(|_| fault("invalid Responses body encoding"))?;
    if !text
        .lines()
        .any(|line| line.starts_with("data:") || line.starts_with(':'))
    {
        return Err(fault("invalid Responses body format"));
    }
    record_sse_frame(body, image, image_url, completed, diagnostic)
}

fn record_sse_frame(
    frame: &[u8],
    image: &mut Option<Vec<u8>>,
    image_url: &mut Option<String>,
    completed: &mut bool,
    diagnostic: &mut JobDiagnostic,
) -> Result<(), PluginFault> {
    let mut data = Vec::new();
    let mut consume = |data: &mut Vec<u8>| -> Result<(), PluginFault> {
        if !data.is_empty() && data != b"[DONE]" {
            // RawSse 也用于注释、心跳和非 JSON data，不能把该子集当完整响应。
            if let Ok(value) = serde_json::from_slice::<Value>(data) {
                record_image_event(&value, image, image_url, completed, diagnostic)?;
            }
        }
        data.clear();
        Ok(())
    };
    for raw_line in frame.split(|byte| *byte == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.is_empty() {
            consume(&mut data)?;
        } else if let Some(line) = line.strip_prefix(b"data:") {
            if !data.is_empty() {
                data.push(b'\n');
            }
            append_output_bytes(&mut data, line.strip_prefix(b" ").unwrap_or(line))?;
        }
    }
    consume(&mut data)
}

fn collect_image_calls<'a>(data: &'a Value, calls: &mut Vec<&'a Value>) {
    if data.get("type").and_then(Value::as_str) == Some("image_generation_call")
        && data
            .get("result")
            .and_then(Value::as_str)
            .is_some_and(|result| !result.is_empty())
    {
        calls.push(data);
        return;
    }
    for key in ["item", "response"] {
        if let Some(value) = data.get(key) {
            collect_image_calls(value, calls);
        }
    }
    for key in ["data", "output"] {
        if let Some(items) = data.get(key).and_then(Value::as_array) {
            for item in items {
                collect_image_calls(item, calls);
            }
        }
    }
}

fn record_image_event(
    data: &Value,
    image: &mut Option<Vec<u8>>,
    image_url: &mut Option<String>,
    completed: &mut bool,
    diagnostic: &mut JobDiagnostic,
) -> Result<(), PluginFault> {
    let event_type = data.get("type").and_then(Value::as_str).unwrap_or("");
    let response = data.get("response").unwrap_or(data);
    let status = response.get("status").and_then(Value::as_str);
    if matches!(
        event_type,
        "error" | "response.failed" | "response.incomplete"
    ) || matches!(status, Some("failed" | "incomplete" | "cancelled"))
        || response.get("error").is_some_and(|error| !error.is_null())
    {
        diagnostic.error_origin = Some("upstream_response".into());
        diagnostic.safe_reason = Some("response_failed_or_incomplete".into());
        return Err(fault("Responses image generation failed or was incomplete"));
    }
    if event_type == "response.completed"
        || (response.get("output").is_some() && status == Some("completed"))
    {
        *completed = true;
    }
    let mut image_calls = Vec::new();
    collect_image_calls(data, &mut image_calls);
    if image_calls.len() > 1 {
        diagnostic.safe_reason = Some("multiple_image_results".into());
        return Err(fault("expected exactly one completed image"));
    }
    if let Some(call) = image_calls.first() {
        let Some(id) = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            diagnostic.safe_reason = Some("image_result_id_missing".into());
            return Err(fault("image tool result has no identifier"));
        };
        if diagnostic
            .image_call_id
            .as_deref()
            .is_some_and(|previous| previous != id)
        {
            diagnostic.safe_reason = Some("multiple_image_results".into());
            return Err(fault("expected exactly one completed image"));
        }
        diagnostic.image_call_id = Some(id.to_owned());
    }
    if let Some(source) = image_calls
        .first()
        .and_then(|call| extract_image_source(call))
    {
        match source {
            ImageSource::Base64(b64) => {
                diagnostic.stage = "image_decode".into();
                *image = Some(decode_image(b64)?);
            }
            ImageSource::Url(url) => *image_url = Some(url.to_owned()),
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum ImageSource<'a> {
    Base64(&'a str),
    Url(&'a str),
}

fn extract_image_source(data: &Value) -> Option<ImageSource<'_>> {
    if let Some(value) = data.get("b64_json").and_then(Value::as_str) {
        return Some(ImageSource::Base64(value));
    }
    if let Some(url) = data.get("url").and_then(Value::as_str) {
        return Some(ImageSource::Url(url));
    }
    if let Some(result) = data.get("result").and_then(Value::as_str) {
        if data.get("type").and_then(Value::as_str) == Some("image_generation_call") {
            return Some(if result.starts_with("https://") {
                ImageSource::Url(result)
            } else {
                ImageSource::Base64(result)
            });
        }
    }
    for name in ["data", "output"] {
        if let Some(items) = data.get(name).and_then(Value::as_array) {
            for item in items {
                if let Some(found) = extract_image_source(item) {
                    return Some(found);
                }
            }
        }
    }
    for key in ["item", "response"] {
        if let Some(value) = data.get(key) {
            if let Some(found) = extract_image_source(value) {
                return Some(found);
            }
        }
    }
    None
}

fn decode_image(text: &str) -> Result<Vec<u8>, PluginFault> {
    if text.len() > MAX_PNG_BYTES * 2 {
        return Err(PluginFault::new(
            ErrorCode::Capacity,
            "image result too large",
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| fault("invalid base64 image"))?;
    if bytes.len() < 16 || bytes.len() > MAX_PNG_BYTES {
        return Err(invalid("image response too short or too large"));
    }
    Ok(bytes)
}

fn inspect_image(
    bytes: &[u8],
    requested_size: &str,
) -> Result<(&'static str, u32, u32), PluginFault> {
    if bytes.len() < 16 || bytes.len() > MAX_PNG_BYTES {
        return Err(invalid("image payload size outside allowed range"));
    }
    let format =
        image::guess_format(bytes).map_err(|_| invalid("image response has unknown format"))?;
    let mime = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        _ => return Err(invalid("image response format is unsupported")),
    };
    let reader = ImageReader::with_format(Cursor::new(bytes), format);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| invalid("image dimensions cannot be decoded"))?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 12_000_000 {
        return Err(invalid("image dimensions exceed rendering safety limit"));
    }
    if let Some((expected_width, expected_height)) = expected_dimensions(requested_size) {
        if (width, height) != (expected_width, expected_height) {
            // 不要将上游忽略尺寸后的 1024 图像误报为 4K 生成成功。
            return Err(PluginFault::new(
                ErrorCode::Rejected,
                format!(
                    "requested {expected_width}x{expected_height} but upstream returned {width}x{height}"
                ),
            ));
        }
    }
    Ok((mime, width, height))
}

// Sub2API 也兼容 data[].url：下载后先验证文件真实格式，避免把 URL 误当 Base64。
// 只允许 HTTPS、公网 DNS、禁用自动跳转；每次跳转重新核实目标，防止 SSRF。
async fn download_upstream_image(address: &str) -> Result<Vec<u8>, PluginFault> {
    let mut current =
        reqwest::Url::parse(address).map_err(|_| invalid("upstream image URL is invalid"))?;
    for redirect_count in 0..=3 {
        if current.scheme() != "https"
            || !current.username().is_empty()
            || current.password().is_some()
        {
            return Err(invalid(
                "only credential-free HTTPS image URLs are supported",
            ));
        }
        let host = current
            .host_str()
            .ok_or_else(|| invalid("image URL hostname is missing"))?;
        let port = current
            .port_or_known_default()
            .ok_or_else(|| invalid("image URL port is invalid"))?;
        if port != 443 {
            return Err(invalid("upstream image download requires HTTPS port 443"));
        }
        let ips = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| fault("cannot resolve image URL host"))?
            .map(|address| address.ip())
            .collect::<Vec<_>>();
        if ips.is_empty() || ips.iter().any(|ip| !is_public_ip(*ip)) {
            return Err(invalid(
                "image download target must resolve to public IP addresses",
            ));
        }
        let sockets = ips
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect::<Vec<_>>();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .no_proxy()
            .resolve_to_addrs(host, &sockets)
            .build()
            .map_err(|_| fault("cannot configure image download"))?;
        let response = client
            .get(current.clone())
            .send()
            .await
            .map_err(|_| fault("upstream image URL download failed"))?;
        if response.status().is_redirection() {
            if redirect_count == 3 {
                return Err(invalid("too many image URL redirects"));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|h| h.to_str().ok())
                .ok_or_else(|| invalid("image redirect has no valid location"))?;
            current = current
                .join(location)
                .map_err(|_| invalid("bad image redirect"))?;
            continue;
        }
        if !response.status().is_success() {
            return Err(fault("image download returned a non-success HTTP status"));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_PNG_BYTES as u64)
        {
            return Err(PluginFault::new(
                ErrorCode::Capacity,
                "image URL payload exceeds limit",
            ));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| fault("image URL stream failed"))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_PNG_BYTES {
                return Err(PluginFault::new(
                    ErrorCode::Capacity,
                    "image URL payload exceeds limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(bytes);
    }
    Err(invalid("cannot download image URL"))
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || a == 169 && b == 254
                || a == 172 && (16..=31).contains(&b)
                || a == 192 && b == 168
                || a == 100 && (64..=127).contains(&b)
                || a == 198 && (b == 18 || b == 19)
                || a == 192 && b == 0 && c == 0
                || a == 192 && b == 0 && c == 2
                || a == 198 && b == 51 && c == 100
                || a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let octets = ip.octets();
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || octets[0] == 0x20 && octets[1] == 0x01 && octets[2] == 0x0d && octets[3] == 0xb8)
        }
    }
}
#[cfg(test)]
#[path = "jobs/integration_tests.rs"]
mod integration_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_mime_and_pixel_dimensions_are_verified() {
        let webp = include_bytes!("../tests/fixtures/tiny.webp");
        let png = include_bytes!("../tests/fixtures/tiny.png");
        assert_eq!(
            inspect_image(webp, "16x16").unwrap(),
            ("image/webp", 16, 16)
        );
        assert_eq!(inspect_image(png, "16x16").unwrap(), ("image/png", 16, 16));
        assert!(
            inspect_image(webp, "3840x2160").is_err(),
            "do not report a tiny image as a 4K success"
        );
        assert!(
            inspect_image(webp, "2160x3840").is_err(),
            "do not report a tiny image as 4K portrait"
        );
    }

    #[test]
    fn image_source_supports_urls_and_base64() {
        let url = json!({"data":[{"url":"https://cdn.example.org/generated.webp"}]});
        assert!(matches!(
            extract_image_source(&url),
            Some(ImageSource::Url("https://cdn.example.org/generated.webp"))
        ));
        let b64 = json!({"data":[{"b64_json":"YWJj"}]});
        assert!(matches!(
            extract_image_source(&b64),
            Some(ImageSource::Base64("YWJj"))
        ));
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("169.254.169.254".parse().unwrap()));
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn webp_chosen_upfront_for_documented_models_and_png_for_unknown() {
        for model in [
            "gpt-image-2",
            "gpt-image-2.5-sunburst",
            "gpt-image-2.6-sunburst",
            "gpt-image-2.6-flare",
        ] {
            assert_eq!(preferred_upstream_format(model), "webp");
        }
        assert_eq!(preferred_upstream_format("gpt-image-custom-future"), "png");
        assert_eq!(expected_dimensions("3840x2160"), Some((3840, 2160)));
        assert_eq!(expected_dimensions("2160x3840"), Some((2160, 3840)));
    }
    #[test]
    fn dispatch_capacity_preserves_account_limits_and_rpc_headroom() {
        assert_eq!(dispatch_capacity(0, 8), 0);
        assert_eq!(dispatch_capacity(1, 8), 8);
        assert_eq!(dispatch_capacity(2, 8), 16);
        assert_eq!(dispatch_capacity(20, 8), 16);
        assert_eq!(dispatch_capacity(usize::MAX, 32), 16);
    }
    #[test]
    fn image_payload_extracts_native_and_tool_results() {
        let native = json!({"data":[{"b64_json":"YWJj"}]});
        assert_eq!(
            extract_image_source(&native).and_then(|image| match image {
                ImageSource::Base64(b64) => Some(b64),
                _ => None,
            }),
            Some("YWJj")
        );
        let tool =
            json!({"response":{"output":[{"type":"image_generation_call","result":"ZGVm"}]}});
        assert_eq!(
            extract_image_source(&tool).and_then(|image| match image {
                ImageSource::Base64(b64) => Some(b64),
                _ => None,
            }),
            Some("ZGVm")
        );
        assert!(
            extract_image_source(&json!({"error":"secret"}))
                .map(|_| "image")
                .is_none()
        );
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
