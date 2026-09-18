//! 从真实初始化、worker 到业务转发的离线协议回归。

mod service;

use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use chrono::Utc;
use futures::StreamExt;
use gateway_core::{
    account::{ProviderAccountId, ProviderAccountStore, TurnStateConfig},
    lifecycle::CancellationToken,
    operation::{GenerateRequest, Operation, ProtocolPayload},
    task::{WorkerContribution, WorkerCycleContext, WorkerRunnable},
};
use provider_openai::{
    OpenAiConfig,
    credential::{CodexAccountProfile, CodexOAuthSecret, ImportCodexOAuthCredential},
};
use secrecy::SecretString;
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use crate::{
    admin::{initialized_attempt_context, initialized_provider_request, turn_state_provider_ports},
    support::MemoryAccountStore,
};

fn token(size: usize) -> String {
    token_at(size, Utc::now().timestamp())
}

fn token_at(size: usize, issued_at: i64) -> String {
    let mut bytes = vec![0; size];
    bytes[0] = 0x80;
    bytes[1..9].copy_from_slice(&(issued_at as u64).to_be_bytes());
    URL_SAFE.encode(bytes)
}

async fn seed(store: &MemoryAccountStore, expired: bool) {
    seed_named(store, expired, "acct_turn_probe").await;
}

async fn seed_named(store: &MemoryAccountStore, expired: bool, id: &str) {
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: id.to_owned(),
            name: "probe".to_owned(),
            secret: CodexOAuthSecret {
                access_token: SecretString::from("test-only-access"),
                refresh_token: Some(SecretString::from("must-never-refresh")),
                id_token: None,
            },
            verified_account: CodexAccountProfile {
                email: None,
                poid: None,
                oauth_subject: "test-subject".to_owned(),
                chatgpt_account_id: "upstream-probe".to_owned(),
                chatgpt_user_id: "test-user".to_owned(),
                plan_type: None,
                access_token_expires_at: Some(
                    Utc::now() + chrono::Duration::seconds(if expired { -60 } else { 3600 }),
                ),
            },
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    store.seed_turn_state(
        id,
        "gpt-5.4",
        TurnStateConfig {
            enabled: true,
            retry_seconds: 1,
            jitter_seconds: 0,
            budget: 10,
            timezone: chrono_tz::Pacific::Kiritimati,
            ..TurnStateConfig::default()
        },
    );
}

