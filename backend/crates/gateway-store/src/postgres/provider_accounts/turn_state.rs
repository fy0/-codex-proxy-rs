//! 路由令牌的原子候选更新、安装及有界观测历史。

use gateway_core::account::{
    MissingTurnStatePolicy, RoutingCookie, TurnStateBucket, TurnStateBusinessStatus,
    TurnStateConfig, TurnStateInstallation, TurnStateObservation, TurnStateStatus, TurnStateToken,
};

use super::*;

fn unavailable(_: impl std::fmt::Debug) -> CoreStoreError {
    CoreStoreError::new(CoreStoreErrorKind::Unavailable)
}

pub(super) fn stored_pair(row: &sqlx::postgres::PgRow, column: &str) -> Option<RoutingCookie> {
    row.try_get::<Option<serde_json::Value>, _>(column)
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_value::<RoutingCookie>(value).ok())
        .filter(|pair| pair.has_pair())
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
    let current_expires_at = row
        .try_get::<Option<i64>, _>("current_expires_at")
        .map_err(unavailable)?;
    let candidate_expires_at = row
        .try_get::<Option<i64>, _>("candidate_expires_at")
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
        current: current.filter(|token| token.live(now, config.ttl_seconds, current_expires_at)),
        current_issued_at: issued_at,
        current_length: row
            .try_get::<Option<i32>, _>("current_length")
            .map_err(unavailable)?
            .map(|n| n as usize),
        current_expires_at,
        installed_pair: stored_pair(&row, "installed_pair"),
        candidate: candidate
            .filter(|token| token.live(now, config.ttl_seconds, candidate_expires_at)),
        candidate_expires_at,
        candidate_pair: stored_pair(&row, "candidate_pair"),
        hunt_attempts: row
            .try_get::<i64, _>("hunt_attempts")
            .map_err(unavailable)?
            .max(0) as u64,
        next_probe_at: row.try_get("next_probe_at").map_err(unavailable)?,
        manual_probe_requested_at: row
            .try_get("manual_probe_requested_at")
            .map_err(unavailable)?,
        manual_override: row.try_get("manual_override").map_err(unavailable)?,
        attached_model: row.try_get("attached_model").map_err(unavailable)?,
        cookie_override_pod: row.try_get("cookie_override_pod").map_err(unavailable)?,
        cookie_override_issued_at: row
            .try_get("cookie_override_issued_at")
            .map_err(unavailable)?,
        cookie_override_name: row.try_get("cookie_override_name").map_err(unavailable)?,
        cookie_override_value: row.try_get("cookie_override_value").map_err(unavailable)?,
        cookie_override_cflb_name: row
            .try_get("cookie_override_cflb_name")
            .map_err(unavailable)?,
        cookie_override_cflb_value: row
            .try_get("cookie_override_cflb_value")
            .map_err(unavailable)?,
        cookie_override_expires_at: row
            .try_get("cookie_override_expires_at")
            .map_err(unavailable)?,
        cookie_override_observation_id: row
            .try_get("cookie_override_observation_id")
            .map_err(unavailable)?,
        config,
        routing_cookies: Vec::new(),
    })
}

impl PgProviderAccountRepository {
    pub(super) async fn copyable_turn_state(
        &self,
        account: &CoreProviderAccountId,
        model: &str,
        issued_at: i64,
        observation_id: Option<i64>,
    ) -> Result<Option<TurnStateToken>, CoreStoreError> {
        let row = sqlx::query("select s.* from account_turn_states s join provider_accounts a on a.id = s.account_id where s.account_id = $1 and s.model = $2 and a.provider_kind = 'openai' and a.authentication_kind = 'oauth' and s.upstream_account_id is not distinct from a.upstream_account_id and s.upstream_user_id is not distinct from a.upstream_user_id")
            .bind(account.as_str()).bind(model).fetch_optional(&self.pool).await.map_err(unavailable)?;
        let Some(state) = row.map(bucket).transpose()? else {
            return Ok(None);
        };
        let now = Utc::now().timestamp();
        let token = state
            .installed_token(now)
            .into_iter()
            .chain(state.candidate.as_ref().filter(|token| {
                token.is_fresh(now, state.config.ttl_seconds)
                    && TurnStateToken::parse(&token.value)
                        .is_some_and(|parsed| parsed.issued_at == token.issued_at)
            }))
            .find(|token| token.issued_at == issued_at);
        if observation_id.is_none() && token.is_some() {
            return Ok(token.cloned());
        }
        // 旧客户端未传观测 ID 时，仅允许没有歧义的历史正文。
        let mut values: Vec<String> = sqlx::query_scalar("select distinct e.detail->>'token' from account_turn_state_events e where e.account_id = $1 and e.model = $2 and e.event_kind = 'observation' and (e.detail->>'issuedAt')::bigint = $3 and ($4::bigint is null or e.id = $4) and e.detail->>'token' is not null limit 2")
            .bind(account.as_str()).bind(model).bind(issued_at).bind(observation_id)
            .fetch_all(&self.pool).await.map_err(unavailable)?;
        if values.len() != 1 {
            return Ok(None);
        }
        let value = values.remove(0);
        let token = TurnStateToken { value, issued_at };
        let parsed =
            TurnStateToken::parse(&token.value).is_some_and(|parsed| parsed.issued_at == issued_at);
        // 指定观测行时保留过期正文，刷新后仍可复制；未指定时只交出仍有效的票。
        Ok(
            (parsed && (observation_id.is_some() || token.is_fresh(now, state.config.ttl_seconds)))
                .then_some(token),
        )
    }

