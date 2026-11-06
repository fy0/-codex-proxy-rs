//! 用固定 SDK 的帧和会话驱动真实后台任务；假宿主仅代替外部回调，不代表生产上游验收。
use super::{BackendJobs, ImageJob, JobDiagnostic};
use crate::config::Config;
use base64::Engine as _;
use gateway_plugin_sdk::{
    CallBudgets, CallContext, Contributions, ErrorCode, Frame, Handshake, Message,
    PROTOCOL_VERSION, PluginFault, SendState, Stage,
    call::{
        data,
        host::{self, ModelEventBatch, ModelExecuteRequest, ModelStreamReadRequest, StateRecord},
        model::{ExecutionEvent, WireEvent, WirePayload},
    },
    client::{
        CallFuture, CallReply, PluginCall, PluginHandler, PluginSession, SessionConfig,
        SessionError, read_frame, write_frame,
    },
};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use serde_json::{Value, json};
use std::{io::Cursor, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{DuplexStream, ReadHalf, WriteHalf},
    task::JoinHandle,
};
use uuid::Uuid;

// 独立写出官方宿主默认额度，不能随被测实现的常量变化而放宽。
const HOST_READ_LIMIT: u32 = 1024 * 1024;
const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const CLIENT_KEY: &str = "private-key-id-regression-sentinel";
const ACCOUNT: &str = "private-account-id-regression-sentinel";
const PROMPT: &str = "private-prompt-regression-sentinel";
const PRIVATE_MESSAGE: &str = "private-upstream-error-regression-sentinel";
const PRIVATE_DETAILS: &str = "private-error-details-regression-sentinel";
const PRIVATE_TOKEN: &str = "private-oauth-token-regression-sentinel";
const REQUEST_ID: &str = "request-image-regression";
const STREAM_ID: &str = "stream-image-regression";

struct ResultsDirectory(PathBuf);

impl Drop for ResultsDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct JobsHandler(Arc<BackendJobs>);

impl PluginHandler for JobsHandler {
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            let value = match call.method.as_str() {
                "test.submit" => self.0.submit(&call.host, &call.params).await?,
                "test.reconcile" => {
                    self.0.reconcile(&call.host).await?;
                    json!({})
                }
                "test.list" => self.0.list(&call.host).await?,
                "test.image_chunk" => {
                    let (bytes, end, total) = self.0.image_chunk(&call.host, &call.params).await?;
                    return Ok(CallReply::unary(json!({"end": end, "total": total}), bytes));
                }
                _ => {
                    return Err(PluginFault::new(
                        ErrorCode::Unsupported,
                        "unknown test call",
                    ));
                }
            };
            Ok(CallReply::unary(value, Vec::new()))
        })
    }
}

struct FakeHost {
    reader: ReadHalf<DuplexStream>,
    writer: WriteHalf<DuplexStream>,
    task: Option<JoinHandle<Result<(), SessionError>>>,
    directory: ResultsDirectory,
    model: &'static str,
    image: Vec<u8>,
    fail_read: bool,
    state: Option<StateRecord>,
    next_call: u64,
    executions: usize,
    read_limits: Vec<u32>,
    closes: usize,
    statuses: Vec<String>,
    logs: Vec<Value>,
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl FakeHost {
    async fn start(model: &'static str, format: ImageFormat, fail_read: bool) -> Self {
        let directory = ResultsDirectory(
            std::env::temp_dir().join(format!("cpr-image-job-regression-{}", Uuid::new_v4())),
        );
        let configuration = json!({
            "imageAccountIds": [ACCOUNT],
            "imageModels": [model],
            "imageStorageDir": directory.0,
            "concurrencyPerAccount": 1
        });
        let jobs = Arc::new(BackendJobs::new(Arc::new(
            Config::from_configuration(&configuration).unwrap(),
        )));
        let mut image = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_pixel(1024, 1024, Rgb([29, 107, 211])))
            .write_to(&mut image, format)
            .unwrap();
        let (host, plugin) = tokio::io::duplex(64 * 1024);
        let (plugin_reader, plugin_writer) = tokio::io::split(plugin);
        let task = tokio::spawn(async move {
            let mut session = PluginSession::accept(
                plugin_reader,
                plugin_writer,
                SessionConfig {
                    maximum_calls: 2,
                    maximum_callbacks: 8,
                    ..SessionConfig::default()
                },
            )
            .await?;
            session.set_call_budgets(CallBudgets::from([
                (Stage::Management, 15_000),
                (Stage::Maintenance, 15_000),
            ]))?;
            session.run(JobsHandler(jobs)).await
        });
        let (reader, writer) = tokio::io::split(host);
        let mut host = Self {
            reader,
            writer,
            task: Some(task),
            directory,
            model,
            image: image.into_inner(),
            fail_read,
            state: None,
            next_call: 1,
            executions: 0,
            read_limits: Vec::new(),
            closes: 0,
            statuses: Vec::new(),
            logs: Vec::new(),
        };
        host.send(Frame::control(Message::Hello {
            handshake: Handshake {
                protocol_version: PROTOCOL_VERSION,
                artifact_sha256: "a".repeat(64),
                plugin_id: "test.image-jobs".into(),
                instance_id: "test-instance".into(),
                generation: 1,
                incarnation: "test-incarnation".into(),
                configuration,
                contributes: Contributions::new(),
            },
        }))
        .await;
        let ready = tokio::time::timeout(TEST_TIMEOUT, host.receive())
            .await
            .unwrap();
        assert!(matches!(
            ready.message,
            Message::Ready {
                protocol_version: PROTOCOL_VERSION,
                ..
            }
        ));
        host
    }

