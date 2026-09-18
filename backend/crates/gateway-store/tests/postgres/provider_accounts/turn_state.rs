use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use gateway_core::account::{
    MissingTurnStatePolicy, TurnStateBusinessStatus, TurnStateConfig, TurnStateObservation,
    TurnStateToken,
};

use super::*;

fn token(length: usize, issued_at: i64) -> TurnStateToken {
    let mut bytes = vec![0; (length / 4) * 3 - 2];
    bytes[0] = 0x80;
    bytes[1..9].copy_from_slice(&(issued_at as u64).to_be_bytes());
    TurnStateToken::parse(&URL_SAFE.encode(bytes)).unwrap()
}

fn observation(account: &str, model: &str, length: usize, issued_at: i64) -> TurnStateObservation {
    TurnStateObservation {
        account_id: account.to_owned(),
        upstream_account_id: None,
        upstream_user_id: Some(account.to_owned()),
        model: model.to_owned(),
        observed_at: Utc::now().timestamp(),
        started_at: Some(Utc::now().timestamp() - 1),
        source: "probe".to_owned(),
        request_state_source: Some("none".to_owned()),
        response_source: Some("http_headers".to_owned()),
        probe_trigger: Some("scheduled".to_owned()),
        outcome: if length == 292 {
            "candidate"
        } else {
            "length_miss"
        }
        .to_owned(),
        http_status: Some(200),
        token_length: Some(length),
        issued_at: Some(issued_at),
        egress: "direct".to_owned(),
        shape: Some("greeting".to_owned()),
        effort: Some("high".to_owned()),
        elapsed_ms: 1,
        probe_id: Some("test-probe".to_owned()),
        stop_mode: Some("headers".to_owned()),
        stop_reason: Some("headers".to_owned()),
    }
}