    pub(super) async fn load_turn_state_buckets_for_model(
        &self,
        accounts: &[CoreProviderAccountId],
        model: &str,
    ) -> Result<Vec<TurnStateBucket>, CoreStoreError> {
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
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
        .map_err(unavailable)?;
        self.with_routing_cookies(rows).await
    }

    pub(super) async fn load_turn_state_buckets(
        &self,
    ) -> Result<Vec<TurnStateBucket>, CoreStoreError> {
        self.clean_routing_cookies().await?;
        // 清理不依赖是否启用轮换，停用的桶也不能继续持有过期正文。
        // 有效期限 = 观测记录的到期点（若有）否则签发+TTL；mint 的近未来票按 mint_fresh 放宽签发侧。
        // installed_pair 按自己的联合到期独立存活：票失效后 pair 仍可驱动下一轮定向打票。
        sqlx::query("update account_turn_states set turn_state_override = case when current_issued_at > 0 and current_issued_at <= extract(epoch from now()) + case when current_expires_at is not null then 30 else 0 end and current_issued_at::numeric + (config->>'ttlSeconds')::numeric > extract(epoch from now()) and (current_expires_at is null or current_expires_at > extract(epoch from now())) then turn_state_override else null end, installed_pair = case when installed_pair is not null and (installed_pair->>'expiresAt')::numeric > extract(epoch from now()) then installed_pair else null end, current_expires_at = case when current_issued_at > 0 and current_issued_at <= extract(epoch from now()) + case when current_expires_at is not null then 30 else 0 end and current_issued_at::numeric + (config->>'ttlSeconds')::numeric > extract(epoch from now()) and (current_expires_at is null or current_expires_at > extract(epoch from now())) then current_expires_at else null end, attached_model = case when current_issued_at > 0 and current_issued_at <= extract(epoch from now()) + case when current_expires_at is not null then 30 else 0 end and current_issued_at::numeric + (config->>'ttlSeconds')::numeric > extract(epoch from now()) and (current_expires_at is null or current_expires_at > extract(epoch from now())) then attached_model else null end, candidate = case when candidate_issued_at > 0 and candidate_issued_at <= extract(epoch from now()) + case when candidate_expires_at is not null then 30 else 0 end and candidate_issued_at::numeric + (config->>'ttlSeconds')::numeric > extract(epoch from now()) and (candidate_expires_at is null or candidate_expires_at > extract(epoch from now())) then candidate else null end, candidate_pair = case when candidate_issued_at > 0 and candidate_issued_at <= extract(epoch from now()) + case when candidate_expires_at is not null then 30 else 0 end and candidate_issued_at::numeric + (config->>'ttlSeconds')::numeric > extract(epoch from now()) and (candidate_expires_at is null or candidate_expires_at > extract(epoch from now())) then candidate_pair else null end, candidate_expires_at = case when candidate_issued_at > 0 and candidate_issued_at <= extract(epoch from now()) + case when candidate_expires_at is not null then 30 else 0 end and candidate_issued_at::numeric + (config->>'ttlSeconds')::numeric > extract(epoch from now()) and (candidate_expires_at is null or candidate_expires_at > extract(epoch from now())) then candidate_expires_at else null end where (turn_state_override is not null and (current_issued_at > 0 and current_issued_at <= extract(epoch from now()) + case when current_expires_at is not null then 30 else 0 end and current_issued_at::numeric + (config->>'ttlSeconds')::numeric > extract(epoch from now()) and (current_expires_at is null or current_expires_at > extract(epoch from now()))) is not true) or (candidate is not null and (candidate_issued_at > 0 and candidate_issued_at <= extract(epoch from now()) + case when candidate_expires_at is not null then 30 else 0 end and candidate_issued_at::numeric + (config->>'ttlSeconds')::numeric > extract(epoch from now()) and (candidate_expires_at is null or candidate_expires_at > extract(epoch from now()))) is not true) or (installed_pair is not null and coalesce((installed_pair->>'expiresAt')::numeric, 0) <= extract(epoch from now()))")
            .execute(&self.pool).await.map_err(unavailable)?;
        // 已安装槽位过期后不再注入。观测记录里已生效过的正文留下；未签发或未来签发的正文摘掉。
        // mint 票允许 30 秒未来偏差，与 mint_fresh 的签发侧容差一致。
        sqlx::query("update account_turn_state_events e set detail = e.detail - 'token' where e.event_kind = 'observation' and e.detail ? 'token' and ((e.detail->>'issuedAt')::numeric > 0 and (e.detail->>'issuedAt')::numeric <= extract(epoch from now()) + 30) is not true")
            .execute(&self.pool).await.map_err(unavailable)?;
        let rows = sqlx::query("select * from account_turn_states order by account_id, model")
            .fetch_all(&self.pool)
            .await
            .map_err(unavailable)?;
        self.with_routing_cookies(rows).await
    }

