use super::*;
use gateway_plugin_sdk::call::services::usage::{
    UsageCostSource, UsageRecord, UsageRecordData, UsageTokens,
};
use serde_json::json;
use std::{collections::VecDeque, future::ready};

const NOW: i64 = 5 * DAY_MS + 123_456;

fn deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + REFRESH_TIMEOUT
}

fn rule(limit: Option<&str>) -> AccountRule {
    serde_json::from_value(json!({
        "accountId": "account-a",
        "fiveHourLimitPercent": null,
        "weeklyLimitPercent": null,
        "dailyLimitUsd": limit,
    }))
    .unwrap()
}

fn record(id: &str, cost: UsageCost, outcome: UsageOutcome) -> UsageRecord {
    UsageRecord {
        id: id.to_owned(),
        data: UsageRecordData {
            client_key_ref: format!("key-{id}"),
            provider: Some("openai".into()),
            account_ref: Some("account-a".into()),
            requested_model: Some(format!("model-{id}")),
            model: Some(format!("model-{id}")),
            protocol: "openai".into(),
            operation: "generate_response".into(),
            endpoint: "/v1/responses".into(),
            client_transport: "http".into(),
            service_tier: None,
            started_at_ms: NOW - 1,
            completed_at_ms: (outcome != UsageOutcome::Running).then_some(NOW),
            outcome,
            tokens: UsageTokens::default(),
            cost,
        },
        billing_snapshot: None,
        provider_observation: None,
    }
}

fn usd(amount: &str) -> UsageCost {
    UsageCost::Known {
        source: UsageCostSource::Calculated,
        amount: amount.to_owned(),
        currency: "USD".into(),
    }
}

fn page(items: Vec<UsageRecord>, next: Option<&str>) -> UsageRecordPage {
    UsageRecordPage {
        items,
        next_cursor: next.map(str::to_owned),
    }
}

fn entry(snapshot: Snapshot) -> CacheEntry {
    CacheEntry {
        snapshot: Some(snapshot),
        ..CacheEntry::default()
    }
}

#[test]
fn money_is_exact_bounded_and_never_rounded() {
    let sum = Usd::cost("0.1")
        .unwrap()
        .checked_add(Usd::cost("0.2").unwrap())
        .unwrap();
    assert_eq!(sum.text(), "0.3");
    assert_eq!(Usd::limit("0003.120000").unwrap().text(), "3.12");
    assert_eq!(
        Usd::cost("0.000000000000000001").unwrap().text(),
        "0.000000000000000001"
    );
    assert!(Usd::limit("999999999999999999.999999999999").is_some());
    for invalid in [
        "",
        "-1",
        "+1",
        "1e2",
        ".1",
        "1.",
        " 1",
        "1 ",
        "1.2.3",
        "NaN",
        "1000000000000000000",
        "0.0000000000001",
    ] {
        assert!(Usd::limit(invalid).is_none(), "{invalid}");
    }
    assert!(Usd::cost("0.0000000000000000001").is_none());
    assert!(Usd(u128::MAX).checked_add(Usd(1)).is_none());
}

#[tokio::test]
async fn complete_pagination_counts_all_keys_models_and_operations_once() {
    let mut second = record("second", usd("0.2"), UsageOutcome::Succeeded);
    second.data.operation = "generate_image".into();
    second.data.endpoint = "/v1/images/generations".into();
    let mut pages = VecDeque::from([
        page(
            vec![record("first", usd("0.1"), UsageOutcome::Succeeded)],
            Some("next"),
        ),
        page(vec![second], None),
    ]);
    let mut calls = 0;
    let snapshot = collect_usage("account-a", NOW, deadline(), |query| {
        assert_eq!(query.filter.account_ref.as_deref(), Some("account-a"));
        assert_eq!(query.filter.start_ms, 5 * DAY_MS);
        assert_eq!(query.filter.end_ms, 6 * DAY_MS);
        assert_eq!(query.filter.provider, None);
        assert_eq!(query.filter.model, None);
        assert_eq!(query.filter.client_key_ref, None);
        assert_eq!(query.limit, Some(200));
        assert_eq!(
            query.cursor.as_deref(),
            if calls == 0 { None } else { Some("next") }
        );
        calls += 1;
        ready(Ok(pages.pop_front().unwrap()))
    })
    .await
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(snapshot.total.text(), "0.3");
    let usage = view(&rule(Some("0.3")), Some(&entry(snapshot)), NOW, 300);
    assert_eq!(usage.status, DailyStatus::Reached);
    assert_eq!(usage.used_usd.as_deref(), Some("0.3"));
    assert_eq!(usage.remaining_usd.as_deref(), Some("0"));
    assert!(usage.excluded());
}