#[tokio::test]
async fn turn_state_candidates_are_atomic_isolated_and_expire_from_issue_time() {
    let Some(database) = TestDatabase::create("turn_state_rotation").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let admin = admin_account_store(&database.pool);
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "turn-state-test".to_owned(),
    };
    for id in ["acct_turn_a", "acct_turn_b"] {
        repository
            .insert_provider_account(account(id, id))
            .await
            .unwrap();
        for model in ["model-a", "model-b"] {
            admin
                .configure_turn_state(
                    &ProviderAccountId::new(id).unwrap(),
                    model,
                    TurnStateConfig {
                        enabled: true,
                        ..TurnStateConfig::default()
                    },
                    &context,
                )
                .await
                .unwrap();
        }
    }
    let id = ProviderAccountId::new("acct_turn_a").unwrap();
    let selected = repository
        .turn_state_buckets_for_model(std::slice::from_ref(&id), "model-a")
        .await
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].account_id, id.as_str());
    assert_eq!(selected[0].model, "model-a");
    admin
        .request_turn_state_probe(&id, "manual-model", &context)
        .await
        .unwrap();
    admin
        .request_turn_state_probe(&id, "manual-model", &context)
        .await
        .unwrap();
    let manual = repository
        .turn_state_bucket(&id, "manual-model")
        .await
        .unwrap()
        .unwrap();
    assert!(!manual.config.enabled);
    assert!(manual.manual_probe_requested_at.is_some());
    assert!(manual.next_probe_at.is_none());
    let (first, second) = tokio::join!(
        repository.claim_turn_state_probe(&id, "manual-model"),
        repository.claim_turn_state_probe(&id, "manual-model")
    );
    assert_eq!(
        usize::from(first.unwrap()) + usize::from(second.unwrap()),
        1
    );
    assert!(
        !repository
            .claim_turn_state_probe(&id, "manual-model")
            .await
            .unwrap()
    );
    let issued = Utc::now().timestamp() - 60;
    let initial = token(292, issued);
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-a", 292, issued),
            Some(initial.clone()),
        )
        .await
        .unwrap();
    assert!(repository.install_turn_state(&id, "model-a").await.unwrap());
    assert!(!repository.install_turn_state(&id, "model-a").await.unwrap());
    assert!(!repository.install_turn_state(&id, "model-b").await.unwrap());
    assert!(
        !repository
            .install_turn_state(&ProviderAccountId::new("acct_turn_b").unwrap(), "model-a")
            .await
            .unwrap()
    );
    // 同签发时间、旧候选和非目标长度都不能越过更新门。
    for (length, time) in [
        (292, issued),
        (292, issued - 1),
        (312, issued + 1),
        (552, issued + 1),
    ] {
        repository
            .observe_turn_state(
                observation(id.as_str(), "model-a", length, time),
                Some(token(length, time)),
            )
            .await
            .unwrap();
        assert!(!repository.install_turn_state(&id, "model-a").await.unwrap());
    }
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-b", 292, issued - 4000),
            Some(token(292, issued - 4000)),
        )
        .await
        .unwrap();
    assert!(!repository.install_turn_state(&id, "model-b").await.unwrap());
    let status = admin.turn_state_status(Some(id.as_str())).await.unwrap();
    let model = status
        .iter()
        .find(|bucket| bucket.model == "model-a")
        .unwrap();
    assert_eq!(model.installations.len(), 1);
    assert_eq!(
        model.account_email.as_deref(),
        Some("acct_turn_a@example.invalid")
    );
    assert!(
        model
            .observations
            .iter()
            .any(|item| item.outcome == "not_newer")
    );
    assert_eq!(model.issued_at, Some(issued));
    assert_eq!(model.installations[0].attempts, 1);
    assert!(model.installations[0].acquired_at >= issued);
    assert!(model.next_probe_at.is_some());
    assert!(
        model
            .observations
            .iter()
            .any(|item| item.token_length == Some(552))
    );
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains(&initial.value)
    );
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-a", 292, issued + 1),
            Some(TurnStateToken {
                value: "x".repeat(292),
                issued_at: issued + 1,
            }),
        )
        .await
        .unwrap();
    assert!(!repository.install_turn_state(&id, "model-a").await.unwrap());
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-a", 292, issued + 2),
            Some(token(292, issued + 2)),
        )
        .await
        .unwrap();
    let (first, second) = tokio::join!(
        repository.install_turn_state(&id, "model-a"),
        repository.install_turn_state(&id, "model-a")
    );
    assert_eq!(
        usize::from(first.unwrap()) + usize::from(second.unwrap()),
        1
    );
    sqlx::query("update account_turn_states set current_issued_at = $1 where account_id = $2 and model = 'model-a'")
        .bind(issued - 4000).bind(id.as_str()).execute(&database.pool).await.unwrap();
    assert!(
        repository
            .turn_state_bucket(&id, "model-a")
            .await
            .unwrap()
            .unwrap()
            .current
            .is_none()
    );
    repository.turn_state_buckets().await.unwrap();
    let expired: Option<String> = sqlx::query_scalar("select turn_state_override from account_turn_states where account_id = $1 and model = 'model-a'")
        .bind(id.as_str()).fetch_one(&database.pool).await.unwrap();
    assert!(expired.is_none());
    // 原身份发出的在途响应不能在重新授权后重新填入同一个本地账号桶。
    admin
        .request_turn_state_probe(&id, "model-a", &context)
        .await
        .unwrap();
    sqlx::query("update provider_accounts set upstream_user_id = 'new-user' where id = $1")
        .bind(id.as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-a", 292, issued + 3),
            Some(token(292, issued + 3)),
        )
        .await
        .unwrap();
    let replaced = repository
        .turn_state_bucket(&id, "model-a")
        .await
        .unwrap()
        .unwrap();
    assert!(replaced.current.is_none());
    assert!(replaced.candidate.is_none());
    assert!(replaced.current_issued_at.is_none());
    assert!(replaced.manual_probe_requested_at.is_none());
    assert!(!replaced.manual_override);
    assert!(replaced.config.enabled);
    assert_eq!(replaced.upstream_user_id.as_deref(), Some("new-user"));
    database.close().await;
}

