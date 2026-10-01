//! 声明模型与云端打票验收：首个 `response.created` 是权威模型，票与完整
//! `__cflb`+`__oailb` pair 都合格才落候选；云端端点走 HTTPS 合同由配置层校验，
//! 这里用本地 mock 覆盖打票响应的强制死线与配对验收。

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_core::account::{CloudMintConfig, CloudMintStrategy, TurnStateStopStrategy};
use serde_json::json;
use wiremock::Respond;

use super::*;

const ACCOUNT: &str = "acct_turn_declared";
const MODEL: &str = "gpt-5.4";
const RELAY_KEY_ENV: &str = "CODEX_PROXY_TEST_RELAY_KEY";
const CASE_COMPLETED: &str = "declared-cloud-case-completed";

fn oailb(pod: &str, iat: i64, exp: i64) -> String {
    format!(
        "{}.{}.c2ln",
        URL_SAFE_NO_PAD.encode(json!({"alg":"ES256"}).to_string()),
        URL_SAFE_NO_PAD.encode(
            json!({"host":format!("chat.gateway.unified-{pod}.api.openai.com"),"iat":iat,"exp":exp})
                .to_string()
        )
    )
}

fn created_sse(model: &str, id: &str) -> String {
    format!(
        "event: response.created\ndata: {{\"type\":\"response.created\",\"response\":{{\"id\":\"{id}\",\"model\":\"{model}\"}}}}\n\n"
    )
}

fn declared_config() -> TurnStateConfig {
    TurnStateConfig {
        enabled: true,
        retry_seconds: 1,
        jitter_seconds: 0,
        budget: 10,
        stop_strategy: TurnStateStopStrategy::DeclaredModel,
        ..TurnStateConfig::default()
    }
}

async fn declared_store() -> (Arc<MemoryAccountStore>, ProviderAccountId) {
    let store = Arc::new(MemoryAccountStore::default());
    seed_named(&store, false, ACCOUNT).await;
    store.seed_turn_state(ACCOUNT, MODEL, declared_config());
    let id = ProviderAccountId::new(ACCOUNT).unwrap();
    (store, id)
}

fn declared_task(
    bundle: &mut provider_openai::ProviderBundle,
) -> (
    Box<dyn gateway_core::task::ScheduledTask>,
    gateway_core::task::WorkerId,
) {
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
    (task, registration.id)
}

async fn declared_bundle(
    store: &Arc<MemoryAccountStore>,
    server: &MockServer,
) -> provider_openai::ProviderBundle {
    let runtime = tempfile::tempdir().unwrap();
    let mut config = OpenAiConfig::default();
    config.api.base_url = server.uri();
    config.resolve_and_validate(runtime.path()).unwrap();
    provider_openai::initialize(config, turn_state_provider_ports(Arc::clone(store)))
        .await
        .unwrap()
}

async fn cycle(
    task: &dyn gateway_core::task::ScheduledTask,
    worker: &gateway_core::task::WorkerId,
) {
    task.run_cycle(WorkerCycleContext::new(
        worker.clone(),
        None,
        CancellationToken::new(),
    ))
    .await
    .unwrap();
}

fn mint_body(
    model: &str,
    ticket: Value,
    cookies: Value,
    expires_at: Value,
    gateway: Value,
) -> String {
    json!({
        "tickets": { model: ticket },
        "cookies": cookies,
        "expires_at": expires_at,
        "gateway": gateway,
    })
    .to_string()
}

fn valid_ticket(state: &str, issued: i64) -> Value {
    json!({
        "turn_state": state,
        "ticket_len": 780,
        "served_model": MODEL,
        "issued_at": issued,
        "expires_at": issued + 3600,
    })
}

fn valid_cookies(jwt: &str) -> Value {
    json!({"__cflb": "cloud-cflb", "__oailb": jwt})
}

#[tokio::test]
async fn declared_model_installs_candidate_pair_and_short_circuits_cache() {
    let server = MockServer::start().await;
    let (store, id) = declared_store().await;
    let now = Utc::now().timestamp();
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "__cflb=declared-cflb; Path=/; HttpOnly")
                .insert_header(
                    "set-cookie",
                    format!(
                        "__oailb={}; Path=/; HttpOnly",
                        oailb("185", now, now + 3600)
                    ),
                )
                .insert_header("x-codex-turn-state", token(585))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(created_sse(MODEL, "resp_declared")),
        )
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    cycle(&*task, &worker).await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    // 首轮裸打不带路由 Cookie，也不带票据。
    assert!(requests[0].headers.get("cookie").is_none());
    assert!(!requests[0].headers.contains_key("x-codex-turn-state"));
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    let current = bucket.current.expect("declared candidate installed");
    assert_eq!(current.value.len(), 780);
    let pair = bucket.installed_pair.expect("installed pair");
    assert!(pair.has_pair());
    assert!(pair.is_usable(MODEL, Utc::now().timestamp()));
    let observations = store.turn_observations();
    let probe = observations
        .iter()
        .find(|item| item.source == "probe")
        .unwrap();
    assert_eq!(probe.outcome, "candidate");
    assert_eq!(probe.stop_reason.as_deref(), Some("response_created"));
    assert_eq!(probe.stop_mode.as_deref(), Some("declared_model"));
    assert_eq!(probe.reported_model.as_deref(), Some(MODEL));
    assert!(probe.answer.is_none() && probe.answer_match.is_none());
    // 已装票命中缓存：下一轮调度不再发请求。
    server.reset().await;
    cycle(&*task, &worker).await;
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn declared_model_rejects_mismatch_missing_model_and_bad_created() {
    let server = MockServer::start().await;
    let (store, id) = declared_store().await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    let cases: [(String, &str); 4] = [
        // 首个 created 声明了别的模型：即使 HTTP 报头与请求模型一致也判 model_mismatch。
        (
            created_sse("gpt-other", "resp_a"),
            "model_mismatch",
        ),
        // created 缺 model 字段。
        (
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_b\"}}\n\n".to_owned(),
            "missing_model",
        ),
        // created 缺响应 ID。
        (
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"model\":\"gpt-5.4\"}}\n\n".to_owned(),
            "invalid_created",
        ),
        // SSE event 名与 JSON type 不符的 created 不算数。
        (
            format!("event: response.completed\ndata: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_d\",\"model\":\"{MODEL}\"}}}}\n\n"),
            "invalid_created",
        ),
    ];
    for (body, outcome) in cases {
        server.reset().await;
        let now = Utc::now().timestamp();
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    // 报头模型与请求一致；验收只认首个 created 声明。
                    .insert_header("openai-model", MODEL)
                    .insert_header("set-cookie", "__cflb=declared-cflb; Path=/")
                    .insert_header(
                        "set-cookie",
                        format!("__oailb={}; Path=/", oailb("185", now, now + 3600)),
                    )
                    .insert_header("x-codex-turn-state", token(585))
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;
        cycle(&*task, &worker).await;
        let observations = store.turn_observations();
        let last = observations.last().unwrap();
        assert_eq!(last.outcome, outcome, "body: {body}");
        assert_eq!(last.source, "probe");
        let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
        assert!(bucket.current.is_none());
        assert!(bucket.candidate.is_none());
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }
}

