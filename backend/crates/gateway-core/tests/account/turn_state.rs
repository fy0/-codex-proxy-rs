use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use gateway_core::account::{
    CloudMintStrategy, CloudMintTransport, MissingTurnStatePolicy, TurnStateBucket,
    TurnStateConfig, TurnStateStopStrategy, TurnStateToken,
};

fn encoded(issued_at: u64, size: usize) -> String {
    let mut bytes = vec![0; size];
    bytes[0] = 0x80;
    bytes[1..9].copy_from_slice(&issued_at.to_be_bytes());
    URL_SAFE.encode(bytes)
}

#[test]
fn feishu_webhook_accepts_only_official_https_bot_endpoints() {
    for (url, valid) in [
        ("", true),
        (
            "https://open.feishu.cn/open-apis/bot/v2/hook/test-bot",
            true,
        ),
        (
            "https://open.larksuite.com/open-apis/bot/v2/hook/test-bot",
            true,
        ),
        (
            "http://open.feishu.cn/open-apis/bot/v2/hook/test-bot",
            false,
        ),
        (
            "https://open.feishu.cn.attacker.invalid/open-apis/bot/v2/hook/test-bot",
            false,
        ),
        (
            "https://user@open.feishu.cn/open-apis/bot/v2/hook/test-bot",
            false,
        ),
        (
            "https://open.feishu.cn/open-apis/bot/v2/hook/test-bot?redirect=other",
            false,
        ),
        (
            "https://open.feishu.cn/open-apis/bot/v2/hook/test-bot#fragment",
            false,
        ),
        ("https://open.feishu.cn/open-apis/bot/v2/hook/", false),
    ] {
        let config = TurnStateConfig {
            feishu_webhook_url: url.to_owned(),
            ..TurnStateConfig::default()
        };
        assert_eq!(config.is_valid(), valid);
    }
}

#[test]
fn embedded_timestamp_is_big_endian_and_age_never_uses_observation_time() {
    let value = encoded(1_800_000_000, 217);
    assert_eq!(value.len(), 292);
    let token = TurnStateToken::parse(&value).unwrap();
    assert_eq!(token.issued_at, 1_800_000_000);
    assert!(token.is_fresh(1_800_001_799, 1800));
    assert!(!token.is_fresh(1_800_001_800, 1800));
    assert!(!token.is_fresh(1_800_003_720, 2700));
    assert!(!token.is_fresh(1_799_999_999, 2700));
    assert!(token.is_newer_than(None));
    assert!(token.is_newer_than(Some(1_799_999_999)));
    assert!(!token.is_newer_than(Some(token.issued_at)));
    assert!(!token.is_newer_than(Some(token.issued_at + 1)));
    assert!(!format!("{token:?}").contains(&value));
}

#[test]
fn all_lengths_are_parseable_without_treating_errors_as_tokens() {
    for (size, length) in [(217, 292), (233, 312), (414, 552), (585, 780)] {
        let value = encoded(1_800_000_000, size);
        assert_eq!(value.len(), length);
        assert!(TurnStateToken::parse(&value).is_some());
        assert_eq!(
            value.len() == TurnStateConfig::default().target_length,
            length == 780
        );
    }
    for value in [
        "transport error".to_owned(),
        "x".repeat(292),
        " ".repeat(292),
        encoded(u64::MAX, 217),
        format!("{}\n", encoded(1_800_000_000, 217)),
    ] {
        assert!(TurnStateToken::parse(&value).is_none());
    }
    let mut invalid_version = vec![0; 217];
    invalid_version[0] = 0x81;
    assert!(TurnStateToken::parse(&URL_SAFE.encode(invalid_version)).is_none());
}

