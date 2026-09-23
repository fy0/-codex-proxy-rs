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

    /// 管理员按需复制仍有效的 Cookie 正文；状态轮询不携带值。
    pub(super) async fn routing_cookie_value(
        &self,
        pod: &str,
    ) -> Result<Option<RoutingCookie>, CoreStoreError> {
        let row = sqlx::query("select * from openai_routing_cookies where pod = $1 and value is not null and expires_at > extract(epoch from now())")
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
