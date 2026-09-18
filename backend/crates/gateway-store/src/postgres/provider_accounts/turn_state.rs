//! 路由令牌的原子候选更新、安装及有界观测历史。

use gateway_core::account::{
    MissingTurnStatePolicy, TurnStateBucket, TurnStateBusinessStatus, TurnStateConfig,
    TurnStateInstallation, TurnStateObservation, TurnStateStatus, TurnStateToken,
};

use super::*;

fn unavailable(_: impl std::fmt::Debug) -> CoreStoreError {
    CoreStoreError::new(CoreStoreErrorKind::Unavailable)
}

fn bucket(row: sqlx::postgres::PgRow) -> Result<TurnStateBucket, CoreStoreError> {
    let config: serde_json::Value = row.try_get("config").map_err(unavailable)?;
    let config: TurnStateConfig = serde_json::from_value(config).map_err(unavailable)?;
    let issued_at = row
        .try_get::<Option<i64>, _>("current_issued_at")
        .map_err(unavailable)?;
    let candidate_issued_at = row
        .try_get::<Option<i64>, _>("candidate_issued_at")
        .map_err(unavailable)?;
    let current = row
        .try_get::<Option<String>, _>("turn_state_override")
        .map_err(unavailable)?
        .zip(issued_at)
        .map(|(value, issued_at)| TurnStateToken { value, issued_at });
    let candidate = row
        .try_get::<Option<String>, _>("candidate")
        .map_err(unavailable)?
        .zip(candidate_issued_at)
        .map(|(value, issued_at)| TurnStateToken { value, issued_at });
    let now = Utc::now().timestamp();
    Ok(TurnStateBucket {
        account_id: row.try_get("account_id").map_err(unavailable)?,
        upstream_account_id: row.try_get("upstream_account_id").map_err(unavailable)?,
        upstream_user_id: row.try_get("upstream_user_id").map_err(unavailable)?,
        model: row.try_get("model").map_err(unavailable)?,
        current: current.filter(|token| token.is_fresh(now, config.ttl_seconds)),
        current_issued_at: issued_at,
        current_length: row
            .try_get::<Option<i32>, _>("current_length")
            .map_err(unavailable)?
            .map(|n| n as usize),
        candidate: candidate.filter(|token| token.is_fresh(now, config.ttl_seconds)),
        hunt_attempts: row
            .try_get::<i64, _>("hunt_attempts")
            .map_err(unavailable)?
            .max(0) as u64,
        next_probe_at: row.try_get("next_probe_at").map_err(unavailable)?,
        manual_probe_requested_at: row
            .try_get("manual_probe_requested_at")
            .map_err(unavailable)?,
        manual_override: row.try_get("manual_override").map_err(unavailable)?,
        config,
    })
}

impl PgProviderAccountRepository {
    pub(super) async fn load_turn_state_buckets_for_model(
        &self,
        accounts: &[CoreProviderAccountId],
        model: &str,
    ) -> Result<Vec<TurnStateBucket>, CoreStoreError> {
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query(
            "select * from account_turn_states where account_id = any($1::text[]) and model = $2",
        )
        .bind(
            accounts
                .iter()
                .map(CoreProviderAccountId::as_str)
                .collect::<Vec<_>>(),
        )
        .bind(model)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?
        .into_iter()
        .map(bucket)
        .collect()
    }

    pub(super) async fn load_turn_state_buckets(
        &self,
    ) -> Result<Vec<TurnStateBucket>, CoreStoreError> {
        // 清理不依赖是否启用轮换，停用的桶也不能继续持有过期正文。
        sqlx::query("update account_turn_states set turn_state_override = case when current_issued_at + (config->>'ttlSeconds')::bigint <= extract(epoch from now()) then null else turn_state_override end, candidate = case when candidate_issued_at + (config->>'ttlSeconds')::bigint <= extract(epoch from now()) then null else candidate end where (turn_state_override is not null and current_issued_at + (config->>'ttlSeconds')::bigint <= extract(epoch from now())) or (candidate is not null and candidate_issued_at + (config->>'ttlSeconds')::bigint <= extract(epoch from now()))")
            .execute(&self.pool).await.map_err(unavailable)?;
        sqlx::query("select * from account_turn_states order by account_id, model")
            .fetch_all(&self.pool)
            .await
            .map_err(unavailable)?
            .into_iter()
            .map(bucket)
            .collect()
    }

