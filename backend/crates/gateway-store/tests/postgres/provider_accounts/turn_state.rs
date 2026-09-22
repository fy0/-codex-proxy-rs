use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use gateway_core::account::{
    MissingTurnStatePolicy, TurnStateBusinessStatus, TurnStateConfig, TurnStateObservation,
    TurnStateToken,
};

use super::*;

pub(super) fn token(length: usize, issued_at: i64) -> TurnStateToken {
    let mut bytes = vec![0; (length / 4) * 3 - 2];
    bytes[0] = 0x80;
    bytes[1..9].copy_from_slice(&(issued_at as u64).to_be_bytes());
    TurnStateToken::parse(&URL_SAFE.encode(bytes)).unwrap()
}

pub(super) fn observation(
    account: &str,
    model: &str,
    length: usize,
    issued_at: i64,
) -> TurnStateObservation {
    TurnStateObservation {
        observation_id: None,
        is_installed: false,
        hunt_attempts: None,
        hunt_seconds: None,
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
        reported_model: None,
        oailb_host: None,
        cookie_expires_at: None,
        // 使用合成正文验证存储边界，失效票应在持久化时被剥离。
        token: Some(token(length, issued_at).value),
        has_token: false,
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
async fn removal_preserves_switches_and_watermark_and_isolates_models() {
    let Some(database) = TestDatabase::create("turn_state_remove").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let admin = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_remove").unwrap();
    repository
        .insert_provider_account(account(id.as_str(), id.as_str()))
        .await
        .unwrap();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "remove-test".to_owned(),
    };
    let issued = Utc::now().timestamp() - 10;
    for model in ["model-a", "model-b"] {
        admin
            .configure_turn_state(
                &id,
                model,
                TurnStateConfig {
                    enabled: true,
                    missing_state_policy: MissingTurnStatePolicy::Pause,
                    ..TurnStateConfig::default()
                },
                &context,
            )
            .await
            .unwrap();
        repository
            .observe_turn_state(
                observation(id.as_str(), model, 292, issued),
                Some(token(292, issued)),
            )
            .await
            .unwrap();
        assert!(repository.install_turn_state(&id, model).await.unwrap());
    }
    assert!(
        admin
            .remove_turn_state(&id, "model-a", issued - 1, &context)
            .await
            .is_err()
    );
    admin
        .remove_turn_state(&id, "model-a", issued, &context)
        .await
        .unwrap();
    assert!(
        admin
            .remove_turn_state(&id, "model-a", issued, &context)
            .await
            .is_err()
    );
    let statuses = admin.turn_state_status(Some(id.as_str())).await.unwrap();
    let removed = statuses
        .iter()
        .find(|item| item.model == "model-a")
        .unwrap();
    assert!(!removed.has_installed_state);
    assert!(!removed.active);
    assert_eq!(
        removed.business_status,
        TurnStateBusinessStatus::WaitingForState
    );
    assert_eq!(removed.issued_at, Some(issued));
    assert!(removed.account_enabled && removed.config.enabled);
    assert!(
        statuses
            .iter()
            .find(|item| item.model == "model-b")
            .unwrap()
            .active
    );
    // 移除只卸载已安装槽位；观测事件留存的正文仍可按签发时间复制，
    // 但保留的签发水位阻止同一张票被重新安装。
    assert_eq!(
        admin
            .turn_state_token(&id, "model-a", issued, None)
            .await
            .unwrap()
            .map(|state| state.value.len()),
        Some(292)
    );
    assert!(
        admin
            .apply_turn_state(&id, "model-a", issued, None, &context)
            .await
            .is_err()
    );
    let bucket = repository
        .turn_state_bucket(&id, "model-a")
        .await
        .unwrap()
        .unwrap();
    assert!(bucket.manages_injection());
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-a", 292, issued),
            Some(token(292, issued)),
        )
        .await
        .unwrap();
    assert!(!repository.install_turn_state(&id, "model-a").await.unwrap());
    let renewed = issued + 1;
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-a", 292, renewed),
            Some(token(292, renewed)),
        )
        .await
        .unwrap();
    assert!(repository.install_turn_state(&id, "model-a").await.unwrap());
    sqlx::query("update provider_accounts set enabled = false where id = $1")
        .bind(id.as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    admin
        .remove_turn_state(&id, "model-a", renewed, &context)
        .await
        .unwrap();
    let statuses = admin.turn_state_status(Some(id.as_str())).await.unwrap();
    assert!(
        statuses
            .iter()
            .all(|item| !item.account_enabled && item.config.enabled)
    );
    assert!(
        statuses
            .iter()
            .all(|item| item.business_status == TurnStateBusinessStatus::ManualDisabled)
    );
    database.close().await;
}