#[tokio::test]
async fn declared_model_header_body_conflict_resolves_to_first_created() {
    let server = MockServer::start().await;
    let (store, id) = declared_store().await;
    let now = Utc::now().timestamp();
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                // HTTP 报头谎称别的模型；created 声明与请求一致时照样验收。
                .insert_header("openai-model", "gpt-decoy")
                .insert_header("set-cookie", "__cflb=declared-cflb; Path=/")
                .insert_header(
                    "set-cookie",
                    format!("__oailb={}; Path=/", oailb("185", now, now + 3600)),
                )
                .insert_header("x-codex-turn-state", token(585))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(created_sse(MODEL, "resp_conflict")),
        )
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    cycle(&*task, &worker).await;
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    assert!(bucket.current.is_some());
    assert_eq!(
        store.turn_observations().last().unwrap().outcome,
        "candidate"
    );
}

#[tokio::test]
async fn declared_model_requires_complete_pair_and_ignores_stale_oailb_expiry() {
    let server = MockServer::start().await;
    let (store, id) = declared_store().await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    // 无 LB Cookie：缺 pair。
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-codex-turn-state", token(585))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(created_sse(MODEL, "resp_nocookie")),
        )
        .mount(&server)
        .await;
    cycle(&*task, &worker).await;
    assert_eq!(
        store.turn_observations().last().unwrap().outcome,
        "missing_cookie"
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    // 只有 __oailb 半边：同响应缺 __cflb 不构成 pair，判已发路由失效。
    server.reset().await;
    let now = Utc::now().timestamp();
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "set-cookie",
                    format!("__oailb={}; Path=/", oailb("185", now, now + 3600)),
                )
                .insert_header("x-codex-turn-state", token(585))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(created_sse(MODEL, "resp_half")),
        )
        .mount(&server)
        .await;
    cycle(&*task, &worker).await;
    assert_eq!(
        store.turn_observations().last().unwrap().outcome,
        "cookie_deleted"
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    // __oailb 的 HTTP 过期属性过期但内嵌 JWT exp 仍有效：JWT 说了算，pair 照常验收。
    server.reset().await;
    let now = Utc::now().timestamp();
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "__cflb=declared-cflb; Path=/; Max-Age=3600")
                .insert_header(
                    "set-cookie",
                    format!(
                        "__oailb={}; Path=/; Expires=Wed, 01 Jan 2020 00:00:00 GMT",
                        oailb("185", now, now + 3600)
                    ),
                )
                .insert_header("x-codex-turn-state", token(585))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(created_sse(MODEL, "resp_jwt")),
        )
        .mount(&server)
        .await;
    cycle(&*task, &worker).await;
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    assert!(
        bucket.current.is_some(),
        "valid JWT must survive stale HTTP expiry"
    );
    assert!(bucket.installed_pair.is_some());
}

#[test]
fn cloud_mint_validates_mandatory_deadlines_and_installs_pair() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_CLOUD_MINT";
    if std::env::var_os(CASE_ENV).is_some() && std::env::var_os(RELAY_KEY_ENV).is_some() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(cloud_mint_case());
        println!("\n{CASE_COMPLETED}");
        return;
    }
    // key_env 只存变量名；用隔离子进程注入测试密钥，进程内不做环境变量写入。
    let current_exe = std::env::current_exe().expect("current test binary path");
    let output = Command::new(current_exe)
        .arg("--exact")
        .arg("provider::turn_state::declared::cloud_mint_validates_mandatory_deadlines_and_installs_pair")
        .arg("--nocapture")
        .env(CASE_ENV, "1")
        .env(RELAY_KEY_ENV, "test-relay-key")
        .output()
        .expect("run isolated cloud mint case");
    assert!(
        output.status.success(),
        "isolated cloud mint case failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout
            .lines()
            .filter(|line| *line == CASE_COMPLETED)
            .count(),
        1,
        "isolated cloud mint case did not complete exactly once\nstdout:\n{stdout}"
    );
}