    pub(super) async fn load_turn_state_bucket(
        &self,
        account: &CoreProviderAccountId,
        model: &str,
    ) -> Result<Option<TurnStateBucket>, CoreStoreError> {
        let row =
            sqlx::query("select * from account_turn_states where account_id = $1 and model = $2")
                .bind(account.as_str())
                .bind(model)
                .fetch_optional(&self.pool)
                .await
                .map_err(unavailable)?;
        Ok(self
            .with_routing_cookies(row.into_iter().collect())
            .await?
            .pop())
    }

    async fn with_routing_cookies(
        &self,
        rows: Vec<sqlx::postgres::PgRow>,
    ) -> Result<Vec<TurnStateBucket>, CoreStoreError> {
        let mut buckets: Vec<_> = rows.into_iter().map(bucket).collect::<Result<_, _>>()?;
        if buckets
            .iter()
            .any(|bucket| bucket.config.cookie_lock_enabled)
        {
            let cookies = self.load_routing_cookies().await?;
            for bucket in &mut buckets {
                if bucket.config.cookie_lock_enabled {
                    bucket.routing_cookies = cookies.clone();
                }
            }
        }
        Ok(buckets)
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
            "select s.*, a.enabled as account_enabled from account_turn_states s join provider_accounts a on a.id = s.account_id where s.account_id = $1 and s.model = $2 and s.upstream_account_id is not distinct from $3 and s.upstream_user_id is not distinct from $4 for update of s",
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
        if !row
            .try_get::<bool, _>("account_enabled")
            .map_err(unavailable)?
        {
            return Ok(());
        }
        let hunt_started_at: Option<i64> = row.try_get("hunt_started_at").map_err(unavailable)?;
        let mut state = bucket(row)?;
        // 按持久化时的配置再次验证，防止探测期间修改 TTL 后仍保存失效正文。
        // 云端 mint 的票带显式 expires_at，允许其近未来签发时间按 mint_fresh 通过。
        observation.token = observation.token.take().filter(|value| {
            TurnStateToken::parse(value).is_some_and(|token| {
                Some(token.issued_at) == observation.issued_at
                    && Some(value.len()) == observation.token_length
                    && token.live(
                        Utc::now().timestamp(),
                        state.config.ttl_seconds,
                        observation.expires_at,
                    )
            })
        });
        observation.observation_id = None;
        observation.has_token = false;
        observation.is_installed = false;
        if observation.outcome == "candidate"
            && candidate.as_ref().is_some_and(|token| {
                !token.is_newer_than(state.current_issued_at)
                    || !token.is_newer_than(state.candidate.as_ref().map(|old| old.issued_at))
            })
        {
            observation.outcome = "not_newer".to_owned();
        }
        let started_at = observation.started_at.unwrap_or(observation.observed_at);
        // 云端打票同样是消耗上游的主动尝试，计入 hunt 尝试数与耗时口径。
        let active_attempt = (observation.source == "probe" && observation.probe_id.is_some())
            || observation.source == "cloud_mint";
        if active_attempt {
            state.hunt_attempts = state.hunt_attempts.saturating_add(1);
            sqlx::query("update account_turn_states set hunt_attempts = hunt_attempts + 1, hunt_started_at = coalesce(hunt_started_at, $3) where account_id = $1 and model = $2")
                .bind(&observation.account_id).bind(&observation.model).bind(started_at)
                .execute(&mut *tx).await.map_err(unavailable)?;
        }
        observation.hunt_attempts = Some(if active_attempt {
            state.hunt_attempts
        } else {
            0
        });
        observation.hunt_seconds = Some(if active_attempt {
            observation
                .observed_at
                .saturating_sub(hunt_started_at.unwrap_or(started_at))
                .max(0) as u64
        } else {
            0
        });
        // 候选槽保持严格语义：只接收命中目标长度的票；非目标票的正文改由观测事件留存，
        // 不再占用候选与签发水位。安装门槛仍由 install_candidate 按 targetLength 校验。
        // 需要路由 pair 的模式必须带完整 pair，否则候选装好后也发不出去；pair 本身
        // 按写库时刻的最新配置完整验收（模型声明、路由有效期与网关白名单）。
        let now = Utc::now().timestamp();
        let pair = observation.pair.take().filter(|pair| {
            pair.is_usable(&state.model, now) && state.config.allows_cookie_gateway(&pair.pod)
        });
        let pair_json = pair
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(unavailable)?;
        if let Some(token) = candidate.filter(|token| {
            (!state.config.requires_route_pair() || pair.is_some())
                && TurnStateToken::parse(&token.value)
                    .is_some_and(|parsed| parsed.issued_at == token.issued_at)
                && token.value.len() == state.config.target_length
                // 新鲜度先按观测自带的 cap 判定：无 cap 的票走严格 is_fresh，
                // 极端签发时刻在任何死线算术之前出局，且旧票不借收窄凭空
                // 获得 mint 侧签发容差。
                && token.live(now, state.config.ttl_seconds, observation.expires_at)
                && token.is_newer_than(state.current_issued_at)
                && token.is_newer_than(state.candidate.as_ref().map(|old| old.issued_at))
        }) {
            // 死线收窄只取观测上限与绑定 pair 到期的较小者，不做签发时刻加法；
            // issued+TTL 已由 live 判定覆盖，收窄后仍有效的票才落候选槽。
            let expires_at = [
                observation.expires_at,
                pair.as_ref().map(|pair| pair.expires_at),
            ]
            .into_iter()
            .flatten()
            .min();
            if token.live(now, state.config.ttl_seconds, expires_at) {
                observation.expires_at = expires_at;
                let (attempts, hunt_started_at) = if active_attempt {
                    (
                        state.hunt_attempts as i64,
                        hunt_started_at.unwrap_or(started_at),
                    )
                } else {
                    (0, observation.observed_at)
                };
                sqlx::query("update account_turn_states set candidate = $3, candidate_issued_at = $4, candidate_source = $5, candidate_observed_at = $6, candidate_attempts = $7, candidate_hunt_started_at = $8, candidate_pair = $9, candidate_expires_at = $10 where account_id = $1 and model = $2")
                    .bind(&observation.account_id).bind(&observation.model).bind(&token.value).bind(token.issued_at).bind(&observation.source)
                    .bind(observation.observed_at).bind(attempts).bind(hunt_started_at)
                    .bind(&pair_json).bind(observation.expires_at)
                    .execute(&mut *tx).await.map_err(unavailable)?;
            }
        }
        // 停止后台日志不丢弃有效候选；手动发起的探测仍保留可操作结果。
        if state.config.enabled
            || state.config.cookie_lock_enabled
            || observation.probe_trigger.as_deref() == Some("manual")
        {
            append_event(
                &mut tx,
                &observation.account_id,
                &observation.model,
                "observation",
                serde_json::to_value(&observation).map_err(unavailable)?,
            )
            .await?;
        }
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

    /// 上报模型与请求模型不一致时作废已发出去的票及其绑定 pair；
    /// 若回放的正是被固定的 pair，则连同固定值一起清掉。
    pub(super) async fn note_installed_model(
        &self,
        account: &CoreProviderAccountId,
        model: &str,
        sent_state: &str,
        reported_model: &str,
        revoke_on_change: bool,
        sent_cookie: Option<&RoutingCookie>,
    ) -> Result<bool, CoreStoreError> {
        if reported_model.is_empty() || reported_model.len() > 256 {
            return Ok(false);
        }
        // 不匹配只发生在“已发出的票”上：WHERE 里的 sent_state 保证只动本次安装的正文。
        let revoked = sqlx::query_scalar::<_, bool>("update account_turn_states set turn_state_override = case when $5 and lower($4) <> lower($2) then null else turn_state_override end, installed_pair = case when $5 and lower($4) <> lower($2) then null else installed_pair end, current_expires_at = case when $5 and lower($4) <> lower($2) then null else current_expires_at end, manual_override = case when $5 and lower($4) <> lower($2) then false else manual_override end, next_probe_at = case when $5 and lower($4) <> lower($2) then null else next_probe_at end, attached_model = case when $5 and lower($4) <> lower($2) then null else $4 end, cookie_override_pod = case when $5 and lower($4) <> lower($2) and cookie_override_name = $6 and cookie_override_value = $7 and ($8::text is null or cookie_override_cflb_value = $8) then null else cookie_override_pod end, cookie_override_issued_at = case when $5 and lower($4) <> lower($2) and cookie_override_name = $6 and cookie_override_value = $7 and ($8::text is null or cookie_override_cflb_value = $8) then null else cookie_override_issued_at end, cookie_override_name = case when $5 and lower($4) <> lower($2) and cookie_override_name = $6 and cookie_override_value = $7 and ($8::text is null or cookie_override_cflb_value = $8) then null else cookie_override_name end, cookie_override_value = case when $5 and lower($4) <> lower($2) and cookie_override_name = $6 and cookie_override_value = $7 and ($8::text is null or cookie_override_cflb_value = $8) then null else cookie_override_value end, cookie_override_cflb_name = case when $5 and lower($4) <> lower($2) and cookie_override_name = $6 and cookie_override_value = $7 and ($8::text is null or cookie_override_cflb_value = $8) then null else cookie_override_cflb_name end, cookie_override_cflb_value = case when $5 and lower($4) <> lower($2) and cookie_override_name = $6 and cookie_override_value = $7 and ($8::text is null or cookie_override_cflb_value = $8) then null else cookie_override_cflb_value end, cookie_override_expires_at = case when $5 and lower($4) <> lower($2) and cookie_override_name = $6 and cookie_override_value = $7 and ($8::text is null or cookie_override_cflb_value = $8) then null else cookie_override_expires_at end, cookie_override_observation_id = case when $5 and lower($4) <> lower($2) and cookie_override_name = $6 and cookie_override_value = $7 and ($8::text is null or cookie_override_cflb_value = $8) then null else cookie_override_observation_id end where account_id = $1 and model = $2 and turn_state_override = $3 returning turn_state_override is null")
            .bind(account.as_str()).bind(model).bind(sent_state).bind(reported_model).bind(revoke_on_change)
            .bind(sent_cookie.map(|cookie| cookie.name.as_str()))
            .bind(sent_cookie.map(|cookie| cookie.value.as_str()))
            .bind(sent_cookie.map(|cookie| cookie.cflb_value.as_str()))
            .fetch_optional(&self.pool).await.map_err(unavailable)?;
        Ok(revoked.unwrap_or(false))
    }

    pub(super) async fn turn_state_statuses(
        &self,
        account_id: Option<&str>,
    ) -> Result<Vec<TurnStateStatus>, CoreStoreError> {
        let rows = sqlx::query("select s.*, a.name as account_name, a.email as account_email, a.enabled as account_enabled, (a.authentication_kind = 'oauth' and s.upstream_account_id is not distinct from a.upstream_account_id and s.upstream_user_id is not distinct from a.upstream_user_id) as identity_matches, coalesce((select jsonb_agg((e.detail - 'token' - 'cookieValue' - 'cookieCflbValue') || jsonb_build_object('observationId', e.id::text, 'hasToken', e.detail->>'token' is not null, 'hasCookie', e.detail->>'cookieValue' is not null, 'isInstalled', coalesce(e.detail->>'token' = s.turn_state_override, false)) order by e.id desc) from account_turn_state_events e where e.account_id = s.account_id and e.model = s.model and e.event_kind = 'observation'), '[]'::jsonb) as observations, coalesce((select jsonb_agg(e.detail order by e.id desc) from account_turn_state_events e where e.account_id = s.account_id and e.model = s.model and e.event_kind = 'installation'), '[]'::jsonb) as installations from account_turn_states s join provider_accounts a on a.id = s.account_id where ($1::text is null or s.account_id = $1) order by s.account_id, s.model")
            .bind(account_id).fetch_all(&self.pool).await.map_err(unavailable)?;
        rows.into_iter()
            .map(|row| {
                let account_name = row.try_get("account_name").map_err(unavailable)?;
                let account_email = row.try_get("account_email").map_err(unavailable)?;
                let account_enabled: bool = row.try_get("account_enabled").map_err(unavailable)?;
                let identity_matches: bool =
                    row.try_get("identity_matches").map_err(unavailable)?;
                let mut observations: Vec<TurnStateObservation> =
                    serde_json::from_value(row.try_get("observations").map_err(unavailable)?)
                        .map_err(unavailable)?;
                let installations =
                    serde_json::from_value(row.try_get("installations").map_err(unavailable)?)
                        .map_err(unavailable)?;
                let state = bucket(row)?;
                let installed = state
                    .installed_token(Utc::now().timestamp())
                    .filter(|_| identity_matches);
                let routing_cookie = state
                    .config
                    .cookie_lock_enabled
                    .then(|| state.routing_cookie(Utc::now().timestamp()))
                    .flatten()
                    .filter(|_| identity_matches);
                let active = account_enabled
                    && if state.config.cookie_lock_enabled {
                        routing_cookie.is_some()
                    } else {
                        installed.is_some()
                    };
                let has_installed_state = installed.is_some();
                for observation in &mut observations {
                    observation.is_installed &= has_installed_state;
                    // 正文还在就可以复制；是否还能安装由有效期单独判断。
                    observation.has_token &= identity_matches;
                }
                let business_status = if !account_enabled {
                    TurnStateBusinessStatus::ManualDisabled
                } else if state.config.missing_state_policy == MissingTurnStatePolicy::Pause
                    && !active
                {
                    TurnStateBusinessStatus::WaitingForState
                } else {
                    TurnStateBusinessStatus::Ready
                };
                let next_probe_at = if !account_enabled
                    || (!state.config.enabled && !state.config.cookie_lock_enabled)
                {
                    None
                } else if state.config.cookie_lock_enabled {
                    Some(state.next_probe_at.unwrap_or_else(|| {
                        routing_cookie.as_ref().map_or_else(
                            || Utc::now().timestamp(),
                            |cookie| {
                                cookie.expires_at
                                    - state.config.cookie_refresh_before_seconds as i64
                            },
                        )
                    }))
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
                let routing_cookie = routing_cookie.map(|cookie| cookie.status());
                Ok(TurnStateStatus {
                    routing_cookie,
                    cookie_pool: Vec::new(),
                    cookie_override_pod: state.cookie_override_pod,
                    cookie_override_issued_at: state.cookie_override_issued_at,
                    cookie_override_name: state.cookie_override_name,
                    // 常规状态不下发 Cookie 正文；复制/查看走独立的 cookie-copy 入口。
                    cookie_override_value: None,
                    cookie_override_cflb_name: state.cookie_override_cflb_name,
                    cookie_override_cflb_value: None,
                    cookie_override_expires_at: state.cookie_override_expires_at,
                    cookie_override_observation_id: state
                        .cookie_override_observation_id
                        .map(|id| id.to_string()),
                    account_id: state.account_id,
                    account_name,
                    account_email,
                    model: state.model,
                    active,
                    has_installed_state,
                    has_installed_pair: state
                        .installed_pair
                        .as_ref()
                        .is_some_and(|pair| pair.is_route_valid(Utc::now().timestamp())),
                    installed_gateway_id: state
                        .installed_pair
                        .as_ref()
                        .filter(|pair| pair.is_route_valid(Utc::now().timestamp()))
                        .map(|pair| pair.gateway_label()),
                    current_expires_at: state.current_expires_at,
                    candidate_expires_at: state.candidate_expires_at,
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
    // mint 票带 expires_at 时签发侧允许 30 秒偏差，到期点取 签发+TTL 与记录上限的较小者。
    // 需要路由 pair 的模式要求候选绑定完整且当前仍有效的 pair；非绑定模式下
    // 失效/半残的候选 pair 不落安装位，避免把可用票据和坏凭据绑在一起。
    let pair_valid = "candidate_pair is not null and coalesce(candidate_pair->>'cflbValue', '') <> '' and (candidate_pair->>'expiresAt')::numeric > extract(epoch from now()) and (candidate_pair->>'issuedAt')::numeric <= extract(epoch from now()) and candidate_pair->>'pod' ~ '^chat\\.gateway\\.unified-[0-9]+\\.api\\.openai\\.com$' and (coalesce(config->>'cookieGatewayIds', '') = '' or substring(candidate_pair->>'pod' from 'unified-([0-9]+)\\.') = any(string_to_array(config->>'cookieGatewayIds', '|'))) and lower(coalesce(candidate_pair->>'reportedModel', '')) = lower(s.model)";
    let pair_free = "coalesce(config->>'stopStrategy', 'headers') <> 'declared_model' and jsonb_array_length(coalesce(config->'cloudMints', '[]'::jsonb)) = 0";
    let sql = format!(
        "update account_turn_states s set attached_model = (select nullif(e.detail->>'reportedModel', '') from account_turn_state_events e where e.account_id = s.account_id and e.model = s.model and e.event_kind = 'observation' and e.detail->>'token' = s.candidate order by e.id desc limit 1), turn_state_override = candidate, current_issued_at = candidate_issued_at, current_length = length(candidate), current_expires_at = (select min(cap) from (values (candidate_expires_at), (case when {pair_valid} then (candidate_pair->>'expiresAt')::bigint end)) caps(cap)), installed_pair = case when {pair_valid} then candidate_pair else null end, manual_override = ($3::bigint is not null), candidate = null, candidate_pair = null, candidate_expires_at = null, hunt_attempts = 0, hunt_started_at = null, next_probe_at = candidate_issued_at + (config->>'refreshAfterSeconds')::bigint where account_id = $1 and model = $2 and (($3::bigint is null and (config->>'enabled')::boolean) or candidate_issued_at = $3) and candidate is not null and length(candidate) = (config->>'targetLength')::integer and candidate_issued_at <= extract(epoch from now()) + case when candidate_expires_at is not null then 30 else 0 end and candidate_issued_at + (config->>'ttlSeconds')::bigint > extract(epoch from now()) and (candidate_expires_at is null or candidate_expires_at > extract(epoch from now())) and ({pair_valid} or {pair_free}) and (current_issued_at is null or candidate_issued_at > current_issued_at) and exists (select 1 from provider_accounts a where a.id = s.account_id and a.enabled and a.provider_kind = 'openai' and a.authentication_kind = 'oauth' and a.upstream_account_id is not distinct from s.upstream_account_id and a.upstream_user_id is not distinct from s.upstream_user_id) returning current_issued_at, current_length, candidate_source, candidate_observed_at, candidate_attempts, candidate_hunt_started_at"
    );
    // 只拼接固定 SQL 谓词；账号、模型及签发时间仍通过 bind 传入。
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(account.as_str())
        .bind(model)
        .bind(expected_issued_at)
        .fetch_optional(&mut **tx)
        .await
        .map_err(unavailable)?;
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
    super::turn_state_notifications::enqueue(tx, account, model, &installation).await?;
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

/// 通过观测 ID 精确选择历史正文；旧调用只允许没有歧义的同秒历史。
/// 门槛与候选安装一致：目标长度、有效期内、签发时间严格更新、身份匹配且账号启用。
pub(super) async fn install_observed_turn_state(
    tx: &mut Transaction<'_, Postgres>,
    account: &CoreProviderAccountId,
    model: &str,
    issued_at: i64,
    observation_id: Option<i64>,
) -> Result<bool, CoreStoreError> {
    // 先锁桶行串行化并发安装，再取事件正文；两段查询比锁 JOIN 行更直观。
    let state = sqlx::query("select s.config, s.current_issued_at from account_turn_states s join provider_accounts a on a.id = s.account_id where s.account_id = $1 and s.model = $2 and a.enabled and a.provider_kind = 'openai' and a.authentication_kind = 'oauth' and s.upstream_account_id is not distinct from a.upstream_account_id and s.upstream_user_id is not distinct from a.upstream_user_id for update of s")
        .bind(account.as_str()).bind(model)
        .fetch_optional(&mut **tx).await.map_err(unavailable)?;
    let Some(state) = state else {
        return Ok(false);
    };
    // 成功结论白名单只对声明模型/云端这类 pair 绑定观测生效；其它模式的
    // 历史正文沿用“非目标长度也可按观测套用”的旧合同，配置改长度后仍可用。
    let rows = sqlx::query("select distinct on (e.detail->>'token') e.detail->>'token' as value, (e.detail->>'observedAt')::bigint as observed_at, e.detail->>'source' as source, e.detail->>'outcome' as outcome, (e.detail->>'huntAttempts')::bigint as hunt_attempts, (e.detail->>'huntSeconds')::bigint as hunt_seconds, (e.detail->>'expiresAt')::bigint as expires_at, e.detail->>'cookieName' as cookie_name, e.detail->>'cookieValue' as cookie_value, e.detail->>'cookieCflbName' as cflb_name, e.detail->>'cookieCflbValue' as cflb_value, e.detail->>'oailbHost' as pod, e.detail->>'cookieOrigin' as cookie_origin, (e.detail->>'cookieIssuedAt')::bigint as cookie_issued_at, (e.detail->>'cookieExpiresAt')::bigint as cookie_expires_at, coalesce(e.detail->>'reportedModel', '') as reported_model from account_turn_state_events e where e.account_id = $1 and e.model = $2 and e.event_kind = 'observation' and (e.detail->>'issuedAt')::bigint = $3 and ($4::bigint is null or e.id = $4) and e.detail->>'token' is not null order by e.detail->>'token', e.id limit 2")
        .bind(account.as_str()).bind(model).bind(issued_at).bind(observation_id)
        .fetch_all(&mut **tx).await.map_err(unavailable)?;
    if rows.len() != 1 {
        return Ok(false);
    }
    let row = &rows[0];
    let config: TurnStateConfig =
        serde_json::from_value(state.try_get("config").map_err(unavailable)?)
            .map_err(unavailable)?;
    let now = Utc::now().timestamp();
    let observed_at: i64 = row.try_get("observed_at").map_err(unavailable)?;
    let source: Option<String> = row.try_get("source").map_err(unavailable)?;
    let outcome: Option<String> = row.try_get("outcome").map_err(unavailable)?;
    // 失败声明（错模型/坏创建事件/残缺 pair）即使带着票也不能套用；
    // 只约束 pair 绑定来源的观测，其它来源不引入新的历史回放门槛。
    if (config.requires_route_pair() || source.as_deref() == Some("cloud_mint"))
        && !matches!(
            outcome.as_deref(),
            Some("candidate" | "cookie_ready" | "pair_ready")
        )
    {
        return Ok(false);
    }
    // 需要路由 pair 的模式从同一条观测重建 pair，并按最新配置做完整验收；
    // 重建不出仍有效 pair 的历史正文不能套用，也不会恢复签发+TTL 的旧寿命。
    let pair = if config.requires_route_pair() {
        let name: Option<String> = row.try_get("cookie_name").map_err(unavailable)?;
        let value: Option<String> = row.try_get("cookie_value").map_err(unavailable)?;
        let cflb_name: Option<String> = row.try_get("cflb_name").map_err(unavailable)?;
        let cflb_value: Option<String> = row.try_get("cflb_value").map_err(unavailable)?;
        let pod: Option<String> = row.try_get("pod").map_err(unavailable)?;
        let origin: Option<String> = row.try_get("cookie_origin").map_err(unavailable)?;
        let cookie_expires_at: Option<i64> =
            row.try_get("cookie_expires_at").map_err(unavailable)?;
        let reported_model: String = row.try_get("reported_model").map_err(unavailable)?;
        let (Some(name), Some(value), Some(cflb_name), Some(cflb_value), Some(pod)) =
            (name, value, cflb_name, cflb_value, pod)
        else {
            return Ok(false);
        };
        // JWT 必须真重解析：伪造的元数据行不能绕过内嵌 pod/iat/exp 校验；
        // __cflb 半边按记录的联合到期重配，不延长 JWT 死线。
        let origin = origin.unwrap_or_default();
        let Some(mut pair) = RoutingCookie::parse(&origin, &name, &value, now)
            .and_then(|parsed| parsed.with_cflb(&cflb_name, &cflb_value, cookie_expires_at))
        else {
            return Ok(false);
        };
        if pair.pod != pod {
            return Ok(false);
        }
        pair.observed_at = observed_at.saturating_mul(1000);
        pair.reported_model = reported_model;
        if !pair.is_usable(model, now) || !config.allows_cookie_gateway(&pair.pod) {
            return Ok(false);
        }
        Some(pair)
    } else {
        None
    };
    let pair_json = pair
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(unavailable)?;
    // 死线同样只取观测上限与重建 pair 到期的较小者：不做签发时刻加法
    // （issued_at 来自请求参数，极端值不得参与算术），issued+TTL 由 live 覆盖；
    // mint 票沿用观测记下的上限，不能让历史套用重新获得签发+TTL 寿命。
    let expires_at = [
        row.try_get::<Option<i64>, _>("expires_at")
            .map_err(unavailable)?,
        pair.as_ref().map(|pair| pair.expires_at),
    ]
    .into_iter()
    .flatten()
    .min();
    let current_issued_at: Option<i64> = state.try_get("current_issued_at").map_err(unavailable)?;
    let token = TurnStateToken {
        value: row.try_get("value").map_err(unavailable)?,
        issued_at,
    };
    if !TurnStateToken::parse(&token.value).is_some_and(|parsed| parsed.issued_at == issued_at)
        || token.value.len() != config.target_length
        || !token.live(now, config.ttl_seconds, expires_at)
        || !token.is_newer_than(current_issued_at)
    {
        return Ok(false);
    }
    sqlx::query("update account_turn_states set attached_model = (select nullif(e.detail->>'reportedModel', '') from account_turn_state_events e where e.account_id = account_turn_states.account_id and e.model = account_turn_states.model and e.event_kind = 'observation' and e.detail->>'token' = $3 order by e.id desc limit 1), turn_state_override = $3, current_issued_at = $4, current_length = length($3), current_expires_at = $5, installed_pair = $6, manual_override = true, candidate = case when candidate_issued_at > $4 then candidate else null end, candidate_pair = case when candidate_issued_at > $4 then candidate_pair else null end, candidate_expires_at = case when candidate_issued_at > $4 then candidate_expires_at else null end, hunt_attempts = 0, hunt_started_at = null, next_probe_at = $4 + (config->>'refreshAfterSeconds')::bigint where account_id = $1 and model = $2")
        .bind(account.as_str()).bind(model).bind(&token.value).bind(issued_at).bind(expires_at).bind(&pair_json)
        .execute(&mut **tx).await.map_err(unavailable)?;
    let installation = TurnStateInstallation {
        installed_at: Utc::now().timestamp(),
        issued_at,
        token_length: token.value.len(),
        source: row
            .try_get::<Option<String>, _>("source")
            .map_err(unavailable)?
            .unwrap_or_else(|| "passive".to_owned()),
        acquired_at: row.try_get("observed_at").map_err(unavailable)?,
        attempts: row
            .try_get::<Option<i64>, _>("hunt_attempts")
            .map_err(unavailable)?
            .unwrap_or(0)
            .max(0) as u64,
        hunt_seconds: row
            .try_get::<Option<i64>, _>("hunt_seconds")
            .map_err(unavailable)?
            .unwrap_or(0)
            .max(0) as u64,
    };
    super::turn_state_notifications::enqueue(tx, account, model, &installation).await?;
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
    sqlx::query("insert into account_turn_state_events(account_id, model, event_kind, detail) values ($1, $2, $3, $4)")
        .bind(account).bind(model).bind(kind).bind(&detail).execute(&mut **tx).await.map_err(unavailable)?;
    // 探测与被动采集的 state 记录全部保留。安装历史仍只留最近 100 条。
    if kind != "observation" {
        sqlx::query("delete from account_turn_state_events where account_id = $1 and model = $2 and event_kind = $3 and id not in (select id from account_turn_state_events where account_id = $1 and model = $2 and event_kind = $3 order by id desc limit 100)")
            .bind(account).bind(model).bind(kind).execute(&mut **tx).await.map_err(unavailable)?;
    }
    Ok(())
}