#[tokio::test]
async fn disabled_logging_retains_candidates_and_copy_is_scoped_to_valid_current_identity() {
    let Some(database) = TestDatabase::create("turn_state_copy").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let admin = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_copy").unwrap();
    repository
        .insert_provider_account(account(id.as_str(), id.as_str()))
        .await
        .unwrap();
    let issued = Utc::now().timestamp() - 5;
    let expected = token(292, issued);
    let mut passive = observation(id.as_str(), "model-a", 292, issued);
    passive.source = "passive".to_owned();
    passive.probe_trigger = None;
    repository
        .observe_turn_state(passive, Some(expected.clone()))
        .await
        .unwrap();
    let status = admin
        .turn_state_status(Some(id.as_str()))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(status.observations.is_empty());
    assert_eq!(status.candidate_issued_at, Some(issued));
    assert_eq!(
        admin
            .turn_state_token(&id, "model-a", issued, None)
            .await
            .unwrap()
            .unwrap()
            .value,
        expected.value
    );
    assert!(
        admin
            .turn_state_token(&id, "model-b", issued, None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        admin
            .turn_state_token(&id, "model-a", issued - 1, None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        admin
            .turn_state_token(
                &ProviderAccountId::new("acct_other").unwrap(),
                "model-a",
                issued,
                None
            )
            .await
            .unwrap()
            .is_none()
    );
    let mut manual = observation(id.as_str(), "model-a", 312, issued + 1);
    manual.probe_trigger = Some("manual".to_owned());
    repository
        .observe_turn_state(manual.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        admin.turn_state_status(Some(id.as_str())).await.unwrap()[0]
            .observations
            .len(),
        1
    );
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "copy-test".to_owned(),
    };
    admin
        .apply_turn_state(&id, "model-a", issued, None, &context)
        .await
        .unwrap();
    assert!(
        admin
            .turn_state_token(&id, "model-a", issued, None)
            .await
            .unwrap()
            .is_some()
    );
    sqlx::query("update provider_accounts set enabled = false where id = $1")
        .bind(id.as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    repository.observe_turn_state(manual, None).await.unwrap();
    assert_eq!(
        admin.turn_state_status(Some(id.as_str())).await.unwrap()[0]
            .observations
            .len(),
        1
    );
    sqlx::query(
        "update account_turn_states set upstream_user_id = 'different' where account_id = $1",
    )
    .bind(id.as_str())
    .execute(&database.pool)
    .await
    .unwrap();
    assert!(
        admin
            .turn_state_token(&id, "model-a", issued, None)
            .await
            .unwrap()
            .is_none()
    );
    let expired = issued - 3600;
    sqlx::query("update account_turn_states set upstream_user_id = $1, turn_state_override = $2, current_issued_at = $3 where account_id = $1")
        .bind(id.as_str()).bind(token(292, expired).value).bind(expired).execute(&database.pool).await.unwrap();
    assert!(
        admin
            .turn_state_token(&id, "model-a", expired, None)
            .await
            .unwrap()
            .is_none()
    );
    database.close().await;
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
            .apply_turn_state(&id, "model-b", issued, None, &context)
            .await
            .is_err()
    );
    assert!(
        admin
            .apply_turn_state(&id, "model-a", issued - 1, None, &context)
            .await
            .is_err()
    );
    let (first, second) = tokio::join!(
        admin.apply_turn_state(&id, "model-a", issued, None, &context),
        admin.apply_turn_state(&id, "model-a", issued, None, &context),
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
            .apply_turn_state(&id, "model-a", issued, None, &context)
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
            .apply_turn_state(&id, "model-a", expired, None, &context)
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

#[tokio::test]
async fn non_target_token_is_kept_in_history_copyable_and_installable_after_retarget() {
    let Some(database) = TestDatabase::create("turn_state_nontarget").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let admin = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_nontarget").unwrap();
    repository
        .insert_provider_account(account(id.as_str(), id.as_str()))
        .await
        .unwrap();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "nontarget-test".to_owned(),
    };
    admin
        .configure_turn_state(
            &id,
            "model-a",
            TurnStateConfig {
                enabled: true,
                ..TurnStateConfig::default()
            },
            &context,
        )
        .await
        .unwrap();
    let issued = Utc::now().timestamp() - 5;
    let expected = token(312, issued);
    let mut miss = observation(id.as_str(), "model-a", 312, issued);
    miss.reported_model = Some("gpt-5.6-luna".to_owned());
    repository
        .observe_turn_state(miss, Some(expected.clone()))
        .await
        .unwrap();
    // 非目标票不进候选也不占水位；正文随观测事件保存，状态接口只暴露 hasToken 标记。
    let status = admin
        .turn_state_status(Some(id.as_str()))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(status.candidate_issued_at.is_none());
    assert!(status.observations[0].has_token);
    assert_eq!(
        status.observations[0].reported_model.as_deref(),
        Some("gpt-5.6-luna")
    );
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains(&expected.value)
    );
    // 复制入口按签发时间从观测事件取回正文；目标长度不符时不能安装。
    assert_eq!(
        admin
            .turn_state_token(&id, "model-a", issued, None)
            .await
            .unwrap()
            .unwrap()
            .value,
        expected.value
    );
    assert!(!repository.install_turn_state(&id, "model-a").await.unwrap());
    assert!(
        admin
            .apply_turn_state(&id, "model-a", issued, None, &context)
            .await
            .is_err()
    );
    // 目标长度改为 312 后，同一张历史票可直接应用安装。
    admin
        .configure_turn_state(
            &id,
            "model-a",
            TurnStateConfig {
                enabled: true,
                target_length: 312,
                ..TurnStateConfig::default()
            },
            &context,
        )
        .await
        .unwrap();
    admin
        .apply_turn_state(&id, "model-a", issued, None, &context)
        .await
        .unwrap();
    let status = admin
        .turn_state_status(Some(id.as_str()))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(status.active);
    assert!(status.manual_override);
    assert_eq!(status.token_length, Some(312));
    assert_eq!(status.installations[0].issued_at, issued);
    database.close().await;
}

#[tokio::test]
async fn history_selection_is_exact_even_when_tokens_share_timestamp_and_length() {
    let Some(database) = TestDatabase::create("turn_state_exact_history").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let admin = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_exact_history").unwrap();
    repository
        .insert_provider_account(account(id.as_str(), id.as_str()))
        .await
        .unwrap();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "exact-history".to_owned(),
    };
    admin
        .configure_turn_state(
            &id,
            "model-a",
            TurnStateConfig {
                enabled: true,
                ..TurnStateConfig::default()
            },
            &context,
        )
        .await
        .unwrap();
    let issued = Utc::now().timestamp() - 5;
    let first = token(292, issued);
    let mut bytes = URL_SAFE.decode(&first.value).unwrap();
    bytes[10] = 1;
    let second = TurnStateToken::parse(&URL_SAFE.encode(bytes)).unwrap();
    let miss = token(312, issued);
    for value in [&first, &second, &miss] {
        let mut observed = observation(id.as_str(), "model-a", value.value.len(), issued);
        observed.started_at = Some(Utc::now().timestamp() - 61);
        observed.token = Some(value.value.clone());
        repository
            .observe_turn_state(observed, Some(value.clone()))
            .await
            .unwrap();
    }
    let status = admin
        .turn_state_status(Some(id.as_str()))
        .await
        .unwrap()
        .remove(0);
    let ids: Vec<i64> = status
        .observations
        .iter()
        .rev()
        .map(|row| row.observation_id.as_ref().unwrap().parse().unwrap())
        .collect();
    for (observation_id, value) in ids.iter().zip([&first, &second, &miss]) {
        assert_eq!(
            admin
                .turn_state_token(&id, "model-a", issued, Some(*observation_id))
                .await
                .unwrap()
                .unwrap()
                .value,
            value.value
        );
    }
    assert!(
        admin
            .turn_state_token(&id, "model-b", issued, Some(ids[1]))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        admin
            .turn_state_token(&id, "model-a", issued - 1, Some(ids[1]))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        admin
            .turn_state_token(&id, "model-a", issued, Some(i64::MAX))
            .await
            .unwrap()
            .is_none()
    );
    // 312 历史行不得借用同秒的 292 候选安装。
    assert!(
        admin
            .apply_turn_state(&id, "model-a", issued, Some(ids[2]), &context)
            .await
            .is_err()
    );
    assert!(
        admin
            .apply_turn_state(&id, "model-a", issued, Some(i64::MAX), &context)
            .await
            .is_err()
    );
    admin
        .apply_turn_state(&id, "model-a", issued, Some(ids[1]), &context)
        .await
        .unwrap();
    let status = admin
        .turn_state_status(Some(id.as_str()))
        .await
        .unwrap()
        .remove(0);
    assert!(status.has_installed_state);
    assert!(status.candidate_issued_at.is_none());
    assert!(!status.observations[0].is_installed);
    assert!(status.observations[1].is_installed);
    assert!(!status.observations[2].is_installed);
    assert_eq!(status.installations[0].attempts, 2);
    assert!(status.installations[0].hunt_seconds >= 61);
    assert_eq!(
        admin
            .turn_state_token(&id, "model-a", issued, None)
            .await
            .unwrap()
            .unwrap()
            .value,
        second.value
    );
    admin
        .remove_turn_state(&id, "model-a", issued, &context)
        .await
        .unwrap();
    let status = admin
        .turn_state_status(Some(id.as_str()))
        .await
        .unwrap()
        .remove(0);
    assert!(status.observations.iter().all(|row| !row.is_installed));
    // 无 ID 的旧调用不能在多个不同正文之间任意挑选。
    assert!(
        admin
            .turn_state_token(&id, "model-a", issued, None)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        admin
            .turn_state_token(&id, "model-a", issued, Some(ids[0]))
            .await
            .unwrap()
            .unwrap()
            .value,
        first.value
    );
    database.close().await;
}