async fn cloud_mint_case() {
    let mint = MockServer::start().await;
    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed_named(&store, false, ACCOUNT).await;
    let config = TurnStateConfig {
        enabled: true,
        retry_seconds: 1,
        jitter_seconds: 0,
        budget: 10,
        // 端点 URL 的 HTTPS 约束由配置校验层强制；内存种子绕过校验只为
        // 用本地 mock 覆盖响应验收路径，生产配置仍拒绝非 HTTPS 端点。
        cloud_mints: vec![CloudMintConfig {
            name: "relay-a".to_owned(),
            url: mint.uri(),
            key_env: RELAY_KEY_ENV.to_owned(),
            gateway: "any".to_owned(),
            ticket_length: 780,
            ttl_seconds: 240,
            timeout_ms: 5_000,
            ..CloudMintConfig::default()
        }],
        strategy: CloudMintStrategy::RoundRobin,
        cloud_failure_threshold: 10,
        ..TurnStateConfig::default()
    };
    store.seed_turn_state(ACCOUNT, MODEL, config.clone());
    let mut bundle = declared_bundle(&store, &upstream).await;
    let (task, worker) = declared_task(&mut bundle);
    let id = ProviderAccountId::new(ACCOUNT).unwrap();
    // 声明的 issued_at 必须与票据内嵌签发时刻一致，先一次性取定再组装各用例。
    let minted = Utc::now().timestamp();
    let minted_state = token_at(585, minted);
    let jwt = oailb("185", minted, minted + 3600);
    let cases: Vec<(String, &str)> = vec![
        // 票据缺 expires_at：强制死线缺失即拒收。
        (
            mint_body(
                MODEL,
                json!({
                    "turn_state": minted_state,
                    "ticket_len": 780,
                    "served_model": MODEL,
                    "issued_at": minted,
                }),
                valid_cookies(&jwt),
                json!(minted + 3600),
                json!("unified-185"),
            ),
            "invalid_token",
        ),
        // 顶层 pair 到期缺失同样拒收。
        (
            mint_body(
                MODEL,
                valid_ticket(&minted_state, minted),
                valid_cookies(&jwt),
                Value::Null,
                json!("unified-185"),
            ),
            "missing_cookie",
        ),
        // 网关声明与 JWT pod 不一致。
        (
            mint_body(
                MODEL,
                valid_ticket(&minted_state, minted),
                valid_cookies(&jwt),
                json!(minted + 3600),
                json!("unified-999"),
            ),
            "cookie_gateway_filtered",
        ),
        // served_model 与请求不符：失败观测仍带实际声明的模型。
        (
            mint_body(
                MODEL,
                json!({
                    "turn_state": minted_state,
                    "ticket_len": 780,
                    "served_model": "gpt-decoy",
                    "issued_at": minted,
                    "expires_at": minted + 3600,
                }),
                valid_cookies(&jwt),
                json!(minted + 3600),
                json!("unified-185"),
            ),
            "model_mismatch",
        ),
        // 声明 issued_at 与票据内嵌签发时间不一致。
        (
            mint_body(
                MODEL,
                json!({
                    "turn_state": minted_state,
                    "ticket_len": 780,
                    "served_model": MODEL,
                    "issued_at": minted + 60,
                    "expires_at": minted + 3600,
                }),
                valid_cookies(&jwt),
                json!(minted + 3600),
                json!("unified-185"),
            ),
            "invalid_token",
        ),
        // ticket_len 不等于目标长度。
        (
            mint_body(
                MODEL,
                json!({
                    "turn_state": minted_state,
                    "ticket_len": 779,
                    "served_model": MODEL,
                    "issued_at": minted,
                    "expires_at": minted + 3600,
                }),
                valid_cookies(&jwt),
                json!(minted + 3600),
                json!("unified-185"),
            ),
            "invalid_length",
        ),
        // 两半路由 Cookie 全缺。
        (
            mint_body(
                MODEL,
                valid_ticket(&minted_state, minted),
                json!({}),
                json!(minted + 3600),
                json!("unified-185"),
            ),
            "missing_cookie",
        ),
        // 签发时间越过允许的 +30 秒未来窗口。
        (
            mint_body(
                MODEL,
                valid_ticket(&token_at(585, minted + 120), minted + 120),
                valid_cookies(&jwt),
                json!(minted + 3600),
                json!("unified-185"),
            ),
            "expired_or_future",
        ),
    ];
    for (body, outcome) in cases {
        mint.reset().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(body),
            )
            .mount(&mint)
            .await;
        cycle(&*task, &worker).await;
        assert_eq!(
            mint.received_requests().await.unwrap().len(),
            1,
            "每个拒绝用例都必须真实出站，避免缓存命中造成伪证"
        );
        let last = store.turn_observations().last().unwrap().clone();
        assert_eq!(last.outcome, outcome);
        assert_eq!(last.source, "cloud_mint");
        assert_eq!(last.endpoint.as_deref(), Some("relay-a"));
        assert!(last.token.is_none());
        let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
        assert!(bucket.current.is_none() && bucket.candidate.is_none());
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }
    // 失败观测保留可安全摘录的事实。
    let observations = store.turn_observations();
    let mismatched = observations
        .iter()
        .find(|item| item.outcome == "model_mismatch")
        .unwrap();
    assert_eq!(mismatched.reported_model.as_deref(), Some("gpt-decoy"));
    assert_eq!(mismatched.token_length, Some(780));
    assert!(mismatched.issued_at.is_some());
    // 完整响应：RFC3339 时间戳同样接受，成功后票与 pair 一并进候选并安装。
    mint.reset().await;
    let issued = Utc::now().timestamp();
    let state = token_at(585, issued);
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(mint_body(
                    MODEL,
                    json!({
                        "turn_state": state,
                        "ticket_len": 780,
                        "served_model": MODEL,
                        "issued_at": chrono::DateTime::from_timestamp(issued, 0)
                            .unwrap()
                            .to_rfc3339(),
                        "expires_at": chrono::DateTime::from_timestamp(issued + 3600, 0)
                            .unwrap()
                            .to_rfc3339(),
                    }),
                    valid_cookies(&oailb("185", issued, issued + 3600)),
                    json!(issued + 3600),
                    json!("unified-185"),
                )),
        )
        .mount(&mint)
        .await;
    cycle(&*task, &worker).await;
    let requests = mint.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.headers["x-relay-key"], "test-relay-key");
    assert_eq!(request.headers["x-mint-model"], MODEL);
    assert_eq!(request.headers["x-mint-transport"], "sse");
    assert_eq!(request.headers["x-mint-len"], "780");
    assert_eq!(request.headers["x-mint-ttl"], "240");
    assert_eq!(request.headers["x-relay-mint"], "any");
    assert_eq!(request.headers["chatgpt-account-id"], "upstream-probe");
    assert!(
        request.headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("Bearer ")
    );
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    let current = bucket.current.as_ref().expect("cloud candidate installed");
    assert_eq!(current.value.len(), 780);
    assert_eq!(
        bucket.current_expires_at,
        Some(issued + 240),
        "有效到期取签发+TTL 与远端死线的最小值"
    );
    let pair = bucket.installed_pair.as_ref().expect("installed pair");
    assert!(pair.has_pair());
    assert_eq!(pair.gateway_label(), "unified-185");
    assert_eq!(pair.reported_model, MODEL);
    assert!(
        bucket.installed_token(Utc::now().timestamp()).is_some(),
        "安装成功的票应按 live 实际可用"
    );
    // 本地出口全程未收到请求。
    assert!(upstream.received_requests().await.unwrap().is_empty());

    // +30 秒内的未来签发偏差可通过 mint 验收；装的票按 mint 死线可用。
    // 先重置桶，避免上一用例的已装票让本轮缓存命中、偏差响应根本没发出去。
    store.seed_turn_state(ACCOUNT, MODEL, config.clone());
    mint.reset().await;
    let skewed = Utc::now().timestamp() + 25;
    let skewed_state = token_at(585, skewed);
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(mint_body(
                    MODEL,
                    valid_ticket(&skewed_state, skewed),
                    // JWT iat 留在过去：pair 校验不接受未来签发，偏差只验票据本体。
                    valid_cookies(&oailb("185", skewed - 30, skewed + 3600)),
                    json!(skewed + 3600),
                    json!("unified-185"),
                )),
        )
        .mount(&mint)
        .await;
    cycle(&*task, &worker).await;
    let requests = mint.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "桶重置后偏差用例必须真实出站打票");
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    let token = bucket
        .installed_token(Utc::now().timestamp())
        .expect("30 秒内的签发偏差应通过 mint 验收并实际可用");
    assert_eq!(token.issued_at, skewed, "安装的票应保留票据内嵌签发时刻");

    // 有效到期取签发+TTL、票据声明、顶层 pair 到期与 JWT exp 的最小值；
    // 逐一让其余三方成为最早死线。
    for (ticket_secs, pair_secs, jwt_secs, expected_secs) in [
        (150_i64, 3600_i64, 3600_i64, 150_i64),
        (3600, 140, 3600, 140),
        (3600, 3600, 130, 130),
    ] {
        store.seed_turn_state(ACCOUNT, MODEL, config.clone());
        mint.reset().await;
        let issued = Utc::now().timestamp();
        let state = token_at(585, issued);
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(mint_body(
                        MODEL,
                        json!({
                            "turn_state": state,
                            "ticket_len": 780,
                            "served_model": MODEL,
                            "issued_at": issued,
                            "expires_at": issued + ticket_secs,
                        }),
                        valid_cookies(&oailb("185", issued, issued + jwt_secs)),
                        json!(issued + pair_secs),
                        json!("unified-185"),
                    )),
            )
            .mount(&mint)
            .await;
        cycle(&*task, &worker).await;
        let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
        assert!(
            bucket.installed_token(Utc::now().timestamp()).is_some(),
            "最早死线 {expected_secs} 的票据应实际可安装"
        );
        assert_eq!(bucket.current_expires_at, Some(issued + expected_secs));
        assert_eq!(
            bucket.installed_pair.as_ref().unwrap().reported_model,
            MODEL,
            "绑定 pair 的声明模型应等于请求模型"
        );
    }

    // 随机策略在可用端点中选一个：一次打票只落到一个端点。
    let mint_b = MockServer::start().await;
    let mut rr = config.clone();
    rr.cloud_mints.push(CloudMintConfig {
        name: "relay-b".to_owned(),
        url: mint_b.uri(),
        key_env: RELAY_KEY_ENV.to_owned(),
        ticket_length: 780,
        ..CloudMintConfig::default()
    });
    // 新 bundle 让轮询游标从零开始，轮转断言不依赖前面用例消耗过的序号。
    let mut bundle = declared_bundle(&store, &upstream).await;
    let (task, worker) = declared_task(&mut bundle);
    mint.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&mint)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&mint_b)
        .await;
    let mut random_pick = rr.clone();
    random_pick.strategy = CloudMintStrategy::Random;
    store.seed_turn_state(ACCOUNT, MODEL, random_pick);
    cycle(&*task, &worker).await;
    let spread = mint.received_requests().await.unwrap().len()
        + mint_b.received_requests().await.unwrap().len();
    assert_eq!(spread, 1, "随机选择每轮只命中一个可用端点");
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // 轮询策略在两个可用端点之间确定轮转；400 只出局当前模型，一轮一次出站。
    mint.reset().await;
    mint_b.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&mint)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&mint_b)
        .await;
    store.seed_turn_state(ACCOUNT, MODEL, rr);
    for _ in 0..3 {
        cycle(&*task, &worker).await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }
    assert_eq!(mint.received_requests().await.unwrap().len(), 2);
    assert_eq!(mint_b.received_requests().await.unwrap().len(), 1);

    // 连续失败进入冷却：成功后清零计数，冷却中回落本地，冷却到期恢复资格。
    let mut bundle = declared_bundle(&store, &upstream).await;
    let (task, worker) = declared_task(&mut bundle);
    let mut cooled = config.clone();
    cooled.cloud_failure_threshold = 2;
    cooled.cloud_cooldown_seconds = 3;
    store.seed_turn_state(ACCOUNT, MODEL, cooled.clone());
    mint.reset().await;
    upstream.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&mint)
        .await;
    cycle(&*task, &worker).await;
    assert_eq!(mint.received_requests().await.unwrap().len(), 1);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    // 一次成功把失败计数清零：否则后续两次失败合计已越过阈值。
    mint.reset().await;
    let issued = Utc::now().timestamp();
    let state = token_at(585, issued);
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(mint_body(
                    MODEL,
                    valid_ticket(&state, issued),
                    valid_cookies(&oailb("185", issued, issued + 3600)),
                    json!(issued + 3600),
                    json!("unified-185"),
                )),
        )
        .mount(&mint)
        .await;
    cycle(&*task, &worker).await;
    assert!(
        store
            .turn_state_bucket(&id, MODEL)
            .await
            .unwrap()
            .unwrap()
            .current
            .is_some()
    );
    store.seed_turn_state(ACCOUNT, MODEL, cooled.clone());
    mint.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&mint)
        .await;
    cycle(&*task, &worker).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    cycle(&*task, &worker).await;
    assert_eq!(
        mint.received_requests().await.unwrap().len(),
        2,
        "成功清零后两次失败才应达到冷却阈值"
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    // 冷却未到期：本轮不再出站云端，回落本地声明模型探测出口。
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: Vec::new(),
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&upstream)
        .await;
    cycle(&*task, &worker).await;
    assert_eq!(upstream.received_requests().await.unwrap().len(), 1);
    // 冷却到期后端点恢复资格。
    store.seed_turn_state(ACCOUNT, MODEL, cooled.clone());
    mint.reset().await;
    tokio::time::sleep(Duration::from_millis(2200)).await;
    let issued = Utc::now().timestamp();
    let state = token_at(585, issued);
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(mint_body(
                    MODEL,
                    valid_ticket(&state, issued),
                    valid_cookies(&oailb("185", issued, issued + 3600)),
                    json!(issued + 3600),
                    json!("unified-185"),
                )),
        )
        .mount(&mint)
        .await;
    cycle(&*task, &worker).await;
    assert_eq!(
        mint.received_requests().await.unwrap().len(),
        1,
        "冷却到期后应重新选择云端端点"
    );
    assert!(
        store
            .turn_state_bucket(&id, MODEL)
            .await
            .unwrap()
            .unwrap()
            .installed_token(Utc::now().timestamp())
            .is_some()
    );
}