#[test]
fn rotation_config_bounds_ttl_budget_and_proxy_selection() {
    let mut config = TurnStateConfig::default();
    assert!(config.is_valid());
    assert_eq!(config.ttl_seconds, 240);
    assert_eq!(config.refresh_after_seconds, 120);
    assert!(!config.detect_actual_model);
    assert!(!config.revokes_ticket_when_model_detaches());
    config.detect_actual_model = true;
    // 与缺票暂停策略解耦：实际模型脱离请求模型就作废已发出去的票。
    assert!(config.revokes_ticket_when_model_detaches());
    config.missing_state_policy = gateway_core::account::MissingTurnStatePolicy::Pause;
    assert!(config.revokes_ticket_when_model_detaches());
    assert!(config.include_account_proxy);
    assert!(!config.include_direct);
    config.refresh_after_seconds = config.ttl_seconds;
    assert!(!config.is_valid());
    config.refresh_after_seconds = config.ttl_seconds - 1;
    config.include_account_proxy = false;
    assert!(!config.is_valid());
    config.proxy_ids = vec!["proxy_test".to_owned()];
    assert!(config.is_valid());
    config.budget = 0;
    assert!(!config.is_valid());
}

#[test]
fn cookie_gateway_ids_accept_pipe_separated_decimal_ids_only() {
    let mut config = TurnStateConfig::default();
    assert!(config.cookie_gateway_ids_valid());
    config.cookie_gateway_ids = "111|222|333".to_owned();
    assert!(config.is_valid());
    assert!(config.allows_cookie_gateway("chat.gateway.unified-222.api.openai.com"));
    assert!(!config.allows_cookie_gateway("chat.gateway.unified-444.api.openai.com"));
    for invalid in ["111||222", "111| 222", "unified-111", "111|abc"] {
        config.cookie_gateway_ids = invalid.to_owned();
        assert!(!config.is_valid());
    }
}