#[tokio::test]
async fn invalid_history_bodies_are_rejected_and_legacy_extreme_dates_do_not_break_cleanup() {
    let Some(database) = TestDatabase::create("turn_state_invalid_history").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let admin = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_invalid_history").unwrap();
    repository
        .insert_provider_account(account(id.as_str(), id.as_str()))
        .await
        .unwrap();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "invalid-history".to_owned(),
    };
    admin
        .configure_turn_state(
            &id,
            "model-a",
            TurnStateConfig {
                enabled: true,
                ..TurnStateConfig::default()
            },
            &context,
        )
        .await
        .unwrap();
    let now = Utc::now().timestamp();
    for issued in [0, now - 3601, now + 3600, i64::MAX] {
        let observed = observation(id.as_str(), "model-a", 312, issued);
        assert!(!format!("{observed:?}").contains(observed.token.as_ref().unwrap()));
        repository
            .observe_turn_state(observed.clone(), None)
            .await
            .unwrap();
        let status = admin
            .turn_state_status(Some(id.as_str()))
            .await
            .unwrap()
            .remove(0);
        assert!(!status.observations[0].has_token);
        // 模拟旧版本已经留存了未来/过期正文的数据库。
        sqlx::query("insert into account_turn_state_events(account_id, model, event_kind, detail) values ($1, 'model-a', 'observation', $2)")
            .bind(id.as_str()).bind(serde_json::to_value(observed).unwrap()).execute(&database.pool).await.unwrap();
    }
    let fresh = token(292, now - 1);
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-a", 292, fresh.issued_at),
            Some(fresh.clone()),
        )
        .await
        .unwrap();
    assert!(repository.install_turn_state(&id, "model-a").await.unwrap());
    repository.turn_state_buckets().await.unwrap();
    let remaining: i64 = sqlx::query_scalar(
        "select count(*) from account_turn_state_events where account_id = $1 and detail ? 'token'",
    )
    .bind(id.as_str())
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(remaining, 1);
    assert_eq!(
        admin
            .turn_state_token(&id, "model-a", fresh.issued_at, None)
            .await
            .unwrap()
            .unwrap()
            .value,
        fresh.value
    );
    database.close().await;
}