    pub(super) async fn load_turn_state_bucket(
        &self,
        account: &CoreProviderAccountId,
        model: &str,
    ) -> Result<Option<TurnStateBucket>, CoreStoreError> {
        sqlx::query("select * from account_turn_states where account_id = $1 and model = $2")
            .bind(account.as_str())
            .bind(model)
            .fetch_optional(&self.pool)
            .await
            .map_err(unavailable)?
            .map(bucket)
            .transpose()
    }

    pub(super) async fn record_turn_state(
        &self,
        mut observation: TurnStateObservation,
        candidate: Option<TurnStateToken>,
    ) -> Result<(), CoreStoreError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let default_config =
            serde_json::to_value(TurnStateConfig::default()).map_err(unavailable)?;
        sqlx::query("insert into account_turn_states(account_id, model, config, upstream_account_id, upstream_user_id) select id, $2, $3, upstream_account_id, upstream_user_id from provider_accounts where id = $1 and provider_kind = 'openai' on conflict do nothing")
            .bind(&observation.account_id).bind(&observation.model).bind(default_config).execute(&mut *tx).await.map_err(unavailable)?;
        // 同桶观测串行化，避免候选竞态与并发历史裁剪失效。
        let row = sqlx::query(
            "select * from account_turn_states where account_id = $1 and model = $2 and upstream_account_id is not distinct from $3 and upstream_user_id is not distinct from $4 for update",
        )
        .bind(&observation.account_id)
        .bind(&observation.model)
        .bind(&observation.upstream_account_id)
        .bind(&observation.upstream_user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?;
        let Some(row) = row else {
            return Ok(());
        };
        let hunt_started_at: Option<i64> = row.try_get("hunt_started_at").map_err(unavailable)?;
        let mut state = bucket(row)?;
        if observation.outcome == "candidate"
            && candidate.as_ref().is_some_and(|token| {
                !token.is_newer_than(state.current_issued_at)
                    || !token.is_newer_than(state.candidate.as_ref().map(|old| old.issued_at))
            })
        {
            observation.outcome = "not_newer".to_owned();
        }
        let started_at = observation.started_at.unwrap_or(observation.observed_at);
        if observation.source == "probe" && observation.probe_id.is_some() {
            state.hunt_attempts = state.hunt_attempts.saturating_add(1);
            sqlx::query("update account_turn_states set hunt_attempts = hunt_attempts + 1, hunt_started_at = coalesce(hunt_started_at, $3) where account_id = $1 and model = $2")
                .bind(&observation.account_id).bind(&observation.model).bind(started_at)
                .execute(&mut *tx).await.map_err(unavailable)?;
        }
        if let Some(token) = candidate.filter(|token| {
            token.value.len() == state.config.target_length
                && TurnStateToken::parse(&token.value)
                    .is_some_and(|parsed| parsed.issued_at == token.issued_at)
                && token.is_fresh(Utc::now().timestamp(), state.config.ttl_seconds)
                && token.is_newer_than(state.current_issued_at)
                && token.is_newer_than(state.candidate.as_ref().map(|old| old.issued_at))
        }) {
            let (attempts, hunt_started_at) = if observation.source == "probe" {
                (
                    state.hunt_attempts as i64,
                    hunt_started_at.unwrap_or(started_at),
                )
            } else {
                (0, observation.observed_at)
            };
            sqlx::query("update account_turn_states set candidate = $3, candidate_issued_at = $4, candidate_source = $5, candidate_observed_at = $6, candidate_attempts = $7, candidate_hunt_started_at = $8 where account_id = $1 and model = $2")
                .bind(&observation.account_id).bind(&observation.model).bind(&token.value).bind(token.issued_at).bind(&observation.source)
                .bind(observation.observed_at).bind(attempts).bind(hunt_started_at)
                .execute(&mut *tx).await.map_err(unavailable)?;
        }
        append_event(
            &mut tx,
            &observation.account_id,
            &observation.model,
            "observation",
            serde_json::to_value(&observation).map_err(unavailable)?,
        )
        .await?;
        tx.commit().await.map_err(unavailable)
    }