#[tokio::test]
async fn cloud_endpoint_cooldown_falls_back_to_local_declared_probe() {
    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed_named(&store, false, ACCOUNT).await;
    let mut config = declared_config();
    config.cloud_mints = vec![CloudMintConfig {
        name: "relay-off".to_owned(),
        url: "https://relay.invalid/mint".to_owned(),
        // 环境里不存在的变量名：端点本轮不可用，不发出任何网络请求。
        key_env: "CODEX_PROXY_TEST_UNSET_RELAY_KEY".to_owned(),
        ticket_length: 780,
        ..CloudMintConfig::default()
    }];
    config.cloud_failure_threshold = 2;
    config.cloud_cooldown_seconds = 600;
    store.seed_turn_state(ACCOUNT, MODEL, config);
    let mut bundle = declared_bundle(&store, &upstream).await;
    let (task, worker) = declared_task(&mut bundle);
    let id = ProviderAccountId::new(ACCOUNT).unwrap();
    for _ in 0..2 {
        cycle(&*task, &worker).await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }
    let failures = store.turn_observations();
    assert!(
        failures
            .iter()
            .all(|item| item.outcome == "endpoint_unavailable")
    );
    // 连续失败到阈值后端点进入冷却：下一轮回落本地声明模型探测出口。
    upstream.reset().await;
    let now = Utc::now().timestamp();
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "__cflb=fallback-cflb; Path=/")
                .insert_header(
                    "set-cookie",
                    format!("__oailb={}; Path=/", oailb("185", now, now + 3600)),
                )
                .insert_header("x-codex-turn-state", token(585))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(created_sse(MODEL, "resp_fallback")),
        )
        .mount(&upstream)
        .await;
    cycle(&*task, &worker).await;
    assert_eq!(upstream.received_requests().await.unwrap().len(), 1);
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    assert!(bucket.current.is_some());
    assert!(bucket.installed_pair.is_some());
}

