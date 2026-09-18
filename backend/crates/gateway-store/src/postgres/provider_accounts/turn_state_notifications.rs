//! 通知任务与安装同事务提交，领取时再次核对当前票和账号身份。

use gateway_core::account::{TurnStateInstallation, TurnStateNotification, TurnStateToken};
use sqlx::{Postgres, Transaction};

use super::*;

pub(super) async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    account: &CoreProviderAccountId,
    model: &str,
    installation: &TurnStateInstallation,
) -> Result<(), CoreStoreError> {
    sqlx::query("insert into account_turn_state_notifications(account_id, model, issued_at, installation, previous_installed_at, next_attempt_at) select s.account_id, s.model, $3, $4, (select (e.detail->>'installedAt')::bigint from account_turn_state_events e where e.account_id = s.account_id and e.model = s.model and e.event_kind = 'installation' order by e.id desc limit 1), $5 from account_turn_states s where s.account_id = $1 and s.model = $2 and coalesce(s.config->>'feishuWebhookUrl', '') <> '' on conflict (account_id, model) do update set issued_at = excluded.issued_at, installation = excluded.installation, previous_installed_at = excluded.previous_installed_at, next_attempt_at = excluded.next_attempt_at, attempts = 0, sent_at = null")
        .bind(account.as_str()).bind(model).bind(installation.issued_at)
        .bind(serde_json::to_value(installation).map_err(unavailable)?)
        .bind(installation.installed_at).execute(&mut **tx).await.map_err(unavailable)?;
    Ok(())
}

impl PgProviderAccountRepository {
    pub(super) async fn claim_state_notifications(
        &self,
    ) -> Result<Vec<TurnStateNotification>, CoreStoreError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let rows = sqlx::query("select n.issued_at, n.installation, n.previous_installed_at, s.account_id, s.model, s.config, s.turn_state_override, s.manual_override, a.name as account_name from account_turn_state_notifications n join account_turn_states s using (account_id, model) join provider_accounts a on a.id = s.account_id where n.sent_at is null and n.attempts < 5 and n.next_attempt_at <= extract(epoch from now()) and a.enabled and a.provider_kind = 'openai' and a.authentication_kind = 'oauth' and s.upstream_account_id is not distinct from a.upstream_account_id and s.upstream_user_id is not distinct from a.upstream_user_id and n.issued_at = s.current_issued_at and s.turn_state_override is not null and coalesce(s.config->>'feishuWebhookUrl', '') <> '' and n.issued_at + (s.config->>'ttlSeconds')::bigint > extract(epoch from now()) order by n.next_attempt_at limit 16 for update of n skip locked")
            .fetch_all(&mut *tx).await.map_err(unavailable)?;
        let mut notifications = Vec::new();
        for row in rows {
            let config: gateway_core::account::TurnStateConfig =
                serde_json::from_value(row.try_get("config").map_err(unavailable)?)
                    .map_err(unavailable)?;
            let value: String = row.try_get("turn_state_override").map_err(unavailable)?;
            let issued_at: i64 = row.try_get("issued_at").map_err(unavailable)?;
            let Some(token) = TurnStateToken::parse(&value).filter(|token| {
                config.is_valid()
                    && token.issued_at == issued_at
                    && value.len() == config.target_length
                    && token.is_fresh(Utc::now().timestamp(), config.ttl_seconds)
            }) else {
                continue;
            };
            let account_id: String = row.try_get("account_id").map_err(unavailable)?;
            let model: String = row.try_get("model").map_err(unavailable)?;
            // 发送超时短于领取窗口；进程崩溃后最多重试五次，不阻塞轮换或业务。
            sqlx::query("update account_turn_state_notifications set attempts = attempts + 1, next_attempt_at = extract(epoch from now())::bigint + 60 where account_id = $1 and model = $2")
                .bind(&account_id).bind(&model).execute(&mut *tx).await.map_err(unavailable)?;
            notifications.push(TurnStateNotification {
                account_id,
                model,
                token,
                account_name: row.try_get("account_name").map_err(unavailable)?,
                webhook_url: config.feishu_webhook_url,
                installation: serde_json::from_value(
                    row.try_get("installation").map_err(unavailable)?,
                )
                .map_err(unavailable)?,
                previous_installed_at: row.try_get("previous_installed_at").map_err(unavailable)?,
                manual: row.try_get("manual_override").map_err(unavailable)?,
            });
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(notifications)
    }
}

fn unavailable<T>(_error: T) -> CoreStoreError {
    CoreStoreError::new(gateway_core::error::StoreErrorKind::Unavailable)
}
