use super::*;

const NOW: i64 = 1_000_000;

fn rule() -> AccountRule {
    AccountRule {
        account_id: "account-a".into(),
        five_hour_limit_percent: Some(95.0),
        weekly_limit_percent: Some(95.0),
        daily_limit_usd: None,
    }
}

fn window(seconds: u64, used: Option<f64>) -> QuotaWindowDetails {
    QuotaWindowDetails {
        key: format!("codex-{seconds}"),
        group: "codex".into(),
        label: "quota".into(),
        limit_id: Some("codex".into()),
        limit_name: None,
        role: None,
        window_seconds: Some(seconds),
        used_percent: used,
        reset_at_ms: Some(NOW + 3_600_000),
        limit_reached: false,
    }
}

fn quota() -> QuotaDetails {
    QuotaDetails {
        schema_version: 1,
        account_id: "account-a".into(),
        observed_at_ms: Some(NOW),
        limit_reached: false,
        windows: vec![
            window(FIVE_HOUR_SECONDS, Some(60.0)),
            window(WEEK_SECONDS, Some(17.0)),
        ],
        credits: None,
    }
}

fn has(assessment: &Assessment, code: ReasonCode) -> bool {
    assessment.reasons.iter().any(|reason| reason.code == code)
}

#[test]
fn screenshot_under_limit_windows_stay_available_when_observation_is_old() {
    let mut quota = quota();
    assert!(!assess(&rule(), &quota, NOW, 300).excluded);
    quota.observed_at_ms = Some(NOW - 300_001);
    let assessment = assess(&rule(), &quota, NOW, 300);
    assert!(!assessment.excluded);
    assert!(has(&assessment, ReasonCode::ObservationStale));
}

#[test]
fn known_limit_is_inclusive_and_expires_at_the_window_reset() {
    let mut quota = quota();
    quota.observed_at_ms = Some(NOW - 300_001);
    quota.windows[0].used_percent = Some(95.0);
    quota.windows[0].reset_at_ms = Some(NOW + 1);
    assert!(assess(&rule(), &quota, NOW, 300).excluded);
    let reset = assess(&rule(), &quota, NOW + 1, 300);
    assert!(!reset.excluded);
    assert!(has(&reset, ReasonCode::WindowExpired));
    quota.windows[0].reset_at_ms = Some(NOW + 3_600_000);
    quota.windows[0].used_percent = Some(1.0);
    quota.observed_at_ms = Some(NOW + 1);
    assert!(!assess(&rule(), &quota, NOW + 1, 300).excluded);
}

#[test]
fn missing_or_invalid_observations_are_information_not_denials() {
    assert!(!unavailable(&rule()).excluded);
    for (version, account, observed, expected) in [
        (2, "account-a", Some(NOW), ReasonCode::UnsupportedSchema),
        (1, "account-b", Some(NOW), ReasonCode::AccountMismatch),
        (1, "account-a", None, ReasonCode::ObservationMissing),
        (
            1,
            "account-a",
            Some(NOW + 1),
            ReasonCode::ObservationInFuture,
        ),
    ] {
        let mut quota = quota();
        quota.schema_version = version;
        quota.account_id = account.into();
        quota.observed_at_ms = observed;
        quota.windows[0].used_percent = Some(100.0);
        let result = assess(&rule(), &quota, NOW, 300);
        assert!(!result.excluded, "{expected:?}");
        assert!(has(&result, expected));
    }
}

#[test]
fn unknown_window_does_not_hide_a_different_windows_known_limit() {
    let mut quota = quota();
    quota.windows[0].used_percent = None;
    assert!(!assess(&rule(), &quota, NOW, 300).excluded);
    quota.windows[1].used_percent = Some(95.0);
    let result = assess(&rule(), &quota, NOW, 300);
    assert!(result.excluded);
    assert!(has(&result, ReasonCode::UsageUnknown));
    assert!(has(&result, ReasonCode::LimitReached));
    quota.windows.clear();
    assert!(!assess(&rule(), &quota, NOW, 300).excluded);
}

#[test]
fn missing_reset_does_not_turn_old_threshold_evidence_into_a_permanent_lock() {
    let mut quota = quota();
    quota.windows[0].reset_at_ms = None;
    assert!(!assess(&rule(), &quota, NOW, 300).excluded);
    quota.windows[0].used_percent = Some(96.0);
    assert!(assess(&rule(), &quota, NOW, 300).excluded);
    assert!(!assess(&rule(), &quota, NOW + 300_001, 300).excluded);
}

#[test]
fn explicit_host_limit_is_evidence_even_if_percentage_is_unknown() {
    let mut quota = quota();
    quota.windows[0].used_percent = None;
    quota.windows[0].limit_reached = true;
    assert!(assess(&rule(), &quota, NOW, 300).excluded);
    quota.windows[0].reset_at_ms = Some(NOW);
    assert!(!assess(&rule(), &quota, NOW, 300).excluded);
}

#[test]
fn only_matching_codex_windows_and_nonnegative_finite_usage_are_evidence() {
    let mut quota = quota();
    for used in [f64::NAN, f64::INFINITY, -1.0] {
        quota.windows[0].used_percent = Some(used);
        assert!(!assess(&rule(), &quota, NOW, 300).excluded);
    }
    quota.windows[0].used_percent = Some(105.0);
    assert!(assess(&rule(), &quota, NOW, 300).excluded);
    quota.windows[0].limit_id = Some("a-model-specific-bucket".into());
    assert!(!assess(&rule(), &quota, NOW, 300).excluded);
}

#[test]
fn explicit_zero_pauses_without_waiting_for_an_observation() {
    let mut rule = rule();
    rule.five_hour_limit_percent = Some(0.0);
    assert!(unavailable(&rule).excluded);
    let mut quota = quota();
    quota.observed_at_ms = None;
    assert!(assess(&rule, &quota, NOW, 300).excluded);
    rule.five_hour_limit_percent = None;
    rule.weekly_limit_percent = None;
    assert!(!unavailable(&rule).excluded);
    assert!(!assess(&rule, &quota, NOW, 300).excluded);
}