#[tokio::test]
async fn probes_refresh_identity_observe_any_length_and_inject_only_target_bucket() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, false).await;
    store.seed_turn_state(
        "acct_turn_probe",
        "gpt-5.4",
        TurnStateConfig {
            enabled: true,
            retry_seconds: 1,
            jitter_seconds: 0,
            timezone: chrono_tz::Pacific::Kiritimati,
            originator: "test-probe-persona".to_owned(),
            user_agent: "test-probe-agent/1.0".to_owned(),
            ..TurnStateConfig::default()
        },
    );
    store.set_egress(
        "acct_turn_probe",
        None,
        Some(gateway_core::account::RequestLocation {
            country: "US".to_owned(),
            region: "Hawaii".to_owned(),
            city: "Honolulu".to_owned(),
            timezone: chrono_tz::Pacific::Honolulu,
        }),
    );
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
            WorkerContribution::Registration(registration)
                if registration.id.owner() == "openai-turn-state" =>
            {
                Some(registration)
            }
            _ => None,
        })
        .unwrap();
    let WorkerRunnable::Scheduled { task, .. } = registration.runnable else {
        panic!("scheduled worker");
    };
    let context =
        || WorkerCycleContext::new(registration.id.clone(), None, CancellationToken::new());
    let mut identities = HashSet::new();
    let id = ProviderAccountId::new("acct_turn_probe").unwrap();
    let target = token(217);
    for (size, length) in [(233, 312), (414, 552), (217, 292)] {
        let value = if length == 292 {
            target.clone()
        } else {
            token(size)
        };
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-codex-turn-state", value)
                    .set_body_string("unread"),
            )
            .mount(&server)
            .await;
        task.run_cycle(context()).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.headers["chatgpt-account-id"], "upstream-probe");
        assert_eq!(request.headers["originator"], "test-probe-persona");
        assert_eq!(request.headers["user-agent"], "test-probe-agent/1.0");
        let bytes = zstd::stream::decode_all(std::io::Cursor::new(&request.body)).unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        for key in [
            "session-id",
            "thread-id",
            "x-client-request-id",
            "x-codex-window-id",
        ] {
            let value = request.headers[key].to_str().unwrap();
            assert_eq!(uuid::Uuid::parse_str(value).unwrap().get_version_num(), 7);
            assert!(identities.insert(value.to_owned()));
            assert_eq!(body["client_metadata"][key], value);
        }
        let metadata: Value =
            serde_json::from_str(request.headers["x-codex-turn-metadata"].to_str().unwrap())
                .unwrap();
        let body_metadata: Value = serde_json::from_str(
            body["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(metadata, body_metadata);
        for key in ["turn_id", "context_window_id"] {
            assert!(identities.insert(metadata[key].as_str().unwrap().to_owned()));
        }
        assert!(
            (Utc::now().timestamp_millis() - metadata["turn_started_at_unix_ms"].as_i64().unwrap())
                .abs()
                < 5000
        );
        assert_eq!(
            body["prompt_cache_key"],
            body["client_metadata"]["session_id"]
        );
        assert!(
            body["input"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains(
                    &Utc::now()
                        .with_timezone(&chrono_tz::Pacific::Kiritimati)
                        .format("%Y-%m-%d")
                        .to_string()
                )
        );
        assert!(
            body["input"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("<timezone>Pacific/Kiritimati</timezone>")
        );
        let bucket = store
            .turn_state_bucket(&id, "gpt-5.4")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bucket.current.is_some(), length == 292);
        assert!(
            store
                .turn_state_bucket(&id, "other-model")
                .await
                .unwrap()
                .is_none()
        );
        server.reset().await;
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    }
    let observations = store.turn_observations();
    assert_eq!(
        observations
            .iter()
            .map(|item| item.token_length)
            .collect::<Vec<_>>(),
        vec![Some(312), Some(552), Some(292)]
    );
    assert_eq!(observations[0].outcome, "length_miss");
    for phase in 0..4 {
        if phase == 2 {
            store.set_current_turn_state(
                id.as_str(),
                "gpt-5.4",
                gateway_core::account::TurnStateToken::parse(&target).unwrap(),
                true,
            );
        }
        if phase == 3 {
            let mut bytes = vec![0; 217];
            bytes[0] = 0x80;
            bytes[1..9].copy_from_slice(&((Utc::now().timestamp() - 3600) as u64).to_be_bytes());
            store.set_current_turn_state(
                id.as_str(),
                "gpt-5.4",
                gateway_core::account::TurnStateToken::parse(&URL_SAFE.encode(bytes)).unwrap(),
                true,
            );
        }
        Mock::given(method("POST")).and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
            .insert_header("x-codex-turn-state", if phase == 0 { token(233) } else { target.clone() })
            .set_body_string("event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_turn_test\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[]}}\n\n"))
        .mount(&server).await;
        let payload = ProtocolPayload::json_object(
        "openai",
        json!({"model": "gpt-5.4", "input": "Hello", "prompt_cache_key": format!("session-{phase}"), "client_metadata": {"x-codex-turn-state": "stale-session-state"}})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap()
    .with_context(json!({"use_websocket": false}).as_object().unwrap().clone());
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));
        let mut stream = bundle
            .core_provider()
            .execute(
                initialized_provider_request(operation, id.as_str()),
                initialized_attempt_context("req_turn_state", id.as_str()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let requests = server.received_requests().await.unwrap();
        if phase == 3 {
            assert!(!requests[0].headers.contains_key("x-codex-turn-state"));
            let bytes = zstd::stream::decode_all(std::io::Cursor::new(&requests[0].body)).unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(body["client_metadata"]["x-codex-turn-state"].is_null());
        } else {
            assert_eq!(requests[0].headers["x-codex-turn-state"], target);
            let bytes = zstd::stream::decode_all(std::io::Cursor::new(&requests[0].body)).unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["client_metadata"]["x-codex-turn-state"], target);
        }
        assert_eq!(requests[0].headers["originator"], "Codex Desktop");
        assert!(
            requests[0].headers["user-agent"]
                .to_str()
                .unwrap()
                .starts_with("Codex Desktop/")
        );
        let observation = store.turn_observations().pop().unwrap();
        assert_eq!(
            observation.outcome,
            ["length_miss", "reused_state", "reused_state", "candidate"][phase]
        );
        assert_eq!(
            observation.request_state_source.as_deref(),
            Some(if phase == 3 {
                "none"
            } else if phase == 2 {
                "bucket_manual_override"
            } else {
                "automatic_override"
            })
        );
        if phase == 1 {
            let bucket = store
                .turn_state_bucket(&id, "gpt-5.4")
                .await
                .unwrap()
                .unwrap();
            assert!(bucket.candidate.is_none());
            assert_eq!(
                bucket.current_issued_at,
                gateway_core::account::TurnStateToken::parse(&target).map(|token| token.issued_at)
            );
        }
        server.reset().await;
    }
    assert!(
        store
            .turn_observations()
            .iter()
            .any(|item| item.source == "passive"
                && item.token_length == Some(312)
                && item.request_state_source.as_deref() == Some("automatic_override"))
    );
}

