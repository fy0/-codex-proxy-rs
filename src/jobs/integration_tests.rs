//! 用固定 SDK 的会话与帧验证历史状态兼容；宿主只允许 state 回调，不模拟生图成功。
use super::{BackendJobs, CHUNK_BYTES};
use crate::config::Config;
use gateway_plugin_sdk::{
    CallBudgets, CallContext, Contributions, ErrorCode, Frame, Handshake, Message,
    PROTOCOL_VERSION, PluginFault, Stage,
    call::host::{self, StateRecord},
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

const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const PNG: &[u8] = include_bytes!("../../tests/fixtures/tiny.png");
const WEBP: &[u8] = include_bytes!("../../tests/fixtures/tiny.webp");

struct ResultsDirectory(PathBuf);

impl Drop for ResultsDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct ArchiveHandler(Arc<BackendJobs>);

impl PluginHandler for ArchiveHandler {
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            let value = match call.method.as_str() {
                "test.list" => self.0.list(&call.host).await?,
                "test.cancel" => self.0.cancel(&call.host, &call.params).await?,
                "test.image_chunk" => {
                    let (bytes, end, total) = self.0.image_chunk(&call.host, &call.params).await?;
                    return Ok(CallReply::unary(json!({"end":end,"total":total}), bytes));
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

struct ArchiveHost {
    reader: ReadHalf<DuplexStream>,
    writer: WriteHalf<DuplexStream>,
    task: Option<JoinHandle<Result<(), SessionError>>>,
    directory: ResultsDirectory,
    state: Option<StateRecord>,
    next_call: u64,
    writes: usize,
    put_attempts: usize,
    fail_get: bool,
    reject_put: bool,
}

impl Drop for ArchiveHost {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl ArchiveHost {
    async fn start(state: Option<StateRecord>) -> Self {
        let directory = ResultsDirectory(
            std::env::temp_dir().join(format!("cpr-image-archive-regression-{}", Uuid::new_v4())),
        );
        tokio::fs::create_dir_all(&directory.0).await.unwrap();
        let configuration = json!({"imageStorageDir":directory.0});
        let jobs = Arc::new(BackendJobs::new(Arc::new(
            Config::from_configuration(&configuration).unwrap(),
        )));
        assert!(!jobs.enabled());
        let (host, plugin) = tokio::io::duplex(64 * 1024);
        let (plugin_reader, plugin_writer) = tokio::io::split(plugin);
        let task = tokio::spawn(async move {
            let mut session = PluginSession::accept(
                plugin_reader,
                plugin_writer,
                SessionConfig {
                    maximum_calls: 2,
                    maximum_callbacks: 2,
                    ..SessionConfig::default()
                },
            )
            .await?;
            session.set_call_budgets(CallBudgets::from([(Stage::Management, 15_000)]))?;
            session.run(ArchiveHandler(jobs)).await
        });
        let (reader, writer) = tokio::io::split(host);
        let mut host = Self {
            reader,
            writer,
            task: Some(task),
            directory,
            state,
            next_call: 1,
            writes: 0,
            put_attempts: 0,
            fail_get: false,
            reject_put: false,
        };
        host.send(Frame::control(Message::Hello {
            handshake: Handshake {
                protocol_version: PROTOCOL_VERSION,
                artifact_sha256: "a".repeat(64),
                plugin_id: "test.image-archive".into(),
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
                // RPC v7 的父调用恢复需要宿主确认。
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
                stage: Stage::Management,
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
                        assert!(frame.payload.is_empty());
                        let reply = match self.respond(&method, params) {
                            Ok(result) => Frame::control(Message::Result { id, result }),
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
        .expect("archive state protocol timed out")
    }

    fn respond(&mut self, method: &str, params: Value) -> Result<Value, PluginFault> {
        match method {
            host::STATE_GET => {
                let query: host::StateGetRequest = serde_json::from_value(params).unwrap();
                assert_eq!(
                    (query.namespace.as_str(), query.key.as_str()),
                    ("image-jobs", "queue")
                );
                if self.fail_get {
                    return Err(PluginFault::new(
                        ErrorCode::Fault,
                        "fixture state unavailable",
                    ));
                }
                Ok(serde_json::to_value(host::StateGetResult {
                    record: self.state.clone(),
                })
                .unwrap())
            }
            host::STATE_PUT => {
                self.put_attempts += 1;
                let update: host::StatePutRequest = serde_json::from_value(params).unwrap();
                assert_eq!(
                    (update.namespace.as_str(), update.key.as_str()),
                    ("image-jobs", "queue")
                );
                let current = self.state.as_ref().map(|record| record.version);
                if self.reject_put || update.expected_version != current {
                    return Err(PluginFault::new(
                        ErrorCode::Conflict,
                        "fixture stale queue version",
                    ));
                }
                self.writes += 1;
                let version = current.unwrap_or(0) + 1;
                self.state = Some(StateRecord {
                    value: update.value,
                    version,
                    schema_version: 1,
                });
                Ok(json!({"version":version}))
            }
            // 任何模型、账号、网络或日志回调都使本测试失败。
            _ => panic!("archive must not invoke host callback: {method}"),
        }
    }

    async fn list(&mut self) -> Value {
        let (value, payload) = self.call("test.list", json!({})).await.unwrap();
        assert!(payload.is_empty());
        assert_eq!(value["legacyOnly"], true);
        assert_eq!(value["backendEnabled"], false);
        assert_eq!(value["workerActive"], false);
        value
    }

    async fn write_image(&self, job: &Value, extension: &str, bytes: &[u8]) {
        tokio::fs::write(
            self.directory
                .0
                .join(format!("{}.{}", job["id"].as_str().unwrap(), extension)),
            bytes,
        )
        .await
        .unwrap();
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

fn job(status: &str, bytes: &[u8]) -> Value {
    let image = image::load_from_memory(bytes).unwrap();
    json!({
        "id":Uuid::new_v4().to_string(),"status":status,
        "clientKeyId":"fixture-key","accountId":"fixture-account",
        "imageModel":"gpt-image-2.5-sunburst","driverModel":"fixture-driver",
        "prompt":"fixture historical prompt",
        "size":format!("{}x{}",image.width(),image.height()),
        "quality":"medium","createdMs":10,"startedMs":20,"completedMs":null,
        "requestId":"fixture-request",
        "diagnostic":{"stage":"model_stream_read","futureDiagnostic":{"retain":[1,2,3]}},
        "futureJobField":{"retain":true}
    })
}

fn state(jobs: Vec<Value>) -> StateRecord {
    StateRecord {
        value: json!({"jobs":jobs}),
        version: 7,
        schema_version: 1,
    }
}

#[tokio::test]
async fn absent_history_is_empty_without_creating_state() {
    let mut host = ArchiveHost::start(None).await;
    assert_eq!(host.list().await["jobs"], json!([]));
    assert!(host.state.is_none());
    assert_eq!(host.put_attempts, 0);
    host.shutdown().await;
}

#[tokio::test]
async fn old_statuses_and_unknown_fields_survive_without_execution() {
    let mut jobs = [
        "queued",
        "running",
        "succeeded",
        "failed",
        "canceled",
        "future_terminal",
    ]
    .into_iter()
    .map(|status| job(status, PNG))
    .collect::<Vec<_>>();
    // 旧版本没有 diagnostic 字段，也必须原样保留。
    jobs[0].as_object_mut().unwrap().remove("diagnostic");
    let original = state(jobs.clone());
    let mut host = ArchiveHost::start(Some(original.clone())).await;
    assert_eq!(host.list().await["jobs"], json!(jobs));
    assert_eq!(host.list().await["jobs"], json!(jobs));
    assert_eq!(host.state.as_ref().unwrap().value, original.value);
    assert_eq!(host.state.as_ref().unwrap().version, original.version);
    assert_eq!(host.put_attempts, 0);
    host.shutdown().await;
}

#[tokio::test]
async fn only_complete_committed_running_images_are_recovered() {
    let webp = job("running", WEBP);
    let png = job("running", PNG);
    let truncated = job("running", PNG);
    let mut mismatched = job("running", PNG);
    mismatched["size"] = json!("1024x1024");
    let queued = job("queued", WEBP);
    let original = vec![
        webp.clone(),
        png.clone(),
        truncated.clone(),
        mismatched.clone(),
        queued.clone(),
    ];
    let mut host = ArchiveHost::start(Some(state(original))).await;
    host.write_image(&webp, "img", WEBP).await;
    host.write_image(&png, "png", PNG).await;
    host.write_image(&truncated, "img", &PNG[..PNG.len() / 2])
        .await;
    host.write_image(&mismatched, "img", PNG).await;
    host.write_image(&queued, "img", WEBP).await;
    let listed = host.list().await;
    for (index, mime, bytes) in [(0, "image/webp", WEBP), (1, "image/png", PNG)] {
        let result = &listed["jobs"][index];
        assert_eq!(result["status"], "succeeded");
        assert_eq!(result["resultMime"], mime);
        assert_eq!(result["resultBytes"], bytes.len() as u64);
        assert!(result["completedMs"].as_u64().unwrap() > 20);
        let mut unchanged = result.clone();
        for key in [
            "status",
            "completedMs",
            "resultBytes",
            "resultMime",
            "actualWidth",
            "actualHeight",
            "error",
        ] {
            unchanged.as_object_mut().unwrap().remove(key);
        }
        let mut before = [webp.clone(), png.clone()][index].clone();
        before.as_object_mut().unwrap().remove("status");
        before.as_object_mut().unwrap().remove("completedMs");
        assert_eq!(unchanged, before);
        let (metadata, payload) = host
            .call("test.image_chunk", json!({"id":result["id"]}))
            .await
            .unwrap();
        assert_eq!(payload, bytes);
        assert_eq!(metadata, json!({"end":true,"total":bytes.len()}));
    }
    assert_eq!(listed["jobs"][2], truncated);
    assert_eq!(listed["jobs"][3], mismatched);
    assert_eq!(listed["jobs"][4], queued);
    assert_eq!(host.writes, 1);
    assert_eq!(host.state.as_ref().unwrap().version, 8);
    host.shutdown().await;
}

#[tokio::test]
async fn cancel_changes_only_selected_history_status_and_keeps_files() {
    let running = job("running", PNG);
    let terminal = job("succeeded", WEBP);
    let original = state(vec![running.clone(), terminal.clone()]);
    let mut host = ArchiveHost::start(Some(original)).await;
    host.write_image(&running, "img", PNG).await;
    let (reply, _) = host
        .call("test.cancel", json!({"id":running["id"]}))
        .await
        .unwrap();
    assert_eq!(reply["status"], "canceled");
    assert_eq!(reply["legacyOnly"], true);
    let stored = &host.state.as_ref().unwrap().value["jobs"][0];
    let mut expected = running.clone();
    expected["status"] = json!("canceled");
    expected["completedMs"] = stored["completedMs"].clone();
    assert_eq!(stored, &expected);
    assert_eq!(host.state.as_ref().unwrap().value["jobs"][1], terminal);
    assert!(
        host.directory
            .0
            .join(format!("{}.img", running["id"].as_str().unwrap()))
            .exists()
    );
    host.call("test.cancel", json!({"id":running["id"]}))
        .await
        .unwrap();
    host.call("test.cancel", json!({"id":terminal["id"]}))
        .await
        .unwrap();
    assert_eq!(host.writes, 1);
    assert!(
        host.call("test.image_chunk", json!({"id":running["id"]}))
            .await
            .is_err()
    );
    host.shutdown().await;
}

#[tokio::test]
async fn unavailable_or_invalid_state_never_becomes_empty_history() {
    let original = state(vec![job("queued", PNG)]);
    let id = original.value["jobs"][0]["id"].clone();
    let mut host = ArchiveHost::start(Some(original.clone())).await;
    host.fail_get = true;
    assert!(host.call("test.list", json!({})).await.is_err());
    assert!(host.call("test.cancel", json!({"id":id})).await.is_err());
    assert_eq!(host.state.as_ref().unwrap().value, original.value);
    host.fail_get = false;
    for value in [json!({"jobs":"bad"}), json!({"jobs":[null]}), json!({})] {
        host.state = Some(StateRecord {
            value: value.clone(),
            version: 7,
            schema_version: 1,
        });
        assert!(host.call("test.list", json!({})).await.is_err());
        assert_eq!(host.state.as_ref().unwrap().value, value);
    }
    host.state = Some(StateRecord {
        value: original.value,
        schema_version: 2,
        version: 7,
    });
    assert!(host.call("test.list", json!({})).await.is_err());
    assert_eq!(host.put_attempts, 0);
    host.shutdown().await;
}

#[tokio::test]
async fn stale_state_write_surfaces_conflict_without_retry_or_reset() {
    let running = job("running", PNG);
    let original = state(vec![running.clone()]);
    let mut host = ArchiveHost::start(Some(original.clone())).await;
    host.write_image(&running, "img", PNG).await;
    host.reject_put = true;
    let error = host.call("test.list", json!({})).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(host.put_attempts, 1);
    assert_eq!(host.writes, 0);
    assert_eq!(host.state.as_ref().unwrap().value, original.value);
    assert_eq!(host.state.as_ref().unwrap().version, 7);
    host.shutdown().await;
}

#[tokio::test]
async fn bounded_recovery_rotates_past_missing_files() {
    let jobs = (0..5).map(|_| job("running", PNG)).collect::<Vec<_>>();
    let mut host = ArchiveHost::start(Some(state(jobs.clone()))).await;
    host.write_image(&jobs[4], "img", PNG).await;
    assert_eq!(host.list().await["jobs"], json!(jobs));
    assert_eq!(host.writes, 0);
    let next = host.list().await;
    for (index, original) in jobs.iter().enumerate().take(4) {
        assert_eq!(next["jobs"][index], *original);
    }
    assert_eq!(next["jobs"][4]["status"], "succeeded");
    assert_eq!(host.writes, 1);
    host.shutdown().await;
}

#[tokio::test]
async fn archived_image_chunks_preserve_bytes_and_reject_invalid_offsets() {
    let mut random = 0x1234_5678u32;
    let image = RgbImage::from_fn(384, 384, |_, _| {
        let mut pixel = [0u8; 3];
        for value in &mut pixel {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            *value = random as u8;
        }
        Rgb(pixel)
    });
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(image)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    let bytes = encoded.into_inner();
    assert!(bytes.len() > CHUNK_BYTES);
    let completed = job("succeeded", &bytes);
    let mut host = ArchiveHost::start(Some(state(vec![completed.clone()]))).await;
    host.write_image(&completed, "img", &bytes).await;
    let mut downloaded = Vec::new();
    loop {
        let (meta, chunk) = host
            .call(
                "test.image_chunk",
                json!({
                    "id":completed["id"],"offset":downloaded.len()
                }),
            )
            .await
            .unwrap();
        assert!(chunk.len() <= CHUNK_BYTES);
        assert_eq!(meta["total"], bytes.len() as u64);
        downloaded.extend(chunk);
        if meta["end"] == true {
            break;
        }
    }
    assert_eq!(downloaded, bytes);
    for offset in [json!(-1), json!("bad"), json!(bytes.len() + 1)] {
        assert!(
            host.call(
                "test.image_chunk",
                json!({"id":completed["id"],"offset":offset})
            )
            .await
            .is_err()
        );
    }
    assert_eq!(host.put_attempts, 0);
    host.shutdown().await;
}