/// 按序返回预设失败状态，耗尽后按请求体里的模型回合格 declared 响应。
/// `cookies` 控制成功响应是否携带 `__cflb`+`__oailb` pair。
struct ProbeResponder {
    failures: Vec<u16>,
    cookies: bool,
    next: AtomicUsize,
}

impl Respond for ProbeResponder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let index = self.next.fetch_add(1, Ordering::Relaxed);
        if let Some(status) = self.failures.get(index) {
            return ResponseTemplate::new(*status);
        }
        // 探测请求体是 zstd 压缩的 JSON；解码失败时按默认模型应答。
        let model = zstd::stream::decode_all(std::io::Cursor::new(&request.body))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|body| body["model"].as_str().map(str::to_owned))
            .unwrap_or_else(|| MODEL.to_owned());
        let now = Utc::now().timestamp();
        let mut response = ResponseTemplate::new(200);
        if self.cookies {
            response = response
                .insert_header("set-cookie", "__cflb=declared-cflb; Path=/; HttpOnly")
                .insert_header(
                    "set-cookie",
                    format!(
                        "__oailb={}; Path=/; HttpOnly",
                        oailb("185", now, now + 3600)
                    ),
                );
        }
        response
            .insert_header("x-codex-turn-state", token(585))
            .insert_header("content-type", "text/event-stream")
            .set_body_string(created_sse(&model, "resp_seq"))
    }
}

/// 429/5xx 在同一账号任务的预算与死线内按 retry_seconds 重试，成功票照常安装。
#[tokio::test]
async fn declared_worker_retries_429_within_the_same_account_task() {
    let server = MockServer::start().await;
    let (store, id) = declared_store().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: vec![429],
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    cycle(&*task, &worker).await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "429 应在同一账号任务内重试");
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    assert!(bucket.current.is_some());
    let observations = store.turn_observations();
    assert_eq!(observations[0].outcome, "http_error");
    assert_eq!(observations[0].http_status, Some(429));
    assert_eq!(observations.last().unwrap().outcome, "candidate");
}