#[tokio::test]
async fn expired_access_token_skips_probe_without_refreshing_credentials() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, true).await;
    let runtime = tempfile::tempdir().unwrap();
    let mut config = OpenAiConfig::default();
    config.api.base_url = server.uri();
    config.auth.oauth_token_endpoint = format!("{}/oauth/token", server.uri());
    config.resolve_and_validate(runtime.path()).unwrap();
    let before = store.account("acct_turn_probe").unwrap();
    let mut bundle =
        provider_openai::initialize(config, turn_state_provider_ports(Arc::clone(&store)))
            .await
            .unwrap();
    for item in bundle.take_worker_contributions() {
        if let WorkerContribution::Registration(registration) = item
            && registration.id.owner() == "openai-turn-state"
            && let WorkerRunnable::Scheduled { task, .. } = registration.runnable
        {
            task.run_cycle(WorkerCycleContext::new(
                registration.id,
                None,
                CancellationToken::new(),
            ))
            .await
            .unwrap();
        }
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(store.account("acct_turn_probe").unwrap(), before);
    assert_eq!(
        store.turn_observations()[0].outcome,
        "access_token_expired_or_unknown"
    );
}

async fn cycle(store: Arc<MemoryAccountStore>, endpoint: String) {
    store
        .schedule_turn_state(
            &ProviderAccountId::new("acct_turn_probe").unwrap(),
            "gpt-5.4",
            Utc::now().timestamp(),
        )
        .await
        .unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let mut config = OpenAiConfig::default();
    config.api.base_url = endpoint;
    config.resolve_and_validate(runtime.path()).unwrap();
    let mut bundle = provider_openai::initialize(config, turn_state_provider_ports(store))
        .await
        .unwrap();
    for item in bundle.take_worker_contributions() {
        if let WorkerContribution::Registration(registration) = item
            && registration.id.owner() == "openai-turn-state"
            && let WorkerRunnable::Scheduled { task, .. } = registration.runnable
        {
            task.run_cycle(WorkerCycleContext::new(
                registration.id,
                None,
                CancellationToken::new(),
            ))
            .await
            .unwrap();
        }
    }
}

#[tokio::test]
async fn probe_stop_strategies_abort_at_the_selected_boundary() {
    use gateway_core::account::TurnStateStopStrategy;
    for strategy in [
        TurnStateStopStrategy::Headers,
        TurnStateStopStrategy::FirstOutput,
        TurnStateStopStrategy::Mixed,
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)
            .insert_header("x-codex-turn-state", token(217))
            .insert_header("content-type", "text/event-stream")
            .set_body_string("data: {\"type\":\"response.created\"}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\ndata: {\"type\":\"response.completed\"}\n\n"))
            .mount(&server).await;
        let store = Arc::new(MemoryAccountStore::default());
        seed(&store, false).await;
        store.seed_turn_state(
            "acct_turn_probe",
            "gpt-5.4",
            TurnStateConfig {
                enabled: true,
                stop_strategy: strategy,
                ..TurnStateConfig::default()
            },
        );
        cycle(Arc::clone(&store), server.uri()).await;
        let observation = store.turn_observations().remove(0);
        assert_eq!(observation.stop_mode, observation.stop_reason);
        match strategy {
            TurnStateStopStrategy::Headers => {
                assert_eq!(observation.stop_reason.as_deref(), Some("headers"))
            }
            TurnStateStopStrategy::FirstOutput => {
                assert_eq!(observation.stop_reason.as_deref(), Some("first_output"))
            }
            TurnStateStopStrategy::Mixed => assert!(
                [Some("headers"), Some("first_output")]
                    .contains(&observation.stop_reason.as_deref())
            ),
        }
        assert_eq!(observation.outcome, "candidate");
    }
}

