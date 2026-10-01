//! 共享路由池的持久化；只比较并存储 Provider 已解析的事实。

use super::*;
use gateway_core::account::{RoutingCookie, RoutingCookieObservation};

fn unavailable(_: impl std::fmt::Debug) -> CoreStoreError {
    CoreStoreError::new(CoreStoreErrorKind::Unavailable)
}

impl PgProviderAccountRepository {
    pub(super) async fn clean_routing_cookies(&self) -> Result<(), CoreStoreError> {
        sqlx::query("update openai_routing_cookies set value = null where expires_at <= extract(epoch from now()) and value is not null").execute(&self.pool).await.map_err(unavailable)?;
        sqlx::query("delete from openai_routing_cookies where expires_at < extract(epoch from now()) - 3600").execute(&self.pool).await.map_err(unavailable)?;
        Ok(())
    }

    fn pool_cookie(row: sqlx::postgres::PgRow) -> Result<RoutingCookie, CoreStoreError> {
        let origin: String = row.try_get("origin").map_err(unavailable)?;
        let name: String = row.try_get("name").map_err(unavailable)?;
        let value: String = row.try_get("value").map_err(unavailable)?;
        let now = Utc::now().timestamp();
        Ok(RoutingCookie {
            // JWT 声明的到期不单独存列，装载时从正文重新解析回来。
            oailb_expires_at: RoutingCookie::parse(&origin, &name, &value, now)
                .and_then(|parsed| parsed.oailb_expires_at),
            pod: row.try_get("pod").map_err(unavailable)?,
            issued_at: row.try_get("issued_at").map_err(unavailable)?,
            expires_at: row.try_get("expires_at").map_err(unavailable)?,
            observed_at: row.try_get("observed_at").map_err(unavailable)?,
            reported_model: row.try_get("reported_model").map_err(unavailable)?,
            cflb_name: row
                .try_get::<Option<String>, _>("cflb_name")
                .map_err(unavailable)?
                .unwrap_or_default(),
            cflb_value: row
                .try_get::<Option<String>, _>("cflb_value")
                .map_err(unavailable)?
                .unwrap_or_default(),
            cflb_expires_at: row.try_get("cflb_expires_at").map_err(unavailable)?,
            origin,
            name,
            value,
        })
    }

    pub(super) async fn load_routing_cookies(&self) -> Result<Vec<RoutingCookie>, CoreStoreError> {
        // 旧行可能只有半边 Cookie，缺 cflb 的记录不可回放，过滤在 SQL 层完成。
        let rows = sqlx::query("select * from openai_routing_cookies where value is not null and cflb_value is not null and expires_at > extract(epoch from now()) order by observed_at desc limit 256").fetch_all(&self.pool).await.map_err(unavailable)?;
        rows.into_iter().map(Self::pool_cookie).collect()
    }