/// 同账号多模型串行打票：首个请求裸打，后续模型复用会话内的种子 pair。
#[tokio::test]
async fn declared_worker_serializes_models_and_reuses_the_seed_pair() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed_named(&store, false, ACCOUNT).await;
    store.seed_turn_state(ACCOUNT, MODEL, declared_config());
    store.seed_turn_state(ACCOUNT, "gpt-5.5", declared_config());
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: Vec::new(),
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    cycle(&*task, &worker).await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].headers.get("cookie").is_none());
    let cookie = requests[1].headers["cookie"].to_str().unwrap().to_owned();
    assert!(cookie.contains("__cflb=declared-cflb"));
    assert!(cookie.contains("__oailb="));
    let id = ProviderAccountId::new(ACCOUNT).unwrap();
    for model in [MODEL, "gpt-5.5"] {
        let bucket = store.turn_state_bucket(&id, model).await.unwrap().unwrap();
        assert!(bucket.current.is_some(), "{model} should be installed");
        assert!(bucket.installed_pair.is_some());
    }
}

/// 401/403 中止整个账号任务：首个请求被拒后其余模型不再出站。
#[tokio::test]
async fn declared_worker_403_aborts_the_whole_account_task() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed_named(&store, false, ACCOUNT).await;
    store.seed_turn_state(ACCOUNT, MODEL, declared_config());
    store.seed_turn_state(ACCOUNT, "gpt-5.5", declared_config());
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: vec![403],
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    cycle(&*task, &worker).await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "账号中止后不得再出站");
    let observations = store.turn_observations();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].outcome, "account_ended");
    assert_eq!(observations[0].http_status, Some(403));
    let id = ProviderAccountId::new(ACCOUNT).unwrap();
    for model in [MODEL, "gpt-5.5"] {
        let bucket = store.turn_state_bucket(&id, model).await.unwrap().unwrap();
        assert!(bucket.current.is_none());
        assert!(bucket.candidate.is_none());
    }
}

/// 400/404/422 只出局当前模型：同账号其余模型在同一任务内照常打票。
#[tokio::test]
async fn declared_worker_404_excludes_only_that_model() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed_named(&store, false, ACCOUNT).await;
    store.seed_turn_state(ACCOUNT, MODEL, declared_config());
    store.seed_turn_state(ACCOUNT, "gpt-5.5", declared_config());
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: vec![404],
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    cycle(&*task, &worker).await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "本模型出局不阻断同任务下一模型");
    let id = ProviderAccountId::new(ACCOUNT).unwrap();
    let mut installed = 0;
    for model in [MODEL, "gpt-5.5"] {
        if store
            .turn_state_bucket(&id, model)
            .await
            .unwrap()
            .unwrap()
            .current
            .is_some()
        {
            installed += 1;
        }
    }
    assert_eq!(installed, 1, "只有一个模型应被排除");
    let observations = store.turn_observations();
    assert_eq!(observations[0].outcome, "http_error");
    assert_eq!(observations[0].http_status, Some(404));
    assert_eq!(observations[1].outcome, "candidate");
}

/// 票过期但已装 pair 仍有效：下一次探测直接带 pair 定向打票；
/// 响应不带新 LB Cookie 时静默沿用已发 pair。
#[tokio::test]
async fn declared_worker_expired_ticket_reprobes_with_the_installed_pair() {
    let server = MockServer::start().await;
    let (store, id) = declared_store().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: Vec::new(),
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    cycle(&*task, &worker).await;
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    let pair = bucket.installed_pair.clone().expect("installed pair");
    assert!(pair.has_pair());
    // 票过期、pair 仍有效：下一轮直接用已装 pair 定向打票，不再裸打。
    store.set_current_turn_state(
        ACCOUNT,
        MODEL,
        gateway_core::account::TurnStateToken::parse(&token_at(585, Utc::now().timestamp() - 1000))
            .unwrap(),
        false,
    );
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            // 只回 __cf_bm 不构成 pair 删除，也不制造新路由凭据：已发 pair 继续沿用。
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "__cf_bm=reused-cf-bm; Path=/; HttpOnly")
                .insert_header("x-codex-turn-state", token(585))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(created_sse(MODEL, "resp_reuse")),
        )
        .mount(&server)
        .await;
    cycle(&*task, &worker).await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].headers["cookie"].to_str().unwrap(),
        pair.header()
    );
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    let current = bucket.current.expect("reprobed ticket");
    assert!(current.is_fresh(Utc::now().timestamp(), bucket.config.ttl_seconds));
}

/// 重铸到其它 pod 且只回了半截 LB Cookie：已发 pair 按失效处理，
/// 半边上不了池、也不参与新候选。
#[tokio::test]
async fn declared_worker_reminted_partial_cookie_invalidates_the_sent_pair() {
    let server = MockServer::start().await;
    let (store, id) = declared_store().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: Vec::new(),
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    cycle(&*task, &worker).await;
    assert!(!store.routing_cookies().await.unwrap().is_empty());
    store.set_current_turn_state(
        ACCOUNT,
        MODEL,
        gateway_core::account::TurnStateToken::parse(&token_at(585, Utc::now().timestamp() - 1000))
            .unwrap(),
        false,
    );
    server.reset().await;
    let now = Utc::now().timestamp();
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                // 换 pod 只回 __oailb：同响应缺 __cflb，判已发 pair 失效。
                .insert_header(
                    "set-cookie",
                    format!("__oailb={}; Path=/", oailb("999", now, now + 3600)),
                )
                .insert_header("x-codex-turn-state", token(585))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(created_sse(MODEL, "resp_remint")),
        )
        .mount(&server)
        .await;
    cycle(&*task, &worker).await;
    assert_eq!(
        store.turn_observations().last().unwrap().outcome,
        "cookie_deleted"
    );
    assert!(
        store
            .routing_cookies()
            .await
            .unwrap()
            .iter()
            .all(|cookie| cookie.pod != "chat.gateway.unified-185.api.openai.com"),
        "已发 pair 的池记录应随失效移除"
    );
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    assert!(bucket.candidate.is_none());
}

