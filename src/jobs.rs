//! 独立于管理页面连接的异步生图任务。任务状态在 host.state，图片写入本地临时目录。
//! maintenance 每 30 秒对队列进行串行认领；网页关闭只终止轮询，不终止后端执行。
use crate::{access, config::Config, quota_guard};
use base64::Engine as _;
use futures_util::StreamExt;
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
use image::{ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
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
    #[serde(default)]
    pub result_mime: Option<String>,
    #[serde(default)]
    pub actual_width: Option<u32>,
    #[serde(default)]
    pub actual_height: Option<u32>,
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
            result_mime: None,
            actual_width: None,
            actual_height: None,
            error: None,
        }
    }
}
pub struct BackendJobs {
    config: Arc<Config>,
    lock: Mutex<()>,
    next_account: AtomicUsize,
    account_semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl BackendJobs {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            lock: Mutex::new(()),
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
        self.ensure_storage().await?;
        // 并发根据实际生图账号池大小乘以“每账号并发”计算，不再固定整池 3 个。
        // 真实账号租约仍由 CPR 宿主维护，不能在插件内绕过其更严格的限额。
        let eligible_accounts = access::account_ids(host, &self.config).await?;
        let tick_capacity = eligible_accounts
            .len()
            .saturating_mul(self.config.concurrency_per_account)
            .min(MAX_PENDING);
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
            for job in queue
                .jobs
                .iter_mut()
                .filter(|j| j.status == "queued")
                .take(tick_capacity)
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
                    let _ = tokio::fs::remove_file(&path).await;
                    let _ = tokio::fs::remove_file(path.with_extension("png")).await;
                }
                continue;
            }
            if job.status != "running" {
                continue;
            }
            job.completed_ms = Some(now_ms());
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
                    // 不把账号私有错误正文或 Token 写入公共管理状态。
                    // Controlled dimension mismatch message is safe to show. Never expose
                    // arbitrary upstream response bodies or account credentials.
                    job.error = Some(
                        if e.code == ErrorCode::Rejected
                            && e.message.starts_with("requested ")
                            && e.message.contains(" but upstream returned ")
                        {
                            e.message.clone()
                        } else if e.code == ErrorCode::Capacity {
                            "image response exceeded the CPR model event/payload limit; try WebP or lower resolution".into()
                        } else {
                            format!("backend image job failed: {:?}", e.code)
                        },
                    );
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
    ) -> Result<(String, String, u64, String, u32, u32), PluginFault> {
        let account = self.choose_account(host, job).await?;
        // 共享账号信号量限制同账号请求；多账号数目自然叠加。维护任务创建的
        // 子请求仍由宿主最终执行账号并发租约与额度限制。
        let semaphore =
            {
                let mut current = self.account_semaphores.lock().await;
                Arc::clone(current.entry(account.clone()).or_insert_with(|| {
                    Arc::new(Semaphore::new(self.config.concurrency_per_account))
                }))
            };
        let _account_permit = semaphore
            .acquire_owned()
            .await
            .map_err(|_| fault("image account concurrency limiter closed"))?;
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
        // WebP is documented for GPT Image 1.5/2.x Images and Responses tools.
        // Choose the format before sending; do not retry a paid image generation on error.
        let output_format = preferred_upstream_format(&job.image_model);
        let request_body = if via_responses {
            json!({
                "model":self.config.driver,
                "input":[{"role":"user","content":[{"type":"input_text","text":job.prompt}]}],
                "tools":[{
                    "type":"image_generation",
                    "model":job.image_model,
                    "size":job.size,
                    "quality":job.quality,
                    "action":"generate",
                    "output_format":output_format
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
                "n":1,
                "output_format":output_format
            })
        };
        let mut request_body = request_body;
        if output_format == "webp" {
            // Official output_compression applies only to webp/jpeg; never send for png.
            if via_responses {
                if let Some(tool) = request_body
                    .get_mut("tools")
                    .and_then(Value::as_array_mut)
                    .and_then(|tools| tools.first_mut())
                    .and_then(Value::as_object_mut)
                {
                    tool.insert("output_compression".to_owned(), json!(85));
                }
            } else if let Some(object) = request_body.as_object_mut() {
                object.insert("output_compression".to_owned(), json!(85));
            }
        }
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
        let mut image_url: Option<String> = None;
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
                                if let Some(value) = extract_image_source(&data) {
                                    match value {
                                        ImageSource::Base64(b64) => {
                                            image = Some(decode_image(b64)?)
                                        }
                                        ImageSource::Url(href) => image_url = Some(href.to_owned()),
                                    }
                                }
                            }
                            WirePayload::Json { data, .. } => {
                                if let Some(value) = extract_image_source(&data) {
                                    match value {
                                        ImageSource::Base64(b64) => {
                                            image = Some(decode_image(b64)?)
                                        }
                                        ImageSource::Url(href) => image_url = Some(href.to_owned()),
                                    }
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
        let image_bytes = match (image, image_url) {
            (Some(bytes), _) => bytes,
            (None, Some(url)) => download_upstream_image(&url).await?,
            (None, None) => {
                let data: Value = serde_json::from_slice(&buffer)
                    .map_err(|_| fault("image model did not return JSON image data"))?;
                match extract_image_source(&data) {
                    Some(ImageSource::Base64(b64)) => decode_image(b64)?,
                    Some(ImageSource::Url(url)) => download_upstream_image(url).await?,
                    None => return Err(fault("no image bytes or image URL in response")),
                }
            }
        };
        let (mime, width, height) = inspect_image(&image_bytes, &job.size)?;
        let path = self.result_path(&job.id)?;
        let staging = path.with_extension("part");
        tokio::fs::write(&staging, &image_bytes)
            .await
            .map_err(|_| fault("cannot write image result"))?;
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
    if let Some(response) = data.get("response") {
        return extract_image_source(response);
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
    fn per_account_capacity_multiplies_across_eligible_accounts() {
        let config = Config::from_configuration(&json!({ "concurrencyPerAccount": 8 })).unwrap();
        assert_eq!(config.concurrency_per_account * 1, 8);
        assert_eq!(config.concurrency_per_account * 4, 32);
        assert_eq!((config.concurrency_per_account * 20).min(MAX_PENDING), 128);
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