#[test]
fn probe_persona_defaults_are_compatible_and_reject_header_injection() {
    let mut config: TurnStateConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(config.originator, "codex-tui");
    assert_eq!(config.client_version, "0.154.0");
    assert!(config.user_agent.is_empty());
    for version in [
        "0.154.0-alpha.1",
        "0.154.0-beta.1",
        "0.154.0-rc.1",
        "0.154.0+local",
        "v0.154.0",
        "",
    ] {
        config.client_version = version.to_owned();
        assert!(!config.is_valid());
    }
    config.client_version = "0.153.0".to_owned();
    config.originator = "Custom Probe".to_owned();
    config.user_agent = "Custom Probe/1.0".to_owned();
    assert!(config.is_valid());
    for invalid in [
        "\r\nx-test: injected".to_owned(),
        " ".to_owned(),
        "x".repeat(1025),
    ] {
        config.user_agent = invalid;
        assert!(!config.is_valid());
    }
    config.user_agent.clear();
    for invalid in ["".to_owned(), "\n".to_owned(), "x".repeat(129)] {
        config.originator = invalid;
        assert!(!config.is_valid());
    }
    assert!(serde_json::from_str::<TurnStateConfig>(r#"{"timezone":"not-a-timezone"}"#).is_err());
}

#[test]
fn missing_state_policy_defaults_to_allow_and_requires_an_installed_matching_ticket() {
    let now = 1_800_000_000;
    let account = super::account("acct_state");
    let mut bucket = TurnStateBucket {
        account_id: account.id().as_str().to_owned(),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().map(str::to_owned),
        model: "upstream-model".to_owned(),
        config: serde_json::from_str("{}").unwrap(),
        routing_cookies: Vec::new(),
        cookie_override_pod: None,
        cookie_override_issued_at: None,
        cookie_override_name: None,
        cookie_override_value: None,
        cookie_override_expires_at: None,
        cookie_override_observation_id: None,
        cookie_override_cflb_name: None,
        cookie_override_cflb_value: None,
        current: None,
        current_issued_at: None,
        current_expires_at: None,
        installed_pair: None,
        current_length: None,
        candidate: Some(TurnStateToken::parse(&encoded(now as u64, 585)).unwrap()),
        candidate_expires_at: None,
        candidate_pair: None,
        hunt_attempts: 0,
        next_probe_at: None,
        manual_probe_requested_at: None,
        manual_override: false,
        attached_model: None,
    };
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(now as u64);
    assert_eq!(
        bucket.config.missing_state_policy,
        MissingTurnStatePolicy::Allow
    );
    assert!(
        bucket
            .scheduling_availability(&account, &bucket.model, now)
            .allows(time)
    );
    bucket.config.missing_state_policy = MissingTurnStatePolicy::Pause;
    assert!(bucket.manages_injection());
    assert!(
        !bucket
            .scheduling_availability(&account, &bucket.model, now)
            .allows(time)
    );
    bucket.current = bucket.candidate.take();
    bucket.current_issued_at = Some(now);
    bucket.current_length = Some(780);
    assert!(
        bucket
            .scheduling_availability(&account, &bucket.model, now)
            .allows(time)
    );
    // 探测开关与安装来源不改变有票事实。
    assert!(!bucket.config.enabled);
    assert!(!bucket.manual_override);
    for (model, other) in [
        (&bucket.model[..], super::account("acct_other")),
        ("alias", account.clone()),
    ] {
        assert!(
            !bucket
                .scheduling_availability(&other, model, now)
                .allows(time)
        );
    }
    bucket.upstream_user_id = Some("different-user".to_owned());
    assert!(
        !bucket
            .scheduling_availability(&account, &bucket.model, now)
            .allows(time)
    );
    bucket.upstream_user_id = account.upstream_user_id().map(str::to_owned);
    let ttl = i64::try_from(bucket.config.ttl_seconds).unwrap();
    assert!(bucket.installed_token(now + ttl - 1).is_some());
    assert!(bucket.installed_token(now + ttl).is_none());
    assert!(bucket.installed_token(now - 1).is_none());
    bucket.current = Some(TurnStateToken::parse(&encoded(now as u64, 233)).unwrap());
    assert!(bucket.installed_token(now).is_none());
    bucket.current = Some(TurnStateToken {
        value: encoded((now - 3600) as u64, 217),
        issued_at: now,
    });
    assert!(bucket.installed_token(now).is_none());
    assert!(
        serde_json::from_str::<TurnStateConfig>(r#"{"missingStatePolicy":"invalid"}"#).is_err()
    );
}

#[test]
fn cookie_override_binds_the_exact_cookie_instance() {
    let now = 1_800_000_000;
    let account = super::account("acct_cookie_pin");
    let cookie = |iat: i64| {
        let mut cookie = gateway_core::account::RoutingCookie::parse(
            "origin",
            "__oailb",
            &format!(
                "{}.{}.c2ln",
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(serde_json::json!({"alg":"ES256"}).to_string()),
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                    serde_json::json!({"host":"chat.gateway.unified-185.api.openai.com","iat":iat,"exp":now + 3600})
                        .to_string()
                )
            ),
            now,
        )
        .unwrap();
        cookie.reported_model = "upstream-model".to_owned();
        cookie
    };
    let mut bucket = TurnStateBucket {
        account_id: account.id().as_str().to_owned(),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().map(str::to_owned),
        model: "upstream-model".to_owned(),
        config: TurnStateConfig::default(),
        routing_cookies: Vec::new(),
        cookie_override_pod: Some("chat.gateway.unified-185.api.openai.com".to_owned()),
        cookie_override_issued_at: Some(now - 100),
        cookie_override_name: Some("__oailb".to_owned()),
        cookie_override_value: Some(cookie(now - 100).value),
        cookie_override_cflb_name: Some("__cflb".to_owned()),
        cookie_override_cflb_value: Some("synthetic-cflb".to_owned()),
        cookie_override_expires_at: Some(now + 3600),
        cookie_override_observation_id: None,
        current: None,
        current_issued_at: None,
        current_expires_at: None,
        installed_pair: None,
        current_length: None,
        candidate: None,
        candidate_expires_at: None,
        candidate_pair: None,
        hunt_attempts: 0,
        next_probe_at: None,
        manual_probe_requested_at: None,
        manual_override: false,
        attached_model: None,
    };
    let pinned = bucket.routing_cookie(now).expect("pinned cookie");
    assert_eq!(pinned.value, cookie(now - 100).value);
    assert_eq!(pinned.cflb_value, "synthetic-cflb");
    assert!(pinned.has_pair());
    bucket.cookie_override_value = None;
    assert!(bucket.routing_cookie(now).is_none());
    bucket.cookie_override_value = Some(cookie(now - 100).value);
    bucket.cookie_override_cflb_value = None;
    assert!(bucket.routing_cookie(now).is_none());
}

/// 固定 pair 换绑后，绑定旧 pair 的已安装票不可继续注入；
/// 固定值与已装 pair 一致时票照常可用。
#[test]
fn pinned_pair_switch_disqualifies_the_installed_token() {
    let now = 1_800_000_000;
    let account = super::account("acct_pin_switch");
    let jwt = |iat: i64| {
        format!(
            "{}.{}.c2ln",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::json!({"alg":"ES256"}).to_string()),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                serde_json::json!({"host":"chat.gateway.unified-185.api.openai.com","iat":iat,"exp":now + 3600})
                    .to_string()
            )
        )
    };
    let installed =
        gateway_core::account::RoutingCookie::parse("origin", "__oailb", &jwt(now - 100), now)
            .unwrap()
            .with_cflb("__cflb", "cflb-a", Some(now + 3600))
            .unwrap();
    let installed = gateway_core::account::RoutingCookie {
        reported_model: "upstream-model".to_owned(),
        ..installed
    };
    let mut bucket = TurnStateBucket {
        account_id: account.id().as_str().to_owned(),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().map(str::to_owned),
        model: "upstream-model".to_owned(),
        config: TurnStateConfig::default(),
        routing_cookies: Vec::new(),
        cookie_override_pod: None,
        cookie_override_issued_at: None,
        cookie_override_name: None,
        cookie_override_value: None,
        cookie_override_expires_at: None,
        cookie_override_observation_id: None,
        cookie_override_cflb_name: None,
        cookie_override_cflb_value: None,
        current: Some(TurnStateToken::parse(&encoded(now as u64, 585)).unwrap()),
        current_issued_at: Some(now),
        current_expires_at: None,
        installed_pair: Some(installed.clone()),
        current_length: Some(780),
        candidate: None,
        candidate_expires_at: None,
        candidate_pair: None,
        hunt_attempts: 0,
        next_probe_at: None,
        manual_probe_requested_at: None,
        manual_override: false,
        attached_model: None,
    };
    assert!(bucket.installed_token(now).is_some());
    // 固定到同 pod 的另一把 pair：票绑定的 pair 与固定值不同，票不再注入。
    bucket.cookie_override_pod = Some("chat.gateway.unified-185.api.openai.com".to_owned());
    bucket.cookie_override_issued_at = Some(now - 50);
    bucket.cookie_override_name = Some("__oailb".to_owned());
    bucket.cookie_override_value = Some(jwt(now - 50));
    bucket.cookie_override_cflb_name = Some("__cflb".to_owned());
    bucket.cookie_override_cflb_value = Some("cflb-b".to_owned());
    bucket.cookie_override_expires_at = Some(now + 3600);
    assert!(bucket.installed_token(now).is_none());
    assert_eq!(
        bucket.routing_cookie(now).unwrap().value,
        jwt(now - 50),
        "固定值优先回放"
    );
    // 固定回已装 pair 的同一把：票恢复可用。
    bucket.cookie_override_value = Some(installed.value);
    bucket.cookie_override_cflb_value = Some(installed.cflb_value);
    assert!(bucket.installed_token(now).is_some());
}