/// 同账号同键并发调度合并为一次出站：等待方直接复用已完成结果。
#[tokio::test]
async fn declared_worker_coalesces_concurrent_cycles_into_one_outbound() {
    let server = MockServer::start().await;
    let (store, id) = declared_store().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: Vec::new(),
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    let context = || WorkerCycleContext::new(worker.clone(), None, CancellationToken::new());
    let (first, second) = tokio::join!(task.run_cycle(context()), task.run_cycle(context()));
    first.unwrap();
    second.unwrap();
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "同键并发只允许一次出站"
    );
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    assert!(bucket.current.is_some());
}

/// 声明模型 + Cookie 锁定：业务请求注入已安装票与其绑定的完整 pair，
/// 缓存命中时下一轮探测不再出站。
#[tokio::test]
async fn declared_business_injects_installed_ticket_and_bound_pair() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed_named(&store, false, ACCOUNT).await;
    let mut config = declared_config();
    config.cookie_lock_enabled = true;
    store.seed_turn_state(ACCOUNT, MODEL, config);
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: Vec::new(),
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    cycle(&*task, &worker).await;
    let id = ProviderAccountId::new(ACCOUNT).unwrap();
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    let now = Utc::now().timestamp();
    let installed = bucket
        .installed_token(now)
        .expect("usable installed token")
        .value
        .clone();
    let pair = bucket.routing_cookie(now).expect("installed bound pair");
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_business\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[]}}\n\n"),
        )
        .mount(&server)
        .await;
    let payload = ProtocolPayload::json_object(
        "openai",
        json!({"model": MODEL, "input": "Hello"})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));
    let mut stream = bundle
        .core_provider()
        .execute(
            initialized_provider_request(operation, ACCOUNT),
            initialized_attempt_context("req_declared_business", ACCOUNT),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].headers["x-codex-turn-state"], installed);
    assert_eq!(
        requests[0].headers["cookie"].to_str().unwrap(),
        pair.header()
    );
    // 已安装票未到期：下一轮探测命中缓存，不再出站。
    server.reset().await;
    cycle(&*task, &worker).await;
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "缓存命中不应再发出探测请求"
    );
}

/// 同键并发且首次尝试失败（非重试类状态码）：等待方复用精确的失败结果，
/// 全程只有一次出站。
#[tokio::test]
async fn declared_worker_coalesces_concurrent_failed_cycles() {
    let server = MockServer::start().await;
    let (store, _id) = declared_store().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: vec![400],
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    let context = || WorkerCycleContext::new(worker.clone(), None, CancellationToken::new());
    let (first, second) = tokio::join!(task.run_cycle(context()), task.run_cycle(context()));
    first.unwrap();
    second.unwrap();
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "并发失败尝试只允许一次出站"
    );
    assert_eq!(store.turn_observations().len(), 1);
    assert_eq!(store.turn_observations()[0].http_status, Some(400));
}

/// 迟到的旧 pair 失效事件按精确值与请求开始水位围栏：同 pod 的更新凭据
/// 和水位更新的同值 pair 都不得被误删。
#[tokio::test]
async fn stale_pair_invalidation_preserves_newer_pool_records() {
    use gateway_core::account::{RoutingCookie, RoutingCookieObservation};
    let store = MemoryAccountStore::default();
    let now = Utc::now().timestamp();
    let pair_at = |value: &str, cflb: &str| {
        RoutingCookie::parse("origin", "__oailb", value, now)
            .unwrap()
            .with_cflb("__cflb", cflb, Some(now + 3600))
            .unwrap()
    };
    let newer = pair_at(&oailb("185", now - 5, now + 3600), "cflb-newer");
    store
        .observe_routing_cookie(RoutingCookieObservation {
            origin: "origin".to_owned(),
            observed_at: now * 1000,
            sent: None,
            received: Some(newer.clone()),
            reported_model: Some(MODEL.to_owned()),
            deleted: false,
        })
        .await
        .unwrap();
    // 同 pod 但值不同的旧 pair 删除：池记录值不符，不动。
    let older = pair_at(&oailb("185", now - 3600, now + 60), "cflb-older");
    store
        .observe_routing_cookie(RoutingCookieObservation {
            origin: "origin".to_owned(),
            observed_at: (now - 1) * 1000,
            sent: Some(older),
            received: None,
            reported_model: None,
            deleted: true,
        })
        .await
        .unwrap();
    let pool = store.routing_cookies().await.unwrap();
    assert_eq!(pool.len(), 1);
    assert_eq!(pool[0].cflb_value, "cflb-newer");
    // 同值 pair 但水位更旧的删除：观察水位不够新，仍不移除。
    store
        .observe_routing_cookie(RoutingCookieObservation {
            origin: "origin".to_owned(),
            observed_at: now * 1000 - 1,
            sent: Some(newer.clone()),
            received: None,
            reported_model: None,
            deleted: true,
        })
        .await
        .unwrap();
    assert_eq!(store.routing_cookies().await.unwrap().len(), 1);
    // 水位够新的同值删除才真正移除。
    store
        .observe_routing_cookie(RoutingCookieObservation {
            origin: "origin".to_owned(),
            observed_at: now * 1000 + 1,
            sent: Some(newer),
            received: None,
            reported_model: None,
            deleted: true,
        })
        .await
        .unwrap();
    assert!(store.routing_cookies().await.unwrap().is_empty());
}