#[tokio::test]
async fn manual_apply_keeps_rotation_disabled_and_rejects_stale_candidates() {
    let Some(database) = TestDatabase::create("turn_state_manual_apply").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let admin = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_manual_apply").unwrap();
    repository
        .insert_provider_account(account(id.as_str(), id.as_str()))
        .await
        .unwrap();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "manual-apply-test".to_owned(),
    };
    let strict = TurnStateConfig {
        missing_state_policy: MissingTurnStatePolicy::Pause,
        ..TurnStateConfig::default()
    };
    admin
        .configure_turn_state(&id, "model-a", strict.clone(), &context)
        .await
        .unwrap();
    let issued = Utc::now().timestamp() - 60;
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-a", 292, issued),
            Some(token(292, issued)),
        )
        .await
        .unwrap();
    let status = admin
        .turn_state_status(Some(id.as_str()))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(status.candidate_issued_at, Some(issued));
    assert_eq!(status.candidate_length, Some(292));
    assert!(!status.config.enabled);
    assert!(!status.active);
    assert_eq!(
        status.business_status,
        TurnStateBusinessStatus::WaitingForState
    );
    assert!(
        admin
            .apply_turn_state(&id, "model-b", issued, &context)
            .await
            .is_err()
    );
    assert!(
        admin
            .apply_turn_state(&id, "model-a", issued - 1, &context)
            .await
            .is_err()
    );
    let (first, second) = tokio::join!(
        admin.apply_turn_state(&id, "model-a", issued, &context),
        admin.apply_turn_state(&id, "model-a", issued, &context),
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let status = admin
        .turn_state_status(Some(id.as_str()))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(status.active);
    assert_eq!(status.business_status, TurnStateBusinessStatus::Ready);
    assert!(status.manual_override);
    assert!(!status.config.enabled);
    assert!(status.next_probe_at.is_none());
    assert!(status.candidate_issued_at.is_none());
    assert_eq!(status.installations.len(), 1);
    admin
        .configure_turn_state(&id, "model-a", strict, &context)
        .await
        .unwrap();
    let configured = repository
        .turn_state_bucket(&id, "model-a")
        .await
        .unwrap()
        .unwrap();
    assert!(configured.installed_token(Utc::now().timestamp()).is_some());
    assert!(configured.manual_override);
    assert!(!configured.config.enabled);
    for outcome in ["length_miss", "missing_header", "transport_error"] {
        let mut miss = observation(id.as_str(), "model-a", 312, issued + 1);
        miss.outcome = outcome.to_owned();
        repository.observe_turn_state(miss, None).await.unwrap();
        assert!(!repository.install_turn_state(&id, "model-a").await.unwrap());
        let retained = repository
            .turn_state_bucket(&id, "model-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            retained
                .installed_token(Utc::now().timestamp())
                .unwrap()
                .issued_at,
            issued
        );
    }
    sqlx::query("update provider_accounts set enabled = false where id = $1")
        .bind(id.as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    let disabled = admin
        .turn_state_status(Some(id.as_str()))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(!disabled.account_enabled);
    assert_eq!(
        disabled.business_status,
        TurnStateBusinessStatus::ManualDisabled
    );
    repository.turn_state_buckets().await.unwrap();
    assert!(!repository.install_turn_state(&id, "model-a").await.unwrap());
    assert!(
        !repository
            .get_account(&id)
            .await
            .unwrap()
            .unwrap()
            .enabled()
    );
    sqlx::query("update provider_accounts set enabled = true where id = $1")
        .bind(id.as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(
        admin
            .apply_turn_state(&id, "model-a", issued, &context)
            .await
            .is_err()
    );
    let expired = Utc::now().timestamp() - 3600;
    sqlx::query("update account_turn_states set turn_state_override = $2, current_issued_at = $3 where account_id = $1")
        .bind(id.as_str()).bind(token(292, expired).value).bind(expired).execute(&database.pool).await.unwrap();
    let status = admin
        .turn_state_status(Some(id.as_str()))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(!status.active);
    assert_eq!(
        status.business_status,
        TurnStateBusinessStatus::WaitingForState
    );
    sqlx::query("update account_turn_states set candidate = $2, candidate_issued_at = $3, current_issued_at = null, turn_state_override = null where account_id = $1")
        .bind(id.as_str()).bind(token(292, expired).value).bind(expired).execute(&database.pool).await.unwrap();
    assert!(
        admin
            .apply_turn_state(&id, "model-a", expired, &context)
            .await
            .is_err()
    );
    admin
        .configure_turn_state(&id, "model-a", TurnStateConfig::default(), &context)
        .await
        .unwrap();
    let state = repository
        .turn_state_bucket(&id, "model-a")
        .await
        .unwrap()
        .unwrap();
    assert!(!state.manual_override);
    assert!(state.current.is_none());
    database.close().await;
}