#[tokio::test]
async fn unknown_final_fees_and_running_requests_are_distinct() {
    let snapshot = collect_usage("account-a", NOW, deadline(), |_| {
        ready(Ok(page(
            vec![
                record("known", usd("1"), UsageOutcome::Succeeded),
                record("failed-with-fee", usd("0.5"), UsageOutcome::Failed),
                record("unknown", UsageCost::Unavailable, UsageOutcome::Succeeded),
                record(
                    "non-usd",
                    UsageCost::Known {
                        source: UsageCostSource::ProviderReported,
                        amount: "9".into(),
                        currency: "EUR".into(),
                    },
                    UsageOutcome::Succeeded,
                ),
                record("running", UsageCost::Unavailable, UsageOutcome::Running),
            ],
            None,
        )))
    })
    .await
    .unwrap();
    assert_eq!(snapshot.total.text(), "1.5");
    assert_eq!(snapshot.unknown_cost_count, 2);
    assert_eq!(snapshot.in_flight_count, 1);
    let cached = entry(snapshot);
    let usage = view(&rule(Some("10")), Some(&cached), NOW, 300);
    assert_eq!(usage.status, DailyStatus::Unknown);
    assert_eq!(usage.used_usd.as_deref(), Some("1.5"));
    assert_eq!(usage.remaining_usd, None);
    assert_eq!(usage.unknown_cost_count, 2);
    assert_eq!(usage.in_flight_count, 1);
    assert!(!usage.complete);
    assert!(!usage.excluded());
    let reached = view(&rule(Some("1.5")), Some(&cached), NOW, 300);
    assert_eq!(reached.status, DailyStatus::Reached);
    assert_eq!(reached.remaining_usd.as_deref(), Some("0"));
    assert!(reached.excluded());
}

#[tokio::test]
async fn invalid_currency_amount_is_unknown_not_zero_or_rounded() {
    for amount in ["-2", "1e-9", "0.0000000000000000001", "1000000000000000000"] {
        let snapshot = collect_usage("account-a", NOW, deadline(), |_| {
            ready(Ok(page(
                vec![record("bad", usd(amount), UsageOutcome::Succeeded)],
                None,
            )))
        })
        .await
        .unwrap();
        assert_eq!(snapshot.unknown_cost_count, 1);
        let usage = view(&rule(Some("100")), Some(&entry(snapshot)), NOW, 300);
        assert!(!usage.excluded());
        assert_eq!(usage.used_usd, None);
        assert!(!usage.complete);
    }
}

#[tokio::test]
async fn an_in_flight_record_is_visible_without_fabricating_a_final_fee() {
    let snapshot = collect_usage("account-a", NOW, deadline(), |_| {
        ready(Ok(page(
            vec![record(
                "running",
                UsageCost::Unavailable,
                UsageOutcome::Running,
            )],
            None,
        )))
    })
    .await
    .unwrap();
    let usage = view(&rule(Some("10")), Some(&entry(snapshot)), NOW, 300);
    assert_eq!(usage.status, DailyStatus::Ready);
    assert_eq!(usage.used_usd.as_deref(), Some("0"));
    assert_eq!(usage.in_flight_count, 1);
    assert_eq!(usage.unknown_cost_count, 0);
}

#[tokio::test]
async fn empty_complete_ledger_is_zero_but_cold_cache_is_unknown() {
    let snapshot = collect_usage("account-a", NOW, deadline(), |_| {
        ready(Ok(page(vec![], None)))
    })
    .await
    .unwrap();
    let cold = view(&rule(Some("10")), None, NOW, 300);
    assert_eq!(cold.status, DailyStatus::Unknown);
    assert_eq!(cold.used_usd, None);
    assert!(!cold.excluded());
    assert!(!cold.complete);
    let usage = view(&rule(Some("10")), Some(&entry(snapshot)), NOW, 300);
    assert_eq!(usage.status, DailyStatus::Ready);
    assert_eq!(usage.used_usd.as_deref(), Some("0"));
    assert_eq!(usage.remaining_usd.as_deref(), Some("10"));
    assert!(usage.complete);
    assert!(!usage.excluded());
}

#[tokio::test]
async fn rebuilding_after_restart_recovers_the_host_ledger() {
    let records = vec![record("persistent", usd("7.125"), UsageOutcome::Succeeded)];
    for _ in 0..2 {
        let snapshot = collect_usage("account-a", NOW, deadline(), |_| {
            ready(Ok(page(records.clone(), None)))
        })
        .await
        .unwrap();
        let usage = view(&rule(Some("7")), Some(&entry(snapshot)), NOW, 300);
        assert_eq!(usage.used_usd.as_deref(), Some("7.125"));
        assert_eq!(usage.status, DailyStatus::Reached);
    }
}