#[tokio::test]
async fn pause_voids_the_installed_ticket_when_the_reported_model_changes() {
    let Some(database) = TestDatabase::create("turn_state_detach").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let admin = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_detach").unwrap();
    repository
        .insert_provider_account(account(id.as_str(), id.as_str()))
        .await
        .unwrap();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "detach".to_owned(),
    };
    admin
        .configure_turn_state(
            &id,
            "model-a",
            TurnStateConfig {
                enabled: true,
                missing_state_policy: MissingTurnStatePolicy::Pause,
                detect_actual_model: true,
                ..TurnStateConfig::default()
            },
            &context,
        )
        .await
        .unwrap();
    let issued = Utc::now().timestamp() - 5;
    let mut seen = observation(id.as_str(), "model-a", 292, issued);
    seen.reported_model = Some("gpt-5.6-astra".to_owned());
    repository
        .observe_turn_state(seen, Some(token(292, issued)))
        .await
        .unwrap();
    assert!(repository.install_turn_state(&id, "model-a").await.unwrap());
    let installed: String = sqlx::query_scalar(
        "select turn_state_override from account_turn_states where account_id = $1",
    )
    .bind(id.as_str())
    .fetch_one(&database.pool)
    .await
    .unwrap();
    let attached: String =
        sqlx::query_scalar("select attached_model from account_turn_states where account_id = $1")
            .bind(id.as_str())
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(attached, "gpt-5.6-astra");
    assert!(
        !repository
            .observe_installed_model(&id, "model-a", &installed, "GPT-5.6-ASTRA", true)
            .await
            .unwrap()
    );
    assert!(
        !repository
            .observe_installed_model(&id, "model-a", &installed, "gpt-5.6-luna", false)
            .await
            .unwrap()
    );
    let still: Option<String> = sqlx::query_scalar(
        "select turn_state_override from account_turn_states where account_id = $1",
    )
    .bind(id.as_str())
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(still.as_deref(), Some(installed.as_str()));
    assert!(
        repository
            .observe_installed_model(&id, "model-a", &installed, "gpt-5.6-luna", true)
            .await
            .unwrap()
    );
    let cleared: Option<String> = sqlx::query_scalar(
        "select turn_state_override from account_turn_states where account_id = $1",
    )
    .bind(id.as_str())
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert!(cleared.is_none());
    let watermark: Option<i64> = sqlx::query_scalar(
        "select current_issued_at from account_turn_states where account_id = $1",
    )
    .bind(id.as_str())
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(watermark, Some(issued));
    database.close().await;
}