    pub(super) async fn install_turn_state_candidate(
        &self,
        account: &CoreProviderAccountId,
        model: &str,
    ) -> Result<bool, CoreStoreError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let installed = install_candidate(&mut tx, account, model, None).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(installed)
    }

    pub(super) async fn turn_state_statuses(
        &self,
        account_id: Option<&str>,
    ) -> Result<Vec<TurnStateStatus>, CoreStoreError> {
        let rows = sqlx::query("select s.*, a.name as account_name, a.email as account_email, a.enabled as account_enabled, (a.authentication_kind = 'oauth' and s.upstream_account_id is not distinct from a.upstream_account_id and s.upstream_user_id is not distinct from a.upstream_user_id) as identity_matches, coalesce((select jsonb_agg(e.detail order by e.id desc) from account_turn_state_events e where e.account_id = s.account_id and e.model = s.model and e.event_kind = 'observation'), '[]'::jsonb) as observations, coalesce((select jsonb_agg(e.detail order by e.id desc) from account_turn_state_events e where e.account_id = s.account_id and e.model = s.model and e.event_kind = 'installation'), '[]'::jsonb) as installations from account_turn_states s join provider_accounts a on a.id = s.account_id where ($1::text is null or s.account_id = $1) order by s.account_id, s.model")
            .bind(account_id).fetch_all(&self.pool).await.map_err(unavailable)?;
        rows.into_iter()
            .map(|row| {
                let account_name = row.try_get("account_name").map_err(unavailable)?;
                let account_email = row.try_get("account_email").map_err(unavailable)?;
                let account_enabled: bool = row.try_get("account_enabled").map_err(unavailable)?;
                let identity_matches: bool =
                    row.try_get("identity_matches").map_err(unavailable)?;
                let observations =
                    serde_json::from_value(row.try_get("observations").map_err(unavailable)?)
                        .map_err(unavailable)?;
                let installations =
                    serde_json::from_value(row.try_get("installations").map_err(unavailable)?)
                        .map_err(unavailable)?;
                let state = bucket(row)?;
                let installed = state
                    .installed_token(Utc::now().timestamp())
                    .filter(|_| identity_matches);
                let active = account_enabled && installed.is_some();
                let business_status = if !account_enabled {
                    TurnStateBusinessStatus::ManualDisabled
                } else if state.config.missing_state_policy == MissingTurnStatePolicy::Pause
                    && !active
                {
                    TurnStateBusinessStatus::WaitingForState
                } else {
                    TurnStateBusinessStatus::Ready
                };
                let next_probe_at = if !account_enabled || !state.config.enabled {
                    None
                } else if let Some(token) = installed.filter(|token| {
                    token.is_fresh(Utc::now().timestamp(), state.config.refresh_after_seconds)
                }) {
                    Some(token.issued_at + state.config.refresh_after_seconds as i64)
                } else {
                    Some(
                        state
                            .next_probe_at
                            .unwrap_or_else(|| Utc::now().timestamp()),
                    )
                };
                Ok(TurnStateStatus {
                    account_id: state.account_id,
                    account_name,
                    account_email,
                    model: state.model,
                    active,
                    business_status,
                    account_enabled,
                    hunt_attempts: state.hunt_attempts,
                    next_probe_at,
                    manual_probe_requested_at: state.manual_probe_requested_at,
                    manual_override: state.manual_override,
                    candidate_issued_at: state.candidate.as_ref().map(|token| token.issued_at),
                    candidate_length: state.candidate.as_ref().map(|token| token.value.len()),
                    config: state.config,
                    token_length: state.current_length,
                    issued_at: state.current_issued_at,
                    age_seconds: state
                        .current_issued_at
                        .map(|time| Utc::now().timestamp().saturating_sub(time)),
                    observations,
                    installations,
                })
            })
            .collect()
    }
}