#[tokio::test]
async fn stale_failed_future_and_new_day_observations_do_not_create_a_denial() {
    let snapshot = collect_usage("account-a", NOW, deadline(), |_| {
        ready(Ok(page(vec![], None)))
    })
    .await
    .unwrap();
    let mut cached = entry(snapshot);
    for at in [NOW + 300_001, NOW - 1, 6 * DAY_MS] {
        let usage = view(&rule(Some("10")), Some(&cached), at, 300);
        assert_eq!(usage.status, DailyStatus::Stale);
        assert!(!usage.complete);
        assert!(!usage.excluded());
    }
    let new_day = view(&rule(Some("10")), Some(&cached), 6 * DAY_MS, 300);
    assert_eq!(new_day.reset_at_ms, 7 * DAY_MS);
    assert_eq!(new_day.used_usd, None);
    cached.failed = true;
    let failed = view(&rule(Some("10")), Some(&cached), NOW, 300);
    assert_eq!(failed.status, DailyStatus::Unknown);
    assert_eq!(failed.used_usd.as_deref(), Some("0"));
    assert!(!failed.excluded());
    assert!(!failed.complete);
    let disabled = view(&rule(None), Some(&cached), NOW, 300);
    assert_eq!(disabled.status, DailyStatus::Disabled);
    assert!(!disabled.excluded());
    assert!(view(&rule(Some("0")), None, NOW, 300).excluded());
    assert!(view(&rule(Some("0")), Some(&cached), 6 * DAY_MS, 300).excluded());
}

#[tokio::test]
async fn known_daily_limit_survives_refresh_failures_but_not_utc_midnight() {
    let snapshot = collect_usage("account-a", NOW, deadline(), |_| {
        ready(Ok(page(
            vec![record("charged", usd("10"), UsageOutcome::Succeeded)],
            None,
        )))
    })
    .await
    .unwrap();
    let mut cached = entry(snapshot);
    cached.failed = true;
    let stale = view(&rule(Some("10")), Some(&cached), NOW + 300_001, 300);
    assert!(stale.excluded());
    assert_eq!(stale.used_usd.as_deref(), Some("10"));
    assert!(!stale.complete);
    let new_day = view(&rule(Some("10")), Some(&cached), 6 * DAY_MS, 300);
    assert!(!new_day.excluded());
    assert_eq!(new_day.used_usd, None);
    assert_eq!(new_day.remaining_usd, None);
    // A future-dated observation cannot establish a threshold for the current instant.
    assert!(!view(&rule(Some("10")), Some(&cached), NOW - 1, 300).excluded());
}

