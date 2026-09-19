use super::turn_state::{observation, token};
use super::*;
use gateway_core::account::TurnStateConfig;

#[tokio::test]
async fn notifications_follow_committed_installations_and_bound_retries_without_copying_tokens() {
    let Some(database) = TestDatabase::create("state_notification").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let admin = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_notice").unwrap();
    repository
        .insert_provider_account(account(id.as_str(), id.as_str()))
        .await
        .unwrap();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "notice-test".to_owned(),
    };
    admin
        .configure_turn_state(
            &id,
            "model-a",
            TurnStateConfig {
                feishu_webhook_url: "https://open.feishu.cn/open-apis/bot/v2/hook/test-only"
                    .to_owned(),
                ..TurnStateConfig::default()
            },
            &context,
        )
        .await
        .unwrap();
    let issued = Utc::now().timestamp() - 100;
    let first = token(292, issued);
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-a", 292, issued),
            Some(first.clone()),
        )
        .await
        .unwrap();
    assert!(
        repository
            .claim_turn_state_notifications()
            .await
            .unwrap()
            .is_empty()
    );
    admin
        .apply_turn_state(&id, "model-a", issued, None, &context)
        .await
        .unwrap();
    let (left, right) = tokio::join!(
        repository.claim_turn_state_notifications(),
        repository.claim_turn_state_notifications()
    );
    let mut notices = left.unwrap();
    notices.extend(right.unwrap());
    assert_eq!(notices.len(), 1);
    let notice = notices.pop().unwrap();
    assert_eq!(notice.token.value, first.value);
    assert!(notice.manual);
    assert!(notice.previous_installed_at.is_none());
    assert_eq!(notice.installation.attempts, 1);
    let first_installed_at = notice.installation.installed_at;
    let stored: serde_json::Value =
        sqlx::query_scalar("select row_to_json(n) from account_turn_state_notifications n")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert!(!stored.to_string().contains(&first.value));
    assert!(!stored.to_string().contains("test-only"));
    assert!(
        repository
            .claim_turn_state_notifications()
            .await
            .unwrap()
            .is_empty()
    );
    repository
        .finish_turn_state_notification(&id, "model-a", issued, false)
        .await
        .unwrap();
    for _ in 1..5 {
        sqlx::query("update account_turn_state_notifications set next_attempt_at = 0")
            .execute(&database.pool)
            .await
            .unwrap();
        assert_eq!(
            repository
                .claim_turn_state_notifications()
                .await
                .unwrap()
                .len(),
            1
        );
    }
    sqlx::query("update account_turn_state_notifications set next_attempt_at = 0")
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(
        repository
            .claim_turn_state_notifications()
            .await
            .unwrap()
            .is_empty()
    );
    let renewed = issued + 1;
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-a", 292, renewed),
            Some(token(292, renewed)),
        )
        .await
        .unwrap();
    admin
        .apply_turn_state(&id, "model-a", renewed, None, &context)
        .await
        .unwrap();
    // 旧通知的晚到确认不能误确认新安装的通知。
    repository
        .finish_turn_state_notification(&id, "model-a", issued, true)
        .await
        .unwrap();
    let notice = repository
        .claim_turn_state_notifications()
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(notice.previous_installed_at, Some(first_installed_at));
    assert_eq!(notice.token.issued_at, renewed);
    repository
        .finish_turn_state_notification(&id, "model-a", renewed, true)
        .await
        .unwrap();
    assert!(
        repository
            .claim_turn_state_notifications()
            .await
            .unwrap()
            .is_empty()
    );
    let latest = renewed + 1;
    repository
        .observe_turn_state(
            observation(id.as_str(), "model-a", 292, latest),
            Some(token(292, latest)),
        )
        .await
        .unwrap();
    admin
        .apply_turn_state(&id, "model-a", latest, None, &context)
        .await
        .unwrap();
    sqlx::query("update provider_accounts set enabled = false where id = $1")
        .bind(id.as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(
        repository
            .claim_turn_state_notifications()
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("update provider_accounts set enabled = true where id = $1")
        .bind(id.as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    admin
        .remove_turn_state(&id, "model-a", latest, &context)
        .await
        .unwrap();
    assert!(
        repository
            .claim_turn_state_notifications()
            .await
            .unwrap()
            .is_empty()
    );
    database.close().await;
}
