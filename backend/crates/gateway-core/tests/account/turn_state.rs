use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use gateway_core::account::{
    MissingTurnStatePolicy, TurnStateBucket, TurnStateConfig, TurnStateToken,
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
    for (size, length) in [(217, 292), (233, 312), (414, 552)] {
        let value = encoded(1_800_000_000, size);
        assert_eq!(value.len(), length);
        assert!(TurnStateToken::parse(&value).is_some());
        assert_eq!(
            value.len() == TurnStateConfig::default().target_length,
            length == 292
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
    assert!(!config.revokes_ticket_when_model_detaches());
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
        current: None,
        current_issued_at: None,
        current_length: None,
        candidate: Some(TurnStateToken::parse(&encoded(now as u64, 217)).unwrap()),
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
    bucket.current_length = Some(292);
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
        cookie_override_expires_at: Some(now + 3600),
        current: None,
        current_issued_at: None,
        current_length: None,
        candidate: None,
        hunt_attempts: 0,
        next_probe_at: None,
        manual_probe_requested_at: None,
        manual_override: false,
        attached_model: None,
    };
    let pinned = bucket.routing_cookie(now).expect("pinned cookie");
    assert_eq!(pinned.value, cookie(now - 100).value);
    bucket.cookie_override_value = None;
    assert!(bucket.routing_cookie(now).is_none());
}