    async fn send(&mut self, frame: Frame) {
        write_frame(&mut self.writer, &frame).await.unwrap();
    }

    async fn receive(&mut self) -> Frame {
        loop {
            let frame = read_frame(&mut self.reader).await.unwrap();
            match frame.message {
                // RPC v7 的父调用恢复需要宿主确认，不能只模拟业务 JSON。
                Message::Suspend { .. } | Message::CallbackConsumed { .. } => {}
                Message::Resume { id, epoch } => {
                    self.send(Frame::control(Message::Resumed { id, epoch }))
                        .await;
                }
                _ => return frame,
            }
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<(Value, Vec<u8>), PluginFault> {
        let call_id = self.next_call;
        self.next_call += 2;
        self.send(Frame::control(Message::Call {
            id: call_id,
            method: method.into(),
            context: CallContext {
                request: None,
                call_id,
                instance_id: "test-instance".into(),
                generation: 1,
                incarnation: "test-incarnation".into(),
                stage: if method == "test.reconcile" {
                    Stage::Maintenance
                } else {
                    Stage::Management
                },
                timeout_ms: TEST_TIMEOUT.as_millis() as u64,
                resource_stream: false,
                resource_scope_id: format!("test-scope-{call_id}"),
                request_id: None,
                attempt_id: None,
                account_id: None,
                credential_revision: None,
            },
            params,
        }))
        .await;
        tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                let frame = self.receive().await;
                match frame.message {
                    Message::Callback {
                        id,
                        parent_id,
                        method,
                        params,
                    } => {
                        assert_eq!(parent_id, call_id);
                        let reply = match self.respond(&method, params, frame.payload) {
                            Ok((result, payload)) => Frame {
                                message: Message::Result { id, result },
                                payload,
                            },
                            Err(error) => Frame::control(Message::Error { id, error }),
                        };
                        self.send(reply).await;
                    }
                    Message::Result { id, result } => {
                        assert_eq!(id, call_id);
                        return Ok((result, frame.payload));
                    }
                    Message::Error { id, error } => {
                        assert_eq!(id, call_id);
                        return Err(error);
                    }
                    message => panic!("unexpected protocol frame: {message:?}"),
                }
            }
        })
        .await
        .expect("backend job protocol timed out")
    }

    fn respond(
        &mut self,
        method: &str,
        params: Value,
        payload: Vec<u8>,
    ) -> Result<(Value, Vec<u8>), PluginFault> {
        let result = match method {
            data::ACCOUNTS_LIST => {
                assert_eq!(params, json!({}));
                let query: data::AccountFactsQuery = serde_json::from_slice(&payload).unwrap();
                assert_eq!(query.provider_id.as_deref(), Some("openai"));
                assert!(query.cursor.is_none());
                return Ok((
                    json!({}),
                    serde_json::to_vec(&data::AccountFactsPage {
                        schema_version: 1,
                        accounts: vec![data::AccountFacts {
                            account_id: ACCOUNT.into(),
                            provider_id: "openai".into(),
                            name: "Regression fixture".into(),
                            email: None,
                            group_ids: Vec::new(),
                            enabled: true,
                            updated_at_ms: 0,
                        }],
                        next_cursor: None,
                    })
                    .unwrap(),
                ));
            }
            data::ACCESS_CHECK => {
                assert_eq!(params, json!({}));
                let query: data::AccessQuery = serde_json::from_slice(&payload).unwrap();
                assert_eq!(query.client_key_id, CLIENT_KEY);
                assert_eq!(query.account_ids, vec![ACCOUNT.to_owned()]);
                assert!(query.upstream_model.is_none());
                return Ok((
                    json!({}),
                    serde_json::to_vec(&data::AccessFacts {
                        schema_version: 1,
                        client_key_id: CLIENT_KEY.into(),
                        accounts: vec![data::AccountAccessFacts {
                            account_id: ACCOUNT.into(),
                            allowed: true,
                        }],
                    })
                    .unwrap(),
                ));
            }
            host::STATE_GET => {
                assert!(payload.is_empty());
                let query: host::StateGetRequest = serde_json::from_value(params).unwrap();
                assert_eq!(
                    (query.namespace.as_str(), query.key.as_str()),
                    ("image-jobs", "queue")
                );
                serde_json::to_value(host::StateGetResult {
                    record: self.state.clone(),
                })
                .unwrap()
            }
            host::STATE_PUT => {
                assert!(payload.is_empty());
                let update: host::StatePutRequest = serde_json::from_value(params).unwrap();
                assert_eq!(
                    (update.namespace.as_str(), update.key.as_str()),
                    ("image-jobs", "queue")
                );
                let current = self.state.as_ref().map(|record| record.version);
                if update.expected_version != current {
                    return Err(PluginFault::new(ErrorCode::Conflict, "stale queue version"));
                }
                let version = current.unwrap_or(0) + 1;
                if let Some(status) = update.value["jobs"][0]["status"].as_str() {
                    self.statuses.push(status.to_owned());
                }
                self.state = Some(StateRecord {
                    value: update.value,
                    version,
                    schema_version: 1,
                });
                json!({"version": version})
            }
            host::MODEL_EXECUTE_STREAM => {
                self.executions += 1;
                let request: ModelExecuteRequest = serde_json::from_value(params).unwrap();
                assert_eq!(request.model, self.model);
                assert_eq!(request.client_key_id.as_deref(), Some(CLIENT_KEY));
                assert_eq!(request.account_id.as_deref(), Some(ACCOUNT));
                assert_eq!(request.provider.as_deref(), Some("openai"));
                assert_eq!(request.protocol, "openai");
                assert!(matches!(
                    request.operation,
                    host::ModelOperation::GenerateImage
                ));
                let body: Value = serde_json::from_slice(&payload).unwrap();
                assert_eq!(body["model"], self.model);
                assert_eq!(body["prompt"], PROMPT);
                assert_eq!(body["size"], "1024x1024");
                assert_eq!(body["n"], 1);
                assert_eq!(body["output_format"], "webp");
                assert_eq!(body["output_compression"], 85);
                json!({"request_id": REQUEST_ID, "stream": STREAM_ID})
            }
            host::MODEL_STREAM_READ => {
                assert!(payload.is_empty());
                let request: ModelStreamReadRequest = serde_json::from_value(params).unwrap();
                assert_eq!(request.stream, STREAM_ID);
                self.read_limits.push(request.maximum_bytes);
                if request.maximum_bytes == 0
                    || request.maximum_bytes > HOST_READ_LIMIT
                    || self.fail_read
                {
                    let mut error = PluginFault::new(ErrorCode::InvalidInput, PRIVATE_MESSAGE);
                    error.http_status = Some(400);
                    error.details = Some(json!({
                        "private": PRIVATE_DETAILS,
                        "token": PRIVATE_TOKEN,
                        "prompt": PROMPT,
                        "clientKeyId": CLIENT_KEY,
                        "accountId": ACCOUNT
                    }));
                    return Err(error);
                }
                if self.read_limits.len() == 1 {
                    let body = serde_json::to_vec(&json!({
                        "data": [{"b64_json": base64::engine::general_purpose::STANDARD.encode(&self.image)}]
                    })).unwrap();
                    let batch = ModelEventBatch {
                        events: vec![ExecutionEvent::wire(WireEvent {
                            protocol: "openai".into(),
                            payload: WirePayload::RawJson { body },
                        })],
                    }
                    .encode()
                    .unwrap();
                    assert!(batch.len() <= HOST_READ_LIMIT as usize);
                    return Ok((json!({"events": 1, "end": false}), batch));
                }
                json!({"events": 0, "end": true})
            }
            host::MODEL_STREAM_CLOSE => {
                assert!(payload.is_empty());
                assert_eq!(params["stream"], STREAM_ID);
                self.closes += 1;
                json!({})
            }
            host::LOG => {
                assert!(payload.is_empty());
                serde_json::from_value::<host::LogRequest>(params.clone()).unwrap();
                self.logs.push(params);
                json!({"recorded": true})
            }
            _ => panic!("unexpected host callback: {method}"),
        };
        Ok((result, Vec::new()))
    }

    async fn submit(&mut self) -> String {
        let (result, payload) = self
            .call(
                "test.submit",
                json!({
                    "clientKeyId": CLIENT_KEY,
                    "imageModel": self.model,
                    "prompt": PROMPT,
                    "size": "1024x1024",
                    "quality": "medium",
                    "count": 1
                }),
            )
            .await
            .unwrap();
        assert!(payload.is_empty());
        assert_eq!(result["status"], "queued");
        result["ids"][0].as_str().unwrap().to_owned()
    }

    async fn job(&mut self) -> Value {
        let (result, payload) = self.call("test.list", json!({})).await.unwrap();
        assert!(payload.is_empty());
        assert_eq!(result["jobs"].as_array().unwrap().len(), 1);
        result["jobs"][0].clone()
    }

    async fn shutdown(mut self) {
        self.send(Frame::control(Message::Shutdown)).await;
        tokio::time::timeout(TEST_TIMEOUT, self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

fn assert_private_values_absent(value: &Value) {
    let serialized = serde_json::to_string(value).unwrap();
    for private in [
        CLIENT_KEY,
        ACCOUNT,
        PROMPT,
        PRIVATE_MESSAGE,
        PRIVATE_DETAILS,
        PRIVATE_TOKEN,
    ] {
        assert!(
            !serialized.contains(private),
            "diagnostic leaked a private fixture value"
        );
    }
}

async fn assert_success(model: &'static str, format: ImageFormat, mime: &str) {
    let mut host = FakeHost::start(model, format, false).await;
    let id = host.submit().await;
    host.call("test.reconcile", json!({})).await.unwrap();
    let job = host.job().await;
    assert_eq!(job["id"], id);
    assert_eq!(job["status"], "succeeded");
    assert_eq!(job["requestId"], REQUEST_ID);
    assert_eq!(job["resultMime"], mime);
    assert_eq!(job["actualWidth"], 1024);
    assert_eq!(job["actualHeight"], 1024);
    assert!(job["startedMs"].as_u64().is_some());
    assert!(job["completedMs"].as_u64().unwrap() >= job["startedMs"].as_u64().unwrap());
    assert!(job["error"].is_null());
    assert_eq!(host.executions, 1);
    assert_eq!(host.read_limits.len(), 2);
    assert!(
        host.read_limits
            .iter()
            .all(|limit| (1..=HOST_READ_LIMIT).contains(limit))
    );
    assert!(host.statuses.iter().any(|status| status == "queued"));
    assert!(host.statuses.iter().any(|status| status == "running"));
    assert_eq!(host.statuses.last().map(String::as_str), Some("succeeded"));

    let result_path = host.directory.0.join(format!("{id}.img"));
    let on_disk = tokio::fs::read(&result_path).await.unwrap();
    assert_eq!(on_disk, host.image);
    assert_eq!(job["resultBytes"].as_u64(), Some(on_disk.len() as u64));
    assert!(!result_path.with_extension("part").exists());
    let mut downloaded = Vec::new();
    loop {
        let (metadata, chunk) = host
            .call(
                "test.image_chunk",
                json!({
                    "id": id, "offset": downloaded.len()
                }),
            )
            .await
            .unwrap();
        assert_eq!(metadata["total"].as_u64(), Some(on_disk.len() as u64));
        assert!(!chunk.is_empty());
        assert!(chunk.len() <= 256 * 1024);
        downloaded.extend_from_slice(&chunk);
        if metadata["end"].as_bool().unwrap() {
            break;
        }
        assert!(downloaded.len() < on_disk.len());
    }
    assert_eq!(downloaded, on_disk);
    assert_eq!(image::guess_format(&downloaded).unwrap(), format);
    let preview = image::load_from_memory(&downloaded).unwrap();
    assert_eq!((preview.width(), preview.height()), (1024, 1024));
    assert!(!host.logs.is_empty());
    assert_private_values_absent(&json!(host.logs));
    // 已完成任务再次维护不能重复生成或计费。
    host.call("test.reconcile", json!({})).await.unwrap();
    assert_eq!(host.executions, 1);
    host.shutdown().await;
}

#[tokio::test]
async fn native_25_job_completes_with_webp_and_downloadable_1024_result() {
    assert_success("gpt-image-2.5-sunburst", ImageFormat::WebP, "image/webp").await;
}

#[tokio::test]
async fn native_26_job_preserves_png_content_and_mime_for_1024_result() {
    // 上游即使返回 PNG，也必须按真实文件签名交付，不能因请求 WebP 而错误标注。
    assert_success("gpt-image-2.6-sunburst", ImageFormat::Png, "image/png").await;
}

#[tokio::test]
async fn stream_read_failure_keeps_request_identity_and_closes_without_retry_or_secrets() {
    let model = "gpt-image-2.5-sunburst";
    let mut host = FakeHost::start(model, ImageFormat::WebP, true).await;
    let id = host.submit().await;
    host.call("test.reconcile", json!({})).await.unwrap();
    let job = host.job().await;
    assert_eq!(job["status"], "failed");
    assert_eq!(job["requestId"], REQUEST_ID);
    assert!(job["completedMs"].as_u64().is_some());
    assert!(job["resultBytes"].is_null());
    let diagnostic = &job["diagnostic"];
    assert_eq!(diagnostic["stage"], "model_stream_read");
    assert_eq!(diagnostic["operation"], "generate_image");
    assert_eq!(diagnostic["modelId"], model);
    assert_eq!(diagnostic["executionModel"], model);
    assert_eq!(diagnostic["size"], "1024x1024");
    assert_eq!(diagnostic["outputFormat"], "webp");
    assert_eq!(diagnostic["executionAccepted"], true);
    assert!(diagnostic["upstreamSent"].is_null());
    assert_eq!(diagnostic["sdkErrorCode"], "invalid_input");
    assert_eq!(diagnostic["sdkSendState"], "not_sent");
    assert_eq!(diagnostic["httpStatus"], 400);
    assert_eq!(diagnostic["requestId"], REQUEST_ID);
    assert_eq!(diagnostic["maximumReadBytes"], 1_048_576);
    assert_eq!(host.executions, 1);
    assert_eq!(host.read_limits, vec![HOST_READ_LIMIT]);
    assert_eq!(host.closes, 1);
    assert!(!host.directory.0.join(format!("{id}.img")).exists());
    assert!(!host.directory.0.join(format!("{id}.part")).exists());
    assert_private_values_absent(diagnostic);
    assert_private_values_absent(&job["error"]);
    assert!(!host.logs.is_empty());
    assert_eq!(host.logs.last().unwrap()["event"], "image_job_failed");
    assert_private_values_absent(&json!(host.logs));
    let error = host
        .call("test.image_chunk", json!({"id": id}))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    host.call("test.reconcile", json!({})).await.unwrap();
    assert_eq!(host.executions, 1);
    assert_eq!(host.closes, 1);
    assert_eq!(host.job().await["status"], "failed");
    host.shutdown().await;
}

#[test]
fn http_status_does_not_invent_or_erase_upstream_send_evidence() {
    let config = Config::from_configuration(&json!({})).unwrap();
    let job = ImageJob::new(
        CLIENT_KEY.into(),
        "gpt-image-2.5-sunburst".into(),
        PROMPT.into(),
        "1024x1024".into(),
        "medium".into(),
    );
    let mut diagnostic = JobDiagnostic::new(&job, &config);
    diagnostic.stage = "model_execute".into();
    let mut error = PluginFault::new(ErrorCode::InvalidInput, PRIVATE_MESSAGE);
    error.http_status = Some(413);
    diagnostic.record_failure(&error);
    assert_eq!(diagnostic.upstream_sent, Some(false));
    assert_eq!(diagnostic.http_status, Some(413));

    error.send_state = SendState::Sent;
    diagnostic.record_failure(&error);
    assert_eq!(diagnostic.upstream_sent, Some(true));
    diagnostic.execution_accepted = true;
    diagnostic.stage = "model_stream_read".into();
    error.send_state = SendState::NotSent;
    diagnostic.record_failure(&error);
    assert_eq!(diagnostic.upstream_sent, Some(true));
}

#[test]
fn persisted_job_without_diagnostic_remains_readable() {
    let job = ImageJob::new(
        CLIENT_KEY.into(),
        "gpt-image-2.5-sunburst".into(),
        PROMPT.into(),
        "1024x1024".into(),
        "medium".into(),
    );
    let mut legacy = serde_json::to_value(job).unwrap();
    legacy.as_object_mut().unwrap().remove("diagnostic");
    let restored: ImageJob = serde_json::from_value(legacy).unwrap();
    assert!(restored.diagnostic.is_none());
    assert_eq!(restored.image_model, "gpt-image-2.5-sunburst");
    assert_eq!(restored.status, "queued");
}