#[tokio::test]
async fn missing_header_invalid_token_and_transport_error_remain_distinct() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, false).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    cycle(Arc::clone(&store), server.uri()).await;
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-codex-turn-state", "not-a-token"))
        .mount(&server)
        .await;
    cycle(Arc::clone(&store), server.uri()).await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    cycle(Arc::clone(&store), endpoint).await;
    let observations = store.turn_observations();
    assert_eq!(
        observations
            .iter()
            .map(|item| item.outcome.as_str())
            .collect::<Vec<_>>(),
        ["missing_header", "invalid_token", "transport_error"]
    );
    assert_eq!(observations[1].token_length, Some(11));
    assert_eq!(observations[2].http_status, None);
    let bucket = store
        .turn_state_bucket(
            &ProviderAccountId::new("acct_turn_probe").unwrap(),
            "gpt-5.4",
        )
        .await
        .unwrap()
        .unwrap();
    assert!(bucket.current.is_none());
    assert!(bucket.candidate.is_none());
}

#[tokio::test]
async fn probes_default_to_account_proxy_and_respect_selected_pool() {
    use gateway_core::account::OutboundProxy;
    let upstream = MockServer::start().await;
    let own = MockServer::start().await;
    let extra_a = MockServer::start().await;
    let extra_b = MockServer::start().await;
    for server in [&own, &extra_a, &extra_b] {
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
    }
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, false).await;
    let own_proxy = OutboundProxy::parse(&own.uri()).unwrap();
    store.set_egress("acct_turn_probe", Some(own_proxy.clone()), None);
    cycle(Arc::clone(&store), upstream.uri()).await;
    assert_eq!(own.received_requests().await.unwrap().len(), 1);
    assert_eq!(store.turn_observations()[0].egress, own_proxy.endpoint());
    let proxy_a = OutboundProxy::parse(&extra_a.uri()).unwrap();
    let proxy_b = OutboundProxy::parse(&extra_b.uri()).unwrap();
    store.seed_turn_proxy("proxy_a", proxy_a.clone());
    store.seed_turn_proxy("proxy_b", proxy_b.clone());
    store.seed_turn_state(
        "acct_turn_probe",
        "gpt-5.4",
        TurnStateConfig {
            enabled: true,
            include_account_proxy: false,
            proxy_ids: vec!["proxy_a".to_owned(), "proxy_b".to_owned()],
            ..TurnStateConfig::default()
        },
    );
    cycle(Arc::clone(&store), upstream.uri()).await;
    assert_eq!(own.received_requests().await.unwrap().len(), 1);
    assert_eq!(
        extra_a.received_requests().await.unwrap().len()
            + extra_b.received_requests().await.unwrap().len(),
        1
    );
    assert!(
        [proxy_a.endpoint(), proxy_b.endpoint()].contains(&store.turn_observations()[1].egress)
    );
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn manual_probe_runs_once_without_enabling_rotation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-codex-turn-state", token(217)))
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, false).await;
    store.seed_turn_state("acct_turn_probe", "gpt-5.4", TurnStateConfig::default());
    store.request_turn_probe("acct_turn_probe", "gpt-5.4");
    cycle(Arc::clone(&store), server.uri()).await;
    cycle(Arc::clone(&store), server.uri()).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let observation = store.turn_observations().pop().unwrap();
    assert_eq!(observation.probe_trigger.as_deref(), Some("manual"));
    let bucket = store
        .turn_state_bucket(
            &ProviderAccountId::new("acct_turn_probe").unwrap(),
            "gpt-5.4",
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!bucket.config.enabled);
    assert!(bucket.current.is_none());
    assert!(bucket.candidate.is_some());
    assert!(bucket.manual_probe_requested_at.is_none());
}
