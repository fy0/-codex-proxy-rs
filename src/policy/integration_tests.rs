//! 通过真实 SDK 会话执行调度和额度回调，验证账号隔离及请求级拒绝归因。
use super::*;
use gateway_plugin_sdk::{
    CallBudgets, CallContext, Contributions, Frame, Handshake, Message, PROTOCOL_VERSION, Stage,
    call::data::{ACCESS_CHECK, QUOTA_DETAILS},
    client::{
        CallFuture, CallReply, PluginCall, PluginHandler, PluginSession, SessionConfig, read_frame,
        write_frame,
    },
};
use serde_json::json;
use std::time::SystemTime;
use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

struct Handler {
    config: Arc<Config>,
    registry: Arc<ImageRegistry>,
}

impl PluginHandler for Handler {
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            let reply = schedule(
                TypedCall {
                    request: serde_json::from_value(call.params).unwrap(),
                    payload: call.payload,
                    context: call.context,
                    host: call.host,
                    cancellation: call.cancellation,
                },
                Arc::clone(&self.config),
                Arc::clone(&self.registry),
            )
            .await?;
            Ok(CallReply::unary(
                serde_json::to_value(reply.result).unwrap(),
                Vec::new(),
            ))
        })
    }
}

struct Host {
    reader: ReadHalf<DuplexStream>,
    writer: WriteHalf<DuplexStream>,
    task: tokio::task::JoinHandle<()>,
    registry: Arc<ImageRegistry>,
    quotas: HashMap<String, Value>,
    queries: Vec<String>,
    next_id: u64,
}

impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Host {
    async fn start(configuration: Value) -> Self {
        let config = Arc::new(Config::from_configuration(&configuration).unwrap());
        let registry = Arc::new(ImageRegistry::default());
        let handler = Handler {
            config,
            registry: Arc::clone(&registry),
        };
        let (host, plugin) = tokio::io::duplex(64 * 1024);
        let (plugin_reader, plugin_writer) = tokio::io::split(plugin);
        let task = tokio::spawn(async move {
            let mut session =
                PluginSession::accept(plugin_reader, plugin_writer, SessionConfig::default())
                    .await
                    .unwrap();
            session
                .set_call_budgets(CallBudgets::from([(Stage::Scheduling, 10_000)]))
                .unwrap();
            session.run(handler).await.unwrap();
        });
        let (reader, writer) = tokio::io::split(host);
        let mut host = Self {
            reader,
            writer,
            task,
            registry,
            quotas: HashMap::new(),
            queries: Vec::new(),
            next_id: 1,
        };
        host.send(Frame::control(Message::Hello {
            handshake: Handshake {
                protocol_version: PROTOCOL_VERSION,
                artifact_sha256: "a".repeat(64),
                plugin_id: "test.quota-isolation".into(),
                instance_id: "test-instance".into(),
                generation: 1,
                incarnation: "test-incarnation".into(),
                configuration,
                contributes: Contributions::new(),
            },
        }))
        .await;
        assert!(matches!(
            host.receive().await.message,
            Message::Ready { .. }
        ));
        host
    }

    async fn send(&mut self, frame: Frame) {
        write_frame(&mut self.writer, &frame).await.unwrap();
    }

    async fn receive(&mut self) -> Frame {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), read_frame(&mut self.reader))
                .await
                .unwrap()
                .unwrap();
            match frame.message {
                Message::Suspend { .. } | Message::CallbackConsumed { .. } => {}
                Message::Resume { id, epoch } => {
                    self.send(Frame::control(Message::Resumed { id, epoch }))
                        .await
                }
                _ => return frame,
            }
        }
    }

    async fn schedule(&mut self, request_id: &str, model: &str, candidates: &[&str]) -> Value {
        let id = self.next_id;
        self.next_id += 2;
        let params = json!({
            "request_id": request_id, "attempt_index": 1, "provider":"openai", "model": model,
            "candidates": candidates.iter().map(|id| json!({
                "account_id":id,"available":true,"weight":100,"in_flight":0,"maximum_concurrency":8,
            })).collect::<Vec<_>>(),
        });
        self.send(Frame::control(Message::Call {
            id,
            method: "policy.schedule_account".into(),
            context: CallContext {
                request: None,
                call_id: id,
                instance_id: "test-instance".into(),
                generation: 1,
                incarnation: "test-incarnation".into(),
                stage: Stage::Scheduling,
                timeout_ms: 10_000,
                resource_stream: false,
                resource_scope_id: format!("scope-{id}"),
                request_id: Some(request_id.into()),
                attempt_id: Some(format!("{request_id}:1")),
                account_id: None,
                credential_revision: None,
            },
            params,
        }))
        .await;
        loop {
            let frame = self.receive().await;
            match frame.message {
                Message::Callback {
                    id: callback,
                    parent_id,
                    method,
                    params,
                } => {
                    assert_eq!(parent_id, id);
                    let result = match method.as_str() {
                        QUOTA_DETAILS => {
                            let account = params["account_id"].as_str().unwrap();
                            self.queries.push(account.to_owned());
                            match self.quotas.get(account) {
                                Some(quota) => Ok(quota.clone()),
                                None => Err(PluginFault::new(
                                    ErrorCode::Fault,
                                    "fixture observation unavailable",
                                )),
                            }
                        }
                        ACCESS_CHECK => Ok(
                            json!({"schema_version":1,"client_key_id":params["client_key_id"],
                            "accounts":params["account_ids"].as_array().unwrap().iter().map(|id| json!({"account_id":id,"allowed":true})).collect::<Vec<_>>() }),
                        ),
                        other => panic!("unexpected callback: {other}"),
                    };
                    self.send(Frame::control(match result {
                        Ok(result) => Message::Result {
                            id: callback,
                            result,
                        },
                        Err(error) => Message::Error {
                            id: callback,
                            error,
                        },
                    }))
                    .await;
                }
                Message::Result {
                    id: result_id,
                    result,
                } => {
                    assert_eq!(result_id, id);
                    return result;
                }
                Message::Error { error, .. } => panic!("schedule failed: {:?}", error.code),
                other => panic!("unexpected frame: {other:?}"),
            }
        }
    }

    fn quota(&mut self, account: &str, used: f64) {
        let now = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        self.quotas.insert(
            account.into(),
            json!({"schema_version":1,"account_id":account,
            "observed_at_ms":now,"limit_reached":used >= 100.0,"credits":null,
            "windows":[{"key":"codex-five-hour","group":"codex","label":"quota","limit_id":"codex",
                "limit_name":null,"role":null,"window_seconds":18000,"used_percent":used,
                "reset_at_ms":now+3_600_000,"limit_reached":used >= 100.0}]}),
        );
    }
}