/// 桶上绑定的安装 pair 同样按精确值+请求水位围栏：旧水位的失效事件不得摘除
/// 水位更新的新绑定；只有同值且水位更新的失效才解绑。
#[tokio::test]
async fn stale_pair_invalidation_preserves_newer_installed_binding() {
    use gateway_core::account::{RoutingCookie, RoutingCookieObservation};
    let store = MemoryAccountStore::default();
    seed_named(&store, false, ACCOUNT).await;
    store.seed_turn_state(ACCOUNT, MODEL, declared_config());
    let id = ProviderAccountId::new(ACCOUNT).unwrap();
    let now = Utc::now().timestamp();
    let mut pair =
        RoutingCookie::parse("origin", "__oailb", &oailb("185", now - 5, now + 3600), now)
            .unwrap()
            .with_cflb("__cflb", "binding-cflb", Some(now + 3600))
            .unwrap();
    pair.observed_at = now * 1000;
    store.set_installed_pair(ACCOUNT, MODEL, pair.clone());
    // 同值旧水位失效：桶上绑定记录的观测水位更新，摘除请求不得生效。
    store
        .observe_routing_cookie(RoutingCookieObservation {
            origin: "origin".to_owned(),
            observed_at: now * 1000 - 1,
            sent: Some(pair.clone()),
            received: None,
            reported_model: None,
            deleted: true,
        })
        .await
        .unwrap();
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    assert!(bucket.installed_pair.is_some());
    // 同值且水位够新的失效才真正解绑。
    store
        .observe_routing_cookie(RoutingCookieObservation {
            origin: "origin".to_owned(),
            observed_at: now * 1000 + 1,
            sent: Some(pair),
            received: None,
            reported_model: None,
            deleted: true,
        })
        .await
        .unwrap();
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    assert!(bucket.installed_pair.is_none());
}

/// Cookie 锁定叠加声明模型但关闭自动打票：注入早期返回只适用于纯 Cookie 模式，
/// 手动安装的票与其绑定 pair 仍照常注入；调度运行也不应触发打票。
#[tokio::test]
async fn declared_cookie_lock_disabled_still_injects_manual_ticket_and_pair() {
    use gateway_core::account::RoutingCookie;
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed_named(&store, false, ACCOUNT).await;
    let mut config = declared_config();
    config.enabled = false;
    config.cookie_lock_enabled = true;
    store.seed_turn_state(ACCOUNT, MODEL, config);
    let now = Utc::now().timestamp();
    let installed = token_at(585, now);
    store.set_current_turn_state(
        ACCOUNT,
        MODEL,
        gateway_core::account::TurnStateToken::parse(&installed).unwrap(),
        true,
    );
    let mut pair =
        RoutingCookie::parse("origin", "__oailb", &oailb("185", now - 5, now + 3600), now)
            .unwrap()
            .with_cflb("__cflb", "binding-cflb", Some(now + 3600))
            .unwrap();
    pair.reported_model = MODEL.to_owned();
    store.set_installed_pair(ACCOUNT, MODEL, pair.clone());
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_manual\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[]}}\n\n"),
        )
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    let payload = ProtocolPayload::json_object(
        "openai",
        json!({"model": MODEL, "input": "Hello"})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));
    let mut stream = bundle
        .core_provider()
        .execute(
            initialized_provider_request(operation, ACCOUNT),
            initialized_attempt_context("req_declared_manual_pair", ACCOUNT),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].headers["x-codex-turn-state"], installed);
    assert_eq!(
        requests[0].headers["cookie"].to_str().unwrap(),
        pair.header()
    );
    // enabled=false：跑一轮调度也不应产生任何出站打票。
    server.reset().await;
    cycle(&*task, &worker).await;
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "自动打票已关闭，调度不应发出探测请求"
    );
}

/// 业务响应首个 created 声明了别的模型：即使 HTTP 模型头与请求一致、响应也不带
/// 新票或新 Cookie，已发送票与绑定 pair 一律作废并移出共享池。
#[tokio::test]
async fn declared_business_created_mismatch_revokes_ticket_and_pair() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed_named(&store, false, ACCOUNT).await;
    let mut config = declared_config();
    config.cookie_lock_enabled = true;
    store.seed_turn_state(ACCOUNT, MODEL, config);
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: Vec::new(),
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let mut bundle = declared_bundle(&store, &server).await;
    let (task, worker) = declared_task(&mut bundle);
    cycle(&*task, &worker).await;
    let id = ProviderAccountId::new(ACCOUNT).unwrap();
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    assert!(bucket.installed_token(Utc::now().timestamp()).is_some());
    assert!(!store.routing_cookies().await.unwrap().is_empty());
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                // 报头模型与请求一致：验收权威是首个 created 的声明。
                .insert_header("openai-model", MODEL)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!(
                    "{}{}",
                    created_sse("gpt-decoy", "resp_decoy"),
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_decoy\",\"model\":\"gpt-decoy\",\"status\":\"completed\",\"output\":[]}}\n\n"
                )),
        )
        .mount(&server)
        .await;
    let payload = ProtocolPayload::json_object(
        "openai",
        json!({"model": MODEL, "input": "Hello"})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));
    let mut stream = bundle
        .core_provider()
        .execute(
            initialized_provider_request(operation, ACCOUNT),
            initialized_attempt_context("req_declared_mismatch", ACCOUNT),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    let bucket = store.turn_state_bucket(&id, MODEL).await.unwrap().unwrap();
    assert!(bucket.current.is_none(), "created 模型不符必须作废已安装票");
    assert!(bucket.installed_pair.is_none());
    assert!(
        store.routing_cookies().await.unwrap().is_empty(),
        "作废的 pair 应移出共享池"
    );
    // 标坏的 pair 同时从账号闸的共享种子表摘除：下一轮打票只能裸打，
    // 不得再携带已作废的 pair 定向到坏节点。
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ProbeResponder {
            failures: Vec::new(),
            cookies: true,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    cycle(&*task, &worker).await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let replayed = requests[0]
        .headers
        .get("cookie")
        .map(|cookie| cookie.to_str().unwrap_or_default().to_owned())
        .unwrap_or_default();
    assert!(
        !replayed.contains("__oailb") && !replayed.contains("declared-cflb"),
        "失效 pair 不得继续作为打票种子"
    );
}