    /// 账号凭据中尚未过期、且不是路由票的 Cookie。值为请求头里的原文。
    pub(super) async fn account_replay_cookies(
        &self,
        account_id: &str,
    ) -> Result<Vec<(String, String)>, CoreStoreError> {
        let rows = sqlx::query(
            "select c->>'name' as name, c->>'value' as value, c->>'expires_at' as expires_at from provider_accounts a cross join lateral jsonb_array_elements(coalesce(a.provider_credentials_json->'cookies', '[]'::jsonb)) c where a.id = $1",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;
        let now = Utc::now();
        let mut cookies = Vec::new();
        for row in rows {
            let name: String = row.try_get("name").map_err(unavailable)?;
            let value: String = row.try_get("value").map_err(unavailable)?;
            let expires_at: Option<String> = row.try_get("expires_at").map_err(unavailable)?;
            if matches!(name.as_str(), "__oailb" | "__oai_lb")
                || name.is_empty()
                || value.is_empty()
                || value.contains(';')
                || value.chars().any(char::is_control)
                || name.chars().any(char::is_control)
            {
                continue;
            }
            if expires_at.as_deref().is_some_and(|expires| {
                chrono::DateTime::parse_from_rfc3339(expires)
                    .is_ok_and(|expires| expires.timestamp() < now.timestamp())
            }) {
                continue;
            }
            cookies.push((name, value));
        }
        Ok(cookies)
    }

    /// 读取某一条探测记录上保存的路由 Cookie。旧记录没有正文时返回空。
    pub(super) async fn recorded_routing_cookie(
        &self,
        account_id: &str,
        model: &str,
        observation_id: i64,
    ) -> Result<Option<RoutingCookie>, CoreStoreError> {
        let row = sqlx::query("select detail->>'cookieName' as name, detail->>'cookieValue' as value, detail->>'cookieCflbName' as cflb_name, detail->>'cookieCflbValue' as cflb_value, detail->>'oailbHost' as pod, detail->>'cookieOrigin' as origin, (detail->>'cookieIssuedAt')::bigint as issued_at, (detail->>'cookieExpiresAt')::bigint as expires_at, coalesce(detail->>'reportedModel', '') as reported_model from account_turn_state_events where id = $1 and account_id = $2 and model = $3 and event_kind = 'observation'")
            .bind(observation_id)
            .bind(account_id)
            .bind(model)
            .fetch_optional(&self.pool)
            .await
            .map_err(unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let name: Option<String> = row.try_get("name").map_err(unavailable)?;
        let value: Option<String> = row.try_get("value").map_err(unavailable)?;
        let cflb_name: Option<String> = row.try_get("cflb_name").map_err(unavailable)?;
        let cflb_value: Option<String> = row.try_get("cflb_value").map_err(unavailable)?;
        let pod: Option<String> = row.try_get("pod").map_err(unavailable)?;
        let origin: Option<String> = row.try_get("origin").map_err(unavailable)?;
        let (Some(name), Some(value), Some(pod)) = (name, value, pod) else {
            return Ok(None);
        };
        // 旧记录没有 cflb 半边，固定后也无法回放，直接视为无记录。
        let (Some(cflb_name), Some(cflb_value)) = (cflb_name, cflb_value) else {
            return Ok(None);
        };
        if name.is_empty()
            || value.is_empty()
            || cflb_name.is_empty()
            || cflb_value.is_empty()
            || pod.is_empty()
        {
            return Ok(None);
        }
        let issued_at: Option<i64> = row.try_get("issued_at").map_err(unavailable)?;
        let expires_at: Option<i64> = row.try_get("expires_at").map_err(unavailable)?;
        let reported_model: String = row.try_get("reported_model").map_err(unavailable)?;
        let origin = origin.unwrap_or_default();
        Ok(Some(RoutingCookie {
            oailb_expires_at: RoutingCookie::parse(&origin, &name, &value, Utc::now().timestamp())
                .and_then(|parsed| parsed.oailb_expires_at),
            cflb_expires_at: None,
            pod,
            issued_at: issued_at.unwrap_or(0),
            expires_at: expires_at.unwrap_or(0),
            observed_at: 0,
            reported_model,
            origin,
            name,
            value,
            cflb_name,
            cflb_value,
        }))
    }

    /// 把探测记录上的那张 Cookie 写回池，供这次人工固定后的请求回放。
    pub(super) async fn restore_routing_cookie(
        &self,
        cookie: &RoutingCookie,
    ) -> Result<(), CoreStoreError> {
        let origin = if !cookie.origin.is_empty() {
            cookie.origin.clone()
        } else {
            let stored: Option<String> = sqlx::query_scalar("select origin from openai_routing_cookies where pod = $1 or true order by observed_at desc limit 1")
                .bind(&cookie.pod)
                .fetch_optional(&self.pool)
                .await
                .map_err(unavailable)?;
            let Some(origin) = stored.filter(|origin| !origin.is_empty()) else {
                return Ok(());
            };
            origin
        };
        let now = Utc::now().timestamp_millis();
        sqlx::query("insert into openai_routing_cookies(origin, pod, name, value, cflb_name, cflb_value, cflb_expires_at, issued_at, expires_at, observed_at, reported_model) values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) on conflict (origin, pod) do update set name = excluded.name, value = excluded.value, cflb_name = excluded.cflb_name, cflb_value = excluded.cflb_value, cflb_expires_at = excluded.cflb_expires_at, issued_at = excluded.issued_at, expires_at = excluded.expires_at, observed_at = excluded.observed_at, reported_model = excluded.reported_model")
            .bind(origin)
            .bind(&cookie.pod)
            .bind(&cookie.name)
            .bind(&cookie.value)
            .bind(&cookie.cflb_name)
            .bind(&cookie.cflb_value)
            .bind(cookie.cflb_expires_at)
            .bind(cookie.issued_at)
            .bind(cookie.expires_at)
            .bind(now)
            .bind(&cookie.reported_model)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// 管理员按需复制仍有效的 Cookie 正文；状态轮询不携带值。
    pub(super) async fn routing_cookie_value(
        &self,
        pod: &str,
    ) -> Result<Option<RoutingCookie>, CoreStoreError> {
        let row = sqlx::query("select * from openai_routing_cookies where pod = $1 and value is not null and cflb_value is not null and expires_at > extract(epoch from now()) order by observed_at desc limit 1")
            .bind(pod)
            .fetch_optional(&self.pool)
            .await
            .map_err(unavailable)?;
        row.map(Self::pool_cookie).transpose()
    }

    pub(super) async fn record_routing_cookie(
        &self,
        observation: RoutingCookieObservation,
    ) -> Result<(), CoreStoreError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        // 跨账号响应和换 pod 同时到达时，按请求开始水位串行更新。
        sqlx::query("select pg_advisory_xact_lock(73192422)")
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        if let Some(sent) = &observation.sent
            && (observation.deleted
                || observation
                    .received
                    .as_ref()
                    .is_some_and(|cookie| cookie.pod != sent.pod))
        {
            // 精确比对 pair 全字段：同 pod 上更新的凭据不能被旧响应水位误删。
            sqlx::query("update openai_routing_cookies set value = null, cflb_name = null, cflb_value = null, cflb_expires_at = null, reported_model = '', observed_at = $3 where origin = $1 and pod = $2 and observed_at <= $3 and name is not distinct from $4 and value is not distinct from $5 and cflb_name is not distinct from $6 and cflb_value is not distinct from $7")
                .bind(&observation.origin).bind(&sent.pod).bind(observation.observed_at).bind(&sent.name).bind(&sent.value).bind(&sent.cflb_name).bind(&sent.cflb_value).execute(&mut *tx).await.map_err(unavailable)?;
            // 同一事务里解绑仍指向这把已失效 pair 的桶：谓词在 UPDATE 的目标行上就地
            // 求值（非先读后写的快照标志），并发提交经 EPQ 按最新行版本复核，
            // 旧响应不会误删并发安装的更新绑定。三段谓词都按 origin+pod+两半名字与值
            // 精确匹配，且只清理不晚于本次请求开始水位记录的绑定（installed/candidate
            // 的 observedAt 是毫秒，固定位 issued_at 是秒）。票据正文随绑定摘除但保留
            // 签发水位列；next_probe_at 置空让下一轮尽快补打。
            let installed = "installed_pair->>'name' is not distinct from $4 and installed_pair->>'value' is not distinct from $5 and installed_pair->>'cflbName' is not distinct from $6 and installed_pair->>'cflbValue' is not distinct from $7 and installed_pair->>'pod' = $2 and coalesce(nullif(installed_pair->>'origin',''), $1) = $1 and coalesce((installed_pair->>'observedAt')::numeric, 0) <= $3";
            let candidate = "candidate_pair->>'name' is not distinct from $4 and candidate_pair->>'value' is not distinct from $5 and candidate_pair->>'cflbName' is not distinct from $6 and candidate_pair->>'cflbValue' is not distinct from $7 and candidate_pair->>'pod' = $2 and coalesce(nullif(candidate_pair->>'origin',''), $1) = $1 and coalesce((candidate_pair->>'observedAt')::numeric, 0) <= $3";
            let pinned = "cookie_override_pod = $2 and cookie_override_name is not distinct from $4 and cookie_override_value is not distinct from $5 and cookie_override_cflb_name is not distinct from $6 and cookie_override_cflb_value is not distinct from $7 and (cookie_override_issued_at is null or cookie_override_issued_at * 1000 <= $3)";
            let sql = format!(
                "update account_turn_states set turn_state_override = case when {installed} then null else turn_state_override end, installed_pair = case when {installed} then null else installed_pair end, current_expires_at = case when {installed} then null else current_expires_at end, attached_model = case when {installed} then null else attached_model end, manual_override = case when {installed} then false else manual_override end, next_probe_at = case when {installed} then null else next_probe_at end, candidate = case when {candidate} then null else candidate end, candidate_pair = case when {candidate} then null else candidate_pair end, candidate_expires_at = case when {candidate} then null else candidate_expires_at end, cookie_override_pod = case when {pinned} then null else cookie_override_pod end, cookie_override_issued_at = case when {pinned} then null else cookie_override_issued_at end, cookie_override_name = case when {pinned} then null else cookie_override_name end, cookie_override_value = case when {pinned} then null else cookie_override_value end, cookie_override_cflb_name = case when {pinned} then null else cookie_override_cflb_name end, cookie_override_cflb_value = case when {pinned} then null else cookie_override_cflb_value end, cookie_override_expires_at = case when {pinned} then null else cookie_override_expires_at end, cookie_override_observation_id = case when {pinned} then null else cookie_override_observation_id end where ({installed}) or ({candidate}) or ({pinned})"
            );
            // 只拼接固定 SQL 谓词；origin、pod、水位及 Cookie 正文仍通过 bind 传入。
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(&observation.origin)
                .bind(&sent.pod)
                .bind(observation.observed_at)
                .bind(&sent.name)
                .bind(&sent.value)
                .bind(&sent.cflb_name)
                .bind(&sent.cflb_value)
                .execute(&mut *tx)
                .await
                .map_err(unavailable)?;
        }
        if let Some(mut cookie) = observation
            .received
            .or(observation.sent)
            .filter(|_| !observation.deleted)
            && let Some(model) = observation
                .reported_model
                .filter(|model| !model.is_empty() && model.len() <= 256)
        {
            cookie.reported_model = model;
            // 池里只存完整 pair：半截 Cookie 不能拼进旧记录里凑成可回放凭证。
            if cookie.origin == observation.origin
                && cookie.has_pair()
                && cookie.expires_at > Utc::now().timestamp()
            {
                sqlx::query("insert into openai_routing_cookies(origin, pod, name, value, cflb_name, cflb_value, cflb_expires_at, issued_at, expires_at, observed_at, reported_model) values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) on conflict (origin,pod) do update set name = case when excluded.issued_at >= openai_routing_cookies.issued_at then excluded.name else openai_routing_cookies.name end, value = case when excluded.issued_at >= openai_routing_cookies.issued_at then excluded.value else openai_routing_cookies.value end, cflb_name = case when excluded.issued_at >= openai_routing_cookies.issued_at then excluded.cflb_name else openai_routing_cookies.cflb_name end, cflb_value = case when excluded.issued_at >= openai_routing_cookies.issued_at then excluded.cflb_value else openai_routing_cookies.cflb_value end, cflb_expires_at = case when excluded.issued_at >= openai_routing_cookies.issued_at then excluded.cflb_expires_at else openai_routing_cookies.cflb_expires_at end, issued_at = greatest(excluded.issued_at, openai_routing_cookies.issued_at), expires_at = case when excluded.issued_at >= openai_routing_cookies.issued_at then excluded.expires_at else openai_routing_cookies.expires_at end, observed_at = excluded.observed_at, reported_model = case when openai_routing_cookies.observed_at = excluded.observed_at and openai_routing_cookies.reported_model <> excluded.reported_model then '' else excluded.reported_model end where openai_routing_cookies.observed_at < excluded.observed_at or (openai_routing_cookies.observed_at = excluded.observed_at and openai_routing_cookies.value is not null and excluded.reported_model <> '')")
                    .bind(&cookie.origin).bind(&cookie.pod).bind(&cookie.name).bind(&cookie.value).bind(&cookie.cflb_name).bind(&cookie.cflb_value).bind(cookie.cflb_expires_at).bind(cookie.issued_at).bind(cookie.expires_at).bind(observation.observed_at).bind(&cookie.reported_model).execute(&mut *tx).await.map_err(unavailable)?;
            }
        }
        sqlx::query("delete from openai_routing_cookies where (origin,pod) not in (select origin,pod from openai_routing_cookies order by observed_at desc limit 256)").execute(&mut *tx).await.map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)
    }
}
