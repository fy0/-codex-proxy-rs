//! 业务选号与探测使用同一桶，但开关、候选和到期的语义保持独立。

use gateway_core::{
    account::{
        AccountSelectionPolicy, AccountWeight, MissingTurnStatePolicy, RotationStrategy,
        TurnStateToken,
    },
    concurrency::ConcurrencyQueuePolicy,
    engine::{
        AccountAttemptContext, AttemptContext, ModelRequestId, RequestAttemptContext,
        provider::ProviderRequest,
    },
    error::ProviderErrorKind,
    policy::ClientApiKeyId,
    routing::{
        ClientRoutingScope, ConfigRevision, FrozenAccountScope, ModelCapabilities, ProviderKind,
        ProviderModel, PublicModelId, RoutingContext, RuntimeAccount, RuntimeAccountDirectory,
        RuntimeSnapshot, UpstreamModelId,
    },
    upstream::UpstreamSendState,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    time::{Duration, SystemTime},
};

use super::*;

const ACCOUNT: &str = "acct_turn_probe";
const MODEL: &str = "gpt-5.4";
const COMPLETED: &str = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_ticket\",\"status\":\"completed\",\"output\":[]}}\n\n";

fn strict(enabled: bool) -> TurnStateConfig {
    TurnStateConfig {
        enabled,
        missing_state_policy: MissingTurnStatePolicy::Pause,
        ..TurnStateConfig::default()
    }
}

fn request(ids: &[&str], model: &str) -> (ProviderRequest, AttemptContext) {
    let provider = ProviderKind::new("openai").unwrap();
    let policy = AccountSelectionPolicy::new(
        RotationStrategy::Sticky,
        NonZeroU32::new(8).unwrap(),
        Duration::ZERO,
    )
    .with_queue(ConcurrencyQueuePolicy {
        max_waiting: 4,
        timeout: Duration::from_secs(20),
    });
    let scope = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(
            ids.iter()
                .map(|id| {
                    (
                        ProviderAccountId::new(*id).unwrap(),
                        RuntimeAccount::new(provider.clone(), BTreeSet::new()),
                    )
                })
                .collect(),
        )),
        ClientRoutingScope::all_accounts(),
    ));
    let payload = ProtocolPayload::json_object(
        "openai",
        json!({
            "model": "public-model", "input": "Hello", "session_id": "ticket-session",
        })
        .as_object()
        .unwrap()
        .clone(),
    )
    .unwrap()
    .with_context(json!({"use_websocket": false}).as_object().unwrap().clone());
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        policy,
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider,
            UpstreamModelId::new(model).unwrap(),
            ModelCapabilities::new(BTreeSet::from([operation.kind()]), Some(32_000))
                .with_upstream_feature_validation(),
        )],
        Vec::new(),
    )
    .unwrap()
    .with_model_mappings(BTreeMap::from([
        ("public-model".to_owned(), "middle-model".to_owned()),
        ("middle-model".to_owned(), model.to_owned()),
    ]));
    let plan = snapshot
        .plan(
            &PublicModelId::new("public-model").unwrap(),
            &operation,
            Arc::clone(&scope),
            &RoutingContext::default(),
        )
        .unwrap();
    let attempt = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_ticket").unwrap(),
            ClientApiKeyId::new("key_ticket").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(30),
        policy,
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
        None,
        CancellationToken::new(),
    );
    (
        ProviderRequest::new(operation, plan.candidates()[0].clone()),
        attempt,
    )
}

