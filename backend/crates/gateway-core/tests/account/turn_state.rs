use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use gateway_core::account::{TurnStateConfig, TurnStateToken};

fn encoded(issued_at: u64, size: usize) -> String {
    let mut bytes = vec![0; size];
    bytes[0] = 0x80;
    bytes[1..9].copy_from_slice(&issued_at.to_be_bytes());
    URL_SAFE.encode(bytes)
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
    assert_eq!(config.ttl_seconds, 3600);
    assert!(config.include_account_proxy);
    assert!(!config.include_direct);
    config.refresh_after_seconds = config.ttl_seconds;
    assert!(!config.is_valid());
    config.refresh_after_seconds = 2100;
    config.include_account_proxy = false;
    assert!(!config.is_valid());
    config.proxy_ids = vec!["proxy_test".to_owned()];
    assert!(config.is_valid());
    config.budget = 0;
    assert!(!config.is_valid());
}

#[test]
fn probe_persona_defaults_are_compatible_and_reject_header_injection() {
    let mut config: TurnStateConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(config.originator, "codex-tui");
    assert!(config.user_agent.is_empty());
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
