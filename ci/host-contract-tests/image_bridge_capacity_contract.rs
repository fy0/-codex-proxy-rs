//! 在固定官方运行时中直接验证 ModelStream 与真实 SDK 编码边界。
//!
//! 事件源只实现 ExecutionSession 的输入与清理信号；大小校验、事件投影、
//! 编码、读取和取消仍执行宿主原有实现，不需要数据库、插件进程或外部账号。

use super::{ModelStream, encode_events};
use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_core::{
    engine::{CommitRequirement, CoordinatedEvent, EngineError, execution::ExecutionSession},
    event::{ProtocolWireEvent, ProviderEvent, ProviderResponseHeader},
};
use gateway_plugin_sdk::{
    ErrorCode,
    call::{host::ModelEventBatch, model::MAX_EXECUTION_PAYLOAD_BYTES},
};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Default)]
struct SessionProbe {
    commits: AtomicUsize,
    cancels: AtomicUsize,
    finalized: AtomicBool,
    finished: Notify,
}

struct OneEventSession {
    event: Option<CoordinatedEvent>,
    probe: Arc<SessionProbe>,
}

impl ExecutionSession for OneEventSession {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async move {
            let event = self.event.take();
            if event.is_none() {
                self.probe.finalized.store(true, Ordering::SeqCst);
            }
            Ok(event)
        })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async { panic!("本回归必须使用真实流式读取路径") })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        &[]
    }

    fn commit_downstream(&mut self, _: Option<u16>) -> BoxFuture<'_, Result<(), EngineError>> {
        self.probe.commits.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn record_client_status(&mut self, _: u16) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async { Ok(()) })
    }

    fn is_finalized(&self) -> bool {
        self.probe.finalized.load(Ordering::SeqCst)
    }

    fn cancel(&self) {
        self.probe.cancels.fetch_add(1, Ordering::SeqCst);
    }

    fn detach_finalize(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.probe.finalized.store(true, Ordering::SeqCst);
            self.probe.finished.notify_one();
        })
    }
}

fn image_event(result_bytes: usize) -> ProviderEvent {
    let data = json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {
            "id": "ig_capacity_contract",
            "type": "image_generation_call",
            "status": "completed",
            "result": "A".repeat(result_bytes)
        }
    });
    let frame = format!("event: response.output_item.done\ndata: {data}\n\n");
    ProviderEvent::wire(
        ProtocolWireEvent::json_with_raw_sse_metadata(
            "openai",
            Some("response.output_item.done".to_owned()),
            data,
            Bytes::from(frame),
            None,
            None,
        )
        .expect("合法的原生图片 SSE 事件"),
    )
}

fn stream(result_bytes: usize, maximum_payload: usize) -> (Arc<ModelStream>, Arc<SessionProbe>) {
    let probe = Arc::new(SessionProbe::default());
    let session = OneEventSession {
        event: Some(CoordinatedEvent::single(
            image_event(result_bytes),
            CommitRequirement::CommitBeforeDelivery,
        )),
        probe: Arc::clone(&probe),
    };
    (
        Arc::new(ModelStream::new(Box::new(session), maximum_payload)),
        probe,
    )
}

#[tokio::test]
async fn small_image_event_is_read_decoded_committed_and_finalized() {
    let budget = crate::RpcLimits::default().maximum_buffered_body_bytes;
    let (stream, probe) = stream(256, budget);
    let read = stream.read(budget).await.expect("小事件应正常读取");
    assert!(!read.end);
    assert_eq!(read.events, 1);
    let batch = ModelEventBatch::decode(&read.payload).expect("读取结果使用真实 SDK 解码");
    assert_eq!(batch.events.len(), 1);
    assert!(batch.events[0].wire.is_some());
    assert_eq!(probe.commits.load(Ordering::SeqCst), 1);

    let end = stream.read(budget).await.expect("流应完成");
    assert!(end.end);
    assert_eq!(end.events, 0);
    assert!(end.payload.is_empty());
    assert!(probe.finalized.load(Ordering::SeqCst));
    assert_eq!(probe.cancels.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn image_event_with_small_raw_frame_exceeds_default_runtime_budget() {
    let budget = crate::RpcLimits::default().maximum_buffered_body_bytes;
    assert_eq!(budget, 1024 * 1024);
    let event = image_event(600 * 1024);
    let frame = event
        .wire_event()
        .expect("图片 wire")
        .raw_sse_frame()
        .expect("原始 SSE");
    assert!(frame.len() < budget, "原始 SSE 帧本身小于 1 MiB");
    let (encoded, count) = encode_events(vec![event]).expect("此事件尚未超过 SDK 编码上限");
    assert_eq!(count, 1);
    assert!(encoded.len() > budget, "JSON 与原始 SSE 共同计入宿主封套");
    assert!(encoded.len() < MAX_EXECUTION_PAYLOAD_BYTES);

    let (stream, probe) = stream(600 * 1024, budget);
    let fault = match stream.read(budget).await {
        Ok(_) => panic!("超预算事件不能静默截断或交付成功"),
        Err(fault) => fault,
    };
    assert_eq!(fault.code, ErrorCode::Capacity);
    assert_eq!(
        fault.message,
        "model event exceeds the stream payload limit"
    );
    assert_eq!(probe.commits.load(Ordering::SeqCst), 0);
    assert_eq!(probe.cancels.load(Ordering::SeqCst), 1);
    assert!(probe.finalized.load(Ordering::SeqCst));
}

#[tokio::test]
async fn sdk_eight_mib_limit_still_applies_with_a_larger_runtime_budget() {
    assert_eq!(MAX_EXECUTION_PAYLOAD_BYTES, 8 * 1024 * 1024);
    let result_bytes = MAX_EXECUTION_PAYLOAD_BYTES / 2 + 1024;
    let (stream, probe) = stream(result_bytes, MAX_EXECUTION_PAYLOAD_BYTES * 2);
    let fault = match stream.read(MAX_EXECUTION_PAYLOAD_BYTES * 2).await {
        Ok(_) => panic!("增大运行时预算不能绕过 SDK 编码上限"),
        Err(fault) => fault,
    };
    assert_eq!(fault.code, ErrorCode::Capacity);
    assert_eq!(fault.message, "model event batch is too large");
    assert_eq!(probe.commits.load(Ordering::SeqCst), 0);

    // 固定宿主在编码失败后保留会话，调用方须关闭原流完成取消与清理。
    stream.close();
    tokio::time::timeout(Duration::from_secs(5), probe.finished.notified())
        .await
        .expect("关闭原流后清理必须完成");
    assert_eq!(probe.cancels.load(Ordering::SeqCst), 1);
    assert!(probe.finalized.load(Ordering::SeqCst));
}