#[tokio::test]
async fn a_shared_deadline_keeps_completed_pages_but_not_an_unread_first_page() {
    let mut calls = 0;
    let until = tokio::time::Instant::now() + Duration::from_millis(20);
    let snapshot = collect_usage("account-a", NOW, until, |_| {
        calls += 1;
        let first = calls == 1;
        async move {
            if first {
                Ok(page(
                    vec![record("paid", usd("2.5"), UsageOutcome::Succeeded)],
                    Some("next"),
                ))
            } else {
                std::future::pending::<Result<UsageRecordPage, PluginFault>>().await
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(snapshot.total.text(), "2.5");
    assert!(!snapshot.complete);
    let cached = entry(snapshot);
    let below = view(&rule(Some("3")), Some(&cached), NOW, 300);
    assert_eq!(below.used_usd.as_deref(), Some("2.5"));
    assert!(!below.complete);
    assert!(!below.excluded());
    assert!(view(&rule(Some("2.5")), Some(&cached), NOW, 300).excluded());

    let unread = collect_usage(
        "account-a",
        NOW,
        tokio::time::Instant::now() + Duration::from_millis(20),
        |_| std::future::pending::<Result<UsageRecordPage, PluginFault>>(),
    )
    .await;
    assert!(unread.is_err());
    let cold = view(&rule(Some("3")), None, NOW, 300);
    assert_eq!(cold.used_usd, None);
    assert!(!cold.excluded());
}

#[tokio::test]
async fn interrupted_pages_keep_the_verified_cost_without_claiming_a_complete_total() {
    let mut calls = 0;
    let snapshot = collect_usage("account-a", NOW, deadline(), |_| {
        calls += 1;
        ready(if calls == 1 {
            Ok(page(
                vec![record("first", usd("1"), UsageOutcome::Succeeded)],
                Some("next"),
            ))
        } else {
            Err(unavailable())
        })
    })
    .await
    .unwrap();
    assert_eq!(snapshot.total.text(), "1");
    assert!(!snapshot.complete);
    let cached = entry(snapshot);
    let below = view(&rule(Some("2")), Some(&cached), NOW, 300);
    assert_eq!(below.used_usd.as_deref(), Some("1"));
    assert_eq!(below.remaining_usd, None);
    assert!(!below.complete);
    assert!(!below.excluded());
    assert!(view(&rule(Some("1")), Some(&cached), NOW, 300).excluded());
    assert!(
        collect_usage("account-a", NOW, deadline(), |_| ready(Err(unavailable())))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn duplicates_and_wrong_account_or_day_never_add_cost_twice_or_to_another_account() {
    let duplicate = collect_usage("account-a", NOW, deadline(), |_| {
        ready(Ok(page(
            vec![
                record("duplicate", usd("1"), UsageOutcome::Succeeded),
                record("duplicate", usd("1"), UsageOutcome::Succeeded),
            ],
            None,
        )))
    })
    .await
    .unwrap();
    assert_eq!(duplicate.total.text(), "1");
    assert!(!duplicate.complete);
    assert!(!view(&rule(Some("1.5")), Some(&entry(duplicate)), NOW, 300).excluded());

    let mut wrong = record("wrong-account", usd("100"), UsageOutcome::Succeeded);
    wrong.data.account_ref = Some("account-b".into());
    let mut outside = record("old-day", usd("100"), UsageOutcome::Succeeded);
    outside.data.started_at_ms = 5 * DAY_MS - 1;
    for invalid in [wrong, outside] {
        let snapshot = collect_usage("account-a", NOW, deadline(), |_| {
            ready(Ok(page(vec![invalid.clone()], None)))
        })
        .await
        .unwrap();
        assert_eq!(snapshot.total.text(), "0");
        assert!(!snapshot.complete);
        let usage = view(&rule(Some("1")), Some(&entry(snapshot)), NOW, 300);
        assert!(!usage.excluded());
        assert_eq!(usage.used_usd, None);
    }
}

#[tokio::test]
async fn repeated_and_unbounded_cursors_stop_with_an_incomplete_snapshot() {
    let mut calls = 0;
    let repeated = collect_usage("account-a", NOW, deadline(), |_| {
        calls += 1;
        ready(Ok(page(vec![], Some("repeated"))))
    })
    .await
    .unwrap();
    assert!(!repeated.complete);
    assert_eq!(calls, 2);

    let mut calls = 0;
    let unbounded = collect_usage("account-a", NOW, deadline(), |_| {
        calls += 1;
        ready(Ok(UsageRecordPage {
            items: vec![],
            next_cursor: Some(format!("cursor-{calls}")),
        }))
    })
    .await
    .unwrap();
    assert!(!unbounded.complete);
    assert_eq!(calls, MAX_PAGES);
}

#[tokio::test]
async fn cancellation_remains_a_cancellation_even_after_a_partial_page() {
    let mut calls = 0;
    let result = collect_usage("account-a", NOW, deadline(), |_| {
        calls += 1;
        ready(if calls == 1 {
            Ok(page(
                vec![record("first", usd("1"), UsageOutcome::Succeeded)],
                Some("next"),
            ))
        } else {
            Err(PluginFault::new(ErrorCode::Cancelled, "cancelled"))
        })
    })
    .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
}

#[tokio::test]
async fn cpr_decimal_cost_is_visible_despite_unpriced_successes_and_unbilled_failures() {
    let mut persisted = record(
        "persisted",
        usd("127.960000000000000000"),
        UsageOutcome::Succeeded,
    );
    // SDK exposes the persisted total separately from its detailed billing snapshot.
    persisted.billing_snapshot = Some(json!({"amount": "999", "currency": "USD"}));
    let mut items = vec![persisted];
    for index in 0..6 {
        items.push(record(
            &format!("unpriced-{index}"),
            UsageCost::Unavailable,
            UsageOutcome::Succeeded,
        ));
    }
    items.push(record(
        "failed",
        UsageCost::Unavailable,
        UsageOutcome::Failed,
    ));
    items.push(record(
        "cancelled",
        UsageCost::Unavailable,
        UsageOutcome::Cancelled,
    ));
    let snapshot = collect_usage("account-a", NOW, deadline(), |_| {
        ready(Ok(page(items.clone(), None)))
    })
    .await
    .unwrap();
    let cached = entry(snapshot);
    let below = view(&rule(Some("150")), Some(&cached), NOW, 300);
    assert_eq!(below.used_usd.as_deref(), Some("127.96"));
    assert_eq!(below.unknown_cost_count, 6);
    assert!(!below.complete);
    assert!(!below.excluded());
    assert!(view(&rule(Some("100")), Some(&cached), NOW, 300).excluded());
}

#[tokio::test]
async fn all_unpriced_successes_are_unknown_while_a_known_zero_remains_zero() {
    let unknown = record("unknown", UsageCost::Unavailable, UsageOutcome::Succeeded);
    for with_known_zero in [false, true] {
        let mut items = vec![unknown.clone()];
        if with_known_zero {
            items.push(record("known-zero", usd("0"), UsageOutcome::Succeeded));
        }
        let snapshot = collect_usage("account-a", NOW, deadline(), |_| {
            ready(Ok(page(items.clone(), None)))
        })
        .await
        .unwrap();
        let usage = view(&rule(Some("1")), Some(&entry(snapshot)), NOW, 300);
        assert_eq!(
            usage.used_usd.as_deref(),
            if with_known_zero { Some("0") } else { None }
        );
        assert_eq!(usage.status, DailyStatus::Unknown);
        assert!(!usage.complete);
        assert!(!usage.excluded());
    }
}

#[test]
fn refresh_leases_are_single_flight_and_cancellation_releases_without_exclusion() {
    let account = "test-cancelled-daily-refresh";
    let lease = RefreshLease::begin(account).unwrap();
    assert!(RefreshLease::begin(account).is_none());
    drop(lease);
    let cache = CACHE.lock().unwrap();
    let entry = cache.get(account).unwrap();
    assert!(!entry.refreshing);
    assert!(entry.failed);
    assert!(!view(&rule(Some("1")), Some(entry), NOW, 300).excluded());
}

#[tokio::test]
async fn partial_refresh_keeps_a_known_limit_but_full_corrections_and_new_days_replace_it() {
    let mut configured = rule(Some("10"));
    configured.account_id = "test-known-daily-cost-refresh".into();
    CACHE
        .lock()
        .unwrap()
        .insert(configured.account_id.clone(), CacheEntry::default());
    let store = |snapshot| {
        RefreshLease {
            account_id: configured.account_id.clone(),
            completed: false,
        }
        .finish(Some(snapshot))
    };
    let known = collect_usage("account-a", NOW, deadline(), |_| {
        ready(Ok(page(
            vec![record("paid", usd("10"), UsageOutcome::Succeeded)],
            None,
        )))
    })
    .await
    .unwrap();
    store(known.clone());
    assert!(daily_usage(&configured, NOW, 300).excluded());

    let mut partial = known.clone();
    partial.observed_at_ms = NOW + 1;
    partial.total = Usd::cost("1").unwrap();
    partial.complete = false;
    store(partial.clone());
    let retained = daily_usage(&configured, NOW + 1, 300);
    assert_eq!(retained.used_usd.as_deref(), Some("10"));
    assert!(retained.excluded());
    assert!(!retained.complete);

    partial.complete = true;
    store(partial);
    assert_eq!(
        daily_usage(&configured, NOW + 1, 300).used_usd.as_deref(),
        Some("1")
    );
    assert!(!daily_usage(&configured, NOW + 1, 300).excluded());

    store(known.clone());
    let mut tomorrow = known;
    tomorrow.day_start_ms = 6 * DAY_MS;
    tomorrow.observed_at_ms = 6 * DAY_MS;
    tomorrow.total = Usd::cost("1").unwrap();
    tomorrow.complete = false;
    store(tomorrow);
    let next_day = daily_usage(&configured, 6 * DAY_MS, 300);
    assert_eq!(next_day.used_usd.as_deref(), Some("1"));
    assert!(!next_day.excluded());
    CACHE.lock().unwrap().remove(&configured.account_id);
}

#[test]
fn public_usage_contract_is_camel_case_with_explicit_currency_timezone_and_nulls() {
    let value = serde_json::to_value(view(&rule(Some("10")), None, NOW, 300)).unwrap();
    assert_eq!(value["limitUsd"], "10");
    assert!(value["usedUsd"].is_null());
    assert!(value["remainingUsd"].is_null());
    assert_eq!(value["status"], "unknown");
    assert_eq!(value["complete"], false);
    assert_eq!(value["timezone"], "UTC");
    assert_eq!(value["currency"], "USD");
    assert_eq!(value["resetAtMs"], 6 * DAY_MS);
}