#[tokio::test]
async fn strict_business_waits_without_queueing_while_worker_and_manual_recovery_remain_available()
{
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, false).await;
    store.seed_turn_state(ACCOUNT, MODEL, strict(true));
    let runtime = tempfile::tempdir().unwrap();
    let mut config = OpenAiConfig::default();
    config.api.base_url = server.uri();
    config.resolve_and_validate(runtime.path()).unwrap();
    let mut bundle =
        provider_openai::initialize(config, turn_state_provider_ports(Arc::clone(&store)))
            .await
            .unwrap();
    let registration = bundle
        .take_worker_contributions()
        .into_iter()
        .find_map(|item| match item {
            WorkerContribution::Registration(item) if item.id.owner() == "openai-turn-state" => {
                Some(item)
            }
            _ => None,
        })
        .unwrap();
    let WorkerRunnable::Scheduled { task, .. } = registration.runnable else {
        panic!("scheduled worker")
    };
    let cycle = || WorkerCycleContext::new(registration.id.clone(), None, CancellationToken::new());
    let id = ProviderAccountId::new(ACCOUNT).unwrap();
    let (input, context) = request(&[ACCOUNT], MODEL);
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        bundle.core_provider().execute(input, context),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    let public = gateway_core::error::GatewayError::from_provider(&error);
    assert_eq!(
        public.kind(),
        gateway_core::error::GatewayErrorKind::NoAvailableProvider
    );
    assert!(
        public
            .to_string()
            .contains("waiting for a valid installed turn state")
    );
    assert_eq!(
        error.diagnostic().unwrap().code(),
        Some("missing_turn_state")
    );
    assert!(server.received_requests().await.unwrap().is_empty());

    let installed = token_at(217, Utc::now().timestamp() - 5);
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-codex-turn-state", installed.clone())
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED),
        )
        .mount(&server)
        .await;
    task.run_cycle(cycle()).await.unwrap();
    assert!(store.account(ACCOUNT).unwrap().enabled());
    assert!(
        store
            .turn_state_bucket(&id, MODEL)
            .await
            .unwrap()
            .unwrap()
            .config
            .enabled
    );
    let (input, context) = request(&[ACCOUNT], MODEL);
    let mut stream = bundle
        .core_provider()
        .execute(input, context)
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    drop(stream);
    let sent = server.received_requests().await.unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].headers["x-codex-turn-state"], installed);
    let body: Value = serde_json::from_slice(
        &zstd::stream::decode_all(std::io::Cursor::new(&sent[1].body)).unwrap(),
    )
    .unwrap();
    assert_eq!(body["model"], MODEL);
    server.reset().await;

    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-codex-turn-state", token(233)))
        .mount(&server)
        .await;
    store.request_turn_probe(ACCOUNT, MODEL);
    task.run_cycle(cycle()).await.unwrap();
    assert_eq!(
        store
            .turn_state_bucket(&id, MODEL)
            .await
            .unwrap()
            .unwrap()
            .current
            .unwrap()
            .value,
        installed
    );
    server.reset().await;
    store.set_current_turn_state(
        ACCOUNT,
        MODEL,
        TurnStateToken::parse(&token_at(217, Utc::now().timestamp() - 3600)).unwrap(),
        false,
    );
    let (input, context) = request(&[ACCOUNT], MODEL);
    assert_eq!(
        bundle
            .core_provider()
            .execute(input, context)
            .await
            .unwrap_err()
            .kind(),
        ProviderErrorKind::NoEligibleAccount
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(store.account(ACCOUNT).unwrap().enabled());

    store.seed_turn_state(ACCOUNT, MODEL, strict(false));
    task.run_cycle(cycle()).await.unwrap();
    assert!(server.received_requests().await.unwrap().is_empty());
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-codex-turn-state", token(217)))
        .mount(&server)
        .await;
    store.request_turn_probe(ACCOUNT, MODEL);
    task.run_cycle(cycle()).await.unwrap();
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    assert!(!bucket.config.enabled);
    assert!(bucket.current.is_none());
    let (input, context) = request(&[ACCOUNT], MODEL);
    assert_eq!(
        bundle
            .core_provider()
            .execute(input, context)
            .await
            .unwrap_err()
            .kind(),
        ProviderErrorKind::NoEligibleAccount
    );
    // 手动安装的持久化事务由 Store 集成测试覆盖，此处验证业务读取安装后的事实。
    store.set_current_turn_state(ACCOUNT, MODEL, bucket.candidate.unwrap(), true);
    let (input, context) = request(&[ACCOUNT], MODEL);
    drop(
        bundle
            .core_provider()
            .execute(input, context)
            .await
            .unwrap(),
    );
    store.set_enabled(ACCOUNT, false);
    store.request_turn_probe(ACCOUNT, MODEL);
    server.reset().await;
    task.run_cycle(cycle()).await.unwrap();
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(!store.account(ACCOUNT).unwrap().enabled());
    let (input, context) = request(&[ACCOUNT], MODEL);
    assert_eq!(
        bundle
            .core_provider()
            .execute(input, context)
            .await
            .unwrap_err()
            .kind(),
        ProviderErrorKind::NoEligibleAccount
    );
}

#[tokio::test]
async fn mapped_model_and_sticky_account_cannot_bypass_expired_bucket() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, false).await;
    seed_named(&store, false, "acct_other").await;
    store.seed_turn_state(ACCOUNT, MODEL, strict(true));
    store.set_scheduling(ACCOUNT, None, AccountWeight::new(100).unwrap());
    store.set_scheduling("acct_other", None, AccountWeight::new(1).unwrap());
    store.set_current_turn_state(
        ACCOUNT,
        MODEL,
        TurnStateToken::parse(&token(217)).unwrap(),
        false,
    );
    let runtime = tempfile::tempdir().unwrap();
    let mut config = OpenAiConfig::default();
    config.api.base_url = server.uri();
    config.resolve_and_validate(runtime.path()).unwrap();
    let bundle = provider_openai::initialize(config, turn_state_provider_ports(Arc::clone(&store)))
        .await
        .unwrap();
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED),
        )
        .mount(&server)
        .await;
    for (model, expected) in [
        (MODEL, ACCOUNT),
        (MODEL, "acct_other"),
        ("other-model", ACCOUNT),
    ] {
        let ids: &[&str] = if model == MODEL {
            &[ACCOUNT, "acct_other"]
        } else {
            &[ACCOUNT]
        };
        let (input, context) = request(ids, model);
        let mut stream = bundle
            .core_provider()
            .execute(input, context)
            .await
            .unwrap();
        assert_eq!(stream.metadata().provider_account_id().as_str(), expected);
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        drop(stream);
        store.set_current_turn_state(
            ACCOUNT,
            MODEL,
            TurnStateToken::parse(&token_at(217, Utc::now().timestamp() - 3600)).unwrap(),
            false,
        );
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}
