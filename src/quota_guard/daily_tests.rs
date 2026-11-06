use super::*;
use gateway_plugin_sdk::call::services::usage::{
    UsageCostSource, UsageRecord, UsageRecordData, UsageTokens,
};
use serde_json::json;
use std::{collections::VecDeque, future::ready};

const NOW: i64 = 5 * DAY_MS + 123_456;

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
    let snapshot = collect_usage("account-a", NOW, |query| {
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
    let snapshot = collect_usage("account-a", NOW, |_| {
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
    let usage = view(&rule(Some("10")), Some(&entry(snapshot)), NOW, 300);
    assert_eq!(usage.status, DailyStatus::Unknown);
    assert_eq!(usage.used_usd, None);
    assert_eq!(usage.remaining_usd, None);
    assert_eq!(usage.unknown_cost_count, 2);
    assert_eq!(usage.in_flight_count, 1);
    assert!(usage.excluded());
}

#[tokio::test]
async fn invalid_currency_amount_is_unknown_not_zero_or_rounded() {
    for amount in ["-2", "1e-9", "0.0000000000000000001", "1000000000000000000"] {
        let snapshot = collect_usage("account-a", NOW, |_| {
            ready(Ok(page(
                vec![record("bad", usd(amount), UsageOutcome::Succeeded)],
                None,
            )))
        })
        .await
        .unwrap();
        assert_eq!(snapshot.unknown_cost_count, 1);
        assert!(view(&rule(Some("100")), Some(&entry(snapshot)), NOW, 300).excluded());
    }
}

#[tokio::test]
async fn an_in_flight_record_is_visible_without_fabricating_a_final_fee() {
    let snapshot = collect_usage("account-a", NOW, |_| {
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
    let snapshot = collect_usage("account-a", NOW, |_| ready(Ok(page(vec![], None))))
        .await
        .unwrap();
    let cold = view(&rule(Some("10")), None, NOW, 300);
    assert_eq!(cold.status, DailyStatus::Unknown);
    assert_eq!(cold.used_usd, None);
    assert!(cold.excluded());
    let usage = view(&rule(Some("10")), Some(&entry(snapshot)), NOW, 300);
    assert_eq!(usage.status, DailyStatus::Ready);
    assert_eq!(usage.used_usd.as_deref(), Some("0"));
    assert_eq!(usage.remaining_usd.as_deref(), Some("10"));
}

#[tokio::test]
async fn rebuilding_after_restart_recovers_the_host_ledger() {
    let records = vec![record("persistent", usd("7.125"), UsageOutcome::Succeeded)];
    for _ in 0..2 {
        let snapshot = collect_usage("account-a", NOW, |_| ready(Ok(page(records.clone(), None))))
            .await
            .unwrap();
        let usage = view(&rule(Some("7")), Some(&entry(snapshot)), NOW, 300);
        assert_eq!(usage.used_usd.as_deref(), Some("7.125"));
        assert_eq!(usage.status, DailyStatus::Reached);
    }
}

#[tokio::test]
async fn stale_failed_future_and_new_day_observations_fail_closed() {
    let snapshot = collect_usage("account-a", NOW, |_| ready(Ok(page(vec![], None))))
        .await
        .unwrap();
    let mut cached = entry(snapshot);
    assert_eq!(
        view(&rule(Some("10")), Some(&cached), NOW + 300_001, 300).status,
        DailyStatus::Stale
    );
    assert_eq!(
        view(&rule(Some("10")), Some(&cached), NOW - 1, 300).status,
        DailyStatus::Stale
    );
    let new_day = view(&rule(Some("10")), Some(&cached), 6 * DAY_MS, 300);
    assert_eq!(new_day.status, DailyStatus::Stale);
    assert_eq!(new_day.reset_at_ms, 7 * DAY_MS);
    assert_eq!(new_day.used_usd, None);
    cached.failed = true;
    let failed = view(&rule(Some("10")), Some(&cached), NOW, 300);
    assert_eq!(failed.status, DailyStatus::Unknown);
    assert_eq!(failed.used_usd, None);
    assert!(failed.excluded());
    let disabled = view(&rule(None), Some(&cached), NOW, 300);
    assert_eq!(disabled.status, DailyStatus::Disabled);
    assert!(!disabled.excluded());
    assert_eq!(
        view(&rule(Some("0")), None, NOW, 300).status,
        DailyStatus::Reached
    );
    assert_eq!(
        view(&rule(Some("0")), Some(&cached), 6 * DAY_MS, 300).status,
        DailyStatus::Reached
    );
}

#[tokio::test]
async fn incomplete_duplicate_mismatched_and_unbounded_pagination_is_rejected() {
    let mut calls = 0;
    let result = collect_usage("account-a", NOW, |_| {
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
    .await;
    assert!(result.is_err());

    let mut calls = 0;
    assert!(
        collect_usage("account-a", NOW, |_| {
            calls += 1;
            ready(Ok(page(vec![], Some("repeated"))))
        })
        .await
        .is_err()
    );
    assert_eq!(calls, 2);

    assert!(
        collect_usage("account-a", NOW, |_| ready(Ok(page(
            vec![
                record("duplicate", usd("1"), UsageOutcome::Succeeded),
                record("duplicate", usd("1"), UsageOutcome::Succeeded),
            ],
            None
        ))))
        .await
        .is_err()
    );

    let mut wrong = record("wrong-account", usd("1"), UsageOutcome::Succeeded);
    wrong.data.account_ref = Some("account-b".into());
    assert!(
        collect_usage("account-a", NOW, |_| ready(Ok(page(
            vec![wrong.clone()],
            None
        ))))
        .await
        .is_err()
    );

    let mut outside = record("old-day", usd("1"), UsageOutcome::Succeeded);
    outside.data.started_at_ms = 5 * DAY_MS - 1;
    assert!(
        collect_usage("account-a", NOW, |_| ready(Ok(page(
            vec![outside.clone()],
            None
        ))))
        .await
        .is_err()
    );

    let mut calls = 0;
    assert!(
        collect_usage("account-a", NOW, |_| {
            calls += 1;
            ready(Ok(UsageRecordPage {
                items: vec![],
                next_cursor: Some(format!("cursor-{calls}")),
            }))
        })
        .await
        .is_err()
    );
    assert_eq!(calls, MAX_PAGES);
}

#[test]
fn refresh_leases_are_single_flight_and_cancellation_fails_closed() {
    let account = "test-cancelled-daily-refresh";
    let lease = RefreshLease::begin(account).unwrap();
    assert!(RefreshLease::begin(account).is_none());
    drop(lease);
    let cache = CACHE.lock().unwrap();
    let entry = cache.get(account).unwrap();
    assert!(!entry.refreshing);
    assert!(entry.failed);
}

#[test]
fn public_usage_contract_is_camel_case_with_explicit_currency_timezone_and_nulls() {
    let value = serde_json::to_value(view(&rule(Some("10")), None, NOW, 300)).unwrap();
    assert_eq!(value["limitUsd"], "10");
    assert!(value["usedUsd"].is_null());
    assert!(value["remainingUsd"].is_null());
    assert_eq!(value["status"], "unknown");
    assert_eq!(value["timezone"], "UTC");
    assert_eq!(value["currency"], "USD");
    assert_eq!(value["resetAtMs"], 6 * DAY_MS);
}