fn configuration() -> Value {
    json!({"quotaGuard":[
        {"accountId":"a","fiveHourLimitPercent":95,"weeklyLimitPercent":null},
        {"accountId":"b","fiveHourLimitPercent":95,"weeklyLimitPercent":null},
        {"accountId":"c","fiveHourLimitPercent":95,"weeklyLimitPercent":null},
    ]})
}

#[tokio::test]
async fn one_accounts_limit_never_denies_other_accounts_or_persists_a_model_ban() {
    let mut host = Host::start(configuration()).await;
    host.quota("a", 100.0);
    host.quota("b", 20.0);
    host.quota("c", 40.0);
    assert_eq!(
        host.schedule("r1", "gpt-6.1-sol", &["a", "b", "c"]).await,
        json!({"decision":"delegate_filtered","exclude_account_ids":["a"]})
    );
    assert_eq!(
        host.schedule("r2", "gpt-6.1-sol", &["b", "c"]).await,
        json!({"decision":"delegate"})
    );
    assert_eq!(
        host.schedule("r3", "another-model", &["b", "c"]).await,
        json!({"decision":"delegate"})
    );
    assert_eq!(
        host.registry.denials.remove("r1").unwrap().code(),
        "cpr_quota_guard_filtered_policy_denied"
    );
    host.quota("a", 10.0);
    assert_eq!(
        host.schedule("r4", "gpt-6.1-sol", &["a", "b", "c"]).await,
        json!({"decision":"delegate"})
    );
}

#[tokio::test]
async fn exhausted_image_scope_explains_why_other_host_candidates_cannot_be_used() {
    let mut config = configuration();
    config["imageAccountIds"] = json!(["a"]);
    let mut host = Host::start(config).await;
    host.quota("a", 100.0);
    host.quota("b", 20.0);
    host.quota("c", 40.0);
    host.registry.mark("image-request", "key", "gpt-image-2");
    assert_eq!(
        host.schedule("image-request", "gpt-6.1-sol", &["a", "b", "c"])
            .await,
        json!({"decision":"reject"})
    );
    let denial = host.registry.denials.remove("image-request").unwrap();
    assert_eq!(denial.code(), "cpr_quota_guard_exhausted");
    assert_eq!(
        denial.body()["error"]["details"],
        json!({"model":"gpt-6.1-sol","candidate_count":3,"scoped_candidate_count":1,"quota_blocked_count":1})
    );
    assert_eq!(host.queries, vec!["a"]);
    // 相同模型的普通请求仍能使用不在生图白名单内的账号。
    assert_eq!(
        host.schedule("text-request", "gpt-6.1-sol", &["b", "c"])
            .await,
        json!({"decision":"delegate"})
    );
}

#[tokio::test]
async fn missing_observations_and_unconfigured_accounts_do_not_inherit_a_known_limit() {
    let mut host = Host::start(configuration()).await;
    host.quota("a", 100.0);
    assert_eq!(
        host.schedule("unknown", "gpt-6.1-sol", &["a", "b", "unconfigured"])
            .await,
        json!({"decision":"delegate_filtered","exclude_account_ids":["a"]})
    );
    host.queries.sort();
    assert_eq!(host.queries, vec!["a", "b"]);
}

#[test]
fn declaring_an_explicitly_disabled_image_tool_does_not_require_image_accounts() {
    assert_eq!(image_intent("generate",br#"{"model":"gpt-6.1-sol","tool_choice":"none","tools":[{"type":"image_generation"}]}"#),None);
    assert_eq!(
        image_intent(
            "generate_image",
            br#"{"model":"gpt-image-2","tool_choice":"none"}"#
        ),
        Some("gpt-image-2".into())
    );
}