pub(super) async fn install_candidate(
    tx: &mut Transaction<'_, Postgres>,
    account: &CoreProviderAccountId,
    model: &str,
    expected_issued_at: Option<i64>,
) -> Result<bool, CoreStoreError> {
    // 手动应用必须匹配用户选中的候选；自动安装仍受轮换开关控制。
    let row = sqlx::query("update account_turn_states s set turn_state_override = candidate, current_issued_at = candidate_issued_at, current_length = length(candidate), manual_override = ($3::bigint is not null), candidate = null, hunt_attempts = 0, hunt_started_at = null, next_probe_at = candidate_issued_at + (config->>'refreshAfterSeconds')::bigint where account_id = $1 and model = $2 and (($3::bigint is null and (config->>'enabled')::boolean) or candidate_issued_at = $3) and candidate is not null and length(candidate) = (config->>'targetLength')::integer and candidate_issued_at <= extract(epoch from now()) and candidate_issued_at + (config->>'ttlSeconds')::bigint > extract(epoch from now()) and (current_issued_at is null or candidate_issued_at > current_issued_at) and exists (select 1 from provider_accounts a where a.id = s.account_id and a.enabled and a.provider_kind = 'openai' and a.authentication_kind = 'oauth' and a.upstream_account_id is not distinct from s.upstream_account_id and a.upstream_user_id is not distinct from s.upstream_user_id) returning current_issued_at, current_length, candidate_source, candidate_observed_at, candidate_attempts, candidate_hunt_started_at")
        .bind(account.as_str()).bind(model).bind(expected_issued_at)
        .fetch_optional(&mut **tx).await.map_err(unavailable)?;
    let Some(row) = row else {
        return Ok(false);
    };
    let acquired_at: i64 = row.try_get("candidate_observed_at").map_err(unavailable)?;
    let started_at: i64 = row
        .try_get("candidate_hunt_started_at")
        .map_err(unavailable)?;
    let installation = TurnStateInstallation {
        installed_at: Utc::now().timestamp(),
        issued_at: row.try_get("current_issued_at").map_err(unavailable)?,
        token_length: row
            .try_get::<i32, _>("current_length")
            .map_err(unavailable)? as usize,
        source: row.try_get("candidate_source").map_err(unavailable)?,
        acquired_at,
        attempts: row
            .try_get::<i64, _>("candidate_attempts")
            .map_err(unavailable)?
            .max(0) as u64,
        hunt_seconds: acquired_at.saturating_sub(started_at).max(0) as u64,
    };
    append_event(
        tx,
        account.as_str(),
        model,
        "installation",
        serde_json::to_value(installation).map_err(unavailable)?,
    )
    .await?;
    Ok(true)
}

async fn append_event(
    tx: &mut Transaction<'_, Postgres>,
    account: &str,
    model: &str,
    kind: &str,
    detail: serde_json::Value,
) -> Result<(), CoreStoreError> {
    let source = detail
        .get("source")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    sqlx::query("insert into account_turn_state_events(account_id, model, event_kind, detail) values ($1, $2, $3, $4)")
        .bind(account).bind(model).bind(kind).bind(detail).execute(&mut **tx).await.map_err(unavailable)?;
    sqlx::query("delete from account_turn_state_events where account_id = $1 and model = $2 and event_kind = $3 and ($3 <> 'observation' or detail->>'source' = $4) and id not in (select id from account_turn_state_events where account_id = $1 and model = $2 and event_kind = $3 and ($3 <> 'observation' or detail->>'source' = $4) order by id desc limit 100)")
        .bind(account).bind(model).bind(kind).bind(source).execute(&mut **tx).await.map_err(unavailable)?;
    Ok(())
}
