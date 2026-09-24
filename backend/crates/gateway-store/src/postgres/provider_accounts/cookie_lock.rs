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

    pub(super) async fn load_routing_cookies(&self) -> Result<Vec<RoutingCookie>, CoreStoreError> {
        let rows = sqlx::query("select * from openai_routing_cookies where value is not null and expires_at > extract(epoch from now()) order by observed_at desc limit 256").fetch_all(&self.pool).await.map_err(unavailable)?;
        rows.into_iter()
            .map(|row| {
                Ok(RoutingCookie {
                    origin: row.try_get("origin").map_err(unavailable)?,
                    pod: row.try_get("pod").map_err(unavailable)?,
                    name: row.try_get("name").map_err(unavailable)?,
                    value: row.try_get("value").map_err(unavailable)?,
                    issued_at: row.try_get("issued_at").map_err(unavailable)?,
                    expires_at: row.try_get("expires_at").map_err(unavailable)?,
                    observed_at: row.try_get("observed_at").map_err(unavailable)?,
                    reported_model: row.try_get("reported_model").map_err(unavailable)?,
                })
            })
            .collect()
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
        let row = sqlx::query("select detail->>'cookieName' as name, detail->>'cookieValue' as value, detail->>'oailbHost' as pod, detail->>'cookieOrigin' as origin, (detail->>'cookieIssuedAt')::bigint as issued_at, (detail->>'cookieExpiresAt')::bigint as expires_at, coalesce(detail->>'reportedModel', '') as reported_model from account_turn_state_events where id = $1 and account_id = $2 and model = $3 and event_kind = 'observation'")
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
        let pod: Option<String> = row.try_get("pod").map_err(unavailable)?;
        let origin: Option<String> = row.try_get("origin").map_err(unavailable)?;
        let (Some(name), Some(value), Some(pod)) = (name, value, pod) else {
            return Ok(None);
        };
        if name.is_empty() || value.is_empty() || pod.is_empty() {
            return Ok(None);
        }
        let issued_at: Option<i64> = row.try_get("issued_at").map_err(unavailable)?;
        let expires_at: Option<i64> = row.try_get("expires_at").map_err(unavailable)?;
        let reported_model: String = row.try_get("reported_model").map_err(unavailable)?;
        Ok(Some(RoutingCookie {
            origin: origin.unwrap_or_default(),
            pod,
            name,
            value,
            issued_at: issued_at.unwrap_or(0),
            expires_at: expires_at.unwrap_or(0),
            observed_at: 0,
            reported_model,
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
        sqlx::query("insert into openai_routing_cookies(origin, pod, name, value, issued_at, expires_at, observed_at, reported_model) values ($1,$2,$3,$4,$5,$6,$7,$8) on conflict (origin, pod) do update set name = excluded.name, value = excluded.value, issued_at = excluded.issued_at, expires_at = excluded.expires_at, observed_at = excluded.observed_at, reported_model = excluded.reported_model")
            .bind(origin)
            .bind(&cookie.pod)
            .bind(&cookie.name)
            .bind(&cookie.value)
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
        let row = sqlx::query("select * from openai_routing_cookies where pod = $1 and value is not null and expires_at > extract(epoch from now()) order by observed_at desc limit 1")
            .bind(pod)
            .fetch_optional(&self.pool)
            .await
            .map_err(unavailable)?;
        row.map(|row| {
            Ok(RoutingCookie {
                origin: row.try_get("origin").map_err(unavailable)?,
                pod: row.try_get("pod").map_err(unavailable)?,
                name: row.try_get("name").map_err(unavailable)?,
                value: row.try_get("value").map_err(unavailable)?,
                issued_at: row.try_get("issued_at").map_err(unavailable)?,
                expires_at: row.try_get("expires_at").map_err(unavailable)?,
                observed_at: row.try_get("observed_at").map_err(unavailable)?,
                reported_model: row.try_get("reported_model").map_err(unavailable)?,
            })
        })
        .transpose()
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
            sqlx::query("update openai_routing_cookies set value = null, reported_model = '', observed_at = $3 where origin = $1 and pod = $2 and observed_at <= $3")
                .bind(&observation.origin).bind(&sent.pod).bind(observation.observed_at).execute(&mut *tx).await.map_err(unavailable)?;
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
            if cookie.origin == observation.origin && cookie.expires_at > Utc::now().timestamp() {
                sqlx::query("insert into openai_routing_cookies(origin, pod, name, value, issued_at, expires_at, observed_at, reported_model) values ($1,$2,$3,$4,$5,$6,$7,$8) on conflict (origin,pod) do update set name = case when excluded.issued_at >= openai_routing_cookies.issued_at then excluded.name else openai_routing_cookies.name end, value = case when excluded.issued_at >= openai_routing_cookies.issued_at then excluded.value else openai_routing_cookies.value end, issued_at = greatest(excluded.issued_at, openai_routing_cookies.issued_at), expires_at = case when excluded.issued_at >= openai_routing_cookies.issued_at then excluded.expires_at else openai_routing_cookies.expires_at end, observed_at = excluded.observed_at, reported_model = case when openai_routing_cookies.observed_at = excluded.observed_at and openai_routing_cookies.reported_model <> excluded.reported_model then '' else excluded.reported_model end where openai_routing_cookies.observed_at < excluded.observed_at or (openai_routing_cookies.observed_at = excluded.observed_at and openai_routing_cookies.value is not null and excluded.reported_model <> '')")
                    .bind(&cookie.origin).bind(&cookie.pod).bind(&cookie.name).bind(&cookie.value).bind(cookie.issued_at).bind(cookie.expires_at).bind(observation.observed_at).bind(&cookie.reported_model).execute(&mut *tx).await.map_err(unavailable)?;
            }
        }
        sqlx::query("delete from openai_routing_cookies where (origin,pod) not in (select origin,pod from openai_routing_cookies order by observed_at desc limit 256)").execute(&mut *tx).await.map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)
    }
}