#[test]
fn cloud_mint_config_serde_aliases_defaults_and_strict_endpoint_validation() {
    // 旧配置 JSON 缺新字段时按默认值解析：780 目标、声明模型关闭、云端列表为空。
    let config: TurnStateConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(config.target_length, 780);
    assert_eq!(config.budget, 24);
    assert!(matches!(
        config.stop_strategy,
        TurnStateStopStrategy::Headers
    ));
    assert!(matches!(config.strategy, CloudMintStrategy::Random));
    assert_eq!(config.cloud_failure_threshold, 3);
    assert_eq!(config.cloud_cooldown_seconds, 300);
    assert_eq!(config.task_timeout_seconds, 75);
    assert!(config.cloud_mints.is_empty());
    assert!(!config.requires_route_pair());

    // declared_model 策略触发 pair 绑定验收；序列化回写 snake_case。
    let declared: TurnStateConfig =
        serde_json::from_str(r#"{"stopStrategy":"declared_model"}"#).unwrap();
    assert!(matches!(
        declared.stop_strategy,
        TurnStateStopStrategy::DeclaredModel
    ));
    assert!(declared.uses_declared_model());
    assert!(declared.requires_route_pair());
    assert_eq!(
        serde_json::to_value(declared.stop_strategy).unwrap(),
        "declared_model"
    );

    // snake_case 别名与 camelCase 本名等价；端点内字段同样接受两种写法。
    let parsed: TurnStateConfig = serde_json::from_str(
        r#"{
            "cloud_mints": [{
                "name": "relay-a",
                "url": "https://relay.example.invalid/mint",
                "key_env": "RELAY_KEY",
                "gateway": "88"
            }],
            "strategy": "round_robin",
            "cloud_failure_threshold": 2,
            "cloud_cooldown_seconds": 120,
            "task_timeout_seconds": 30
        }"#,
    )
    .unwrap();
    assert!(matches!(parsed.strategy, CloudMintStrategy::RoundRobin));
    assert_eq!(parsed.cloud_failure_threshold, 2);
    assert_eq!(parsed.cloud_cooldown_seconds, 120);
    assert_eq!(parsed.task_timeout_seconds, 30);
    let endpoint = &parsed.cloud_mints[0];
    assert_eq!(endpoint.ticket_length, 780);
    assert_eq!(endpoint.ttl_seconds, 240);
    assert_eq!(endpoint.timeout_ms, 90_000);
    assert_eq!(endpoint.gateway_target().as_deref(), Some("unified-88"));
    assert!(parsed.is_valid());
    assert!(parsed.requires_route_pair());
    let camel: TurnStateConfig = serde_json::from_str(
        r#"{
            "cloudMints": [{
                "name": "relay-b",
                "url": "https://relay.example.invalid/mint",
                "keyEnv": "RELAY_KEY",
                "ticket_length": 780,
                "ttl_seconds": 300,
                "timeout_ms": 5000,
                "transport": "websocket",
                "proxy_url": "http://127.0.0.1:8080"
            }]
        }"#,
    )
    .unwrap();
    assert_eq!(camel.cloud_mints[0].ttl_seconds, 300);
    assert_eq!(camel.cloud_mints[0].timeout_ms, 5000);
    assert!(matches!(
        camel.cloud_mints[0].transport,
        CloudMintTransport::Websocket
    ));
    assert!(camel.is_valid());
    // 端点 Debug 不落代理地址等敏感配置。
    assert!(!format!("{:?}", camel.cloud_mints[0]).contains("127.0.0.1"));

    // 端点 URL 必须是干净 HTTPS：非 HTTPS、内嵌凭据、query、fragment 一律拒绝。
    for url in [
        "http://relay.example.invalid/mint",
        "https://user:pass@relay.example.invalid/mint",
        "https://relay.example.invalid/mint?x=1",
        "https://relay.example.invalid/mint#frag",
        "not a url",
    ] {
        let mut bad = parsed.clone();
        bad.cloud_mints[0].url = url.to_owned();
        assert!(!bad.is_valid(), "{url}");
    }
    // key_env 只收合法环境变量名，密钥正文进不了配置。
    let mut bad = parsed.clone();
    bad.cloud_mints[0].key_env = "sk-live-secret".to_owned();
    assert!(!bad.is_valid());
    // 端点票据长度必须与桶目标一致，否则打出无法安装的票。
    let mut bad = parsed.clone();
    bad.cloud_mints[0].ticket_length = 292;
    assert!(!bad.is_valid());
    // 端点名在配置内唯一。
    let mut bad = parsed.clone();
    bad.cloud_mints.push(parsed.cloud_mints[0].clone());
    assert!(!bad.is_valid());
    // 网关只允许 any 或统一网编号。
    let mut bad = parsed.clone();
    bad.cloud_mints[0].gateway = "chat.gateway.unified-9.api.openai.com".to_owned();
    assert!(!bad.is_valid());
}
