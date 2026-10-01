//! 路由 Cookie 仅来自同一上游响应，模型声明与请求开始水位一起提交。

use chrono::Utc;
use gateway_core::account::{ProviderAccountId, RoutingCookie, RoutingCookieObservation};
use secrecy::ExposeSecret;

use super::TurnStateService;
use crate::credential::CodexCookiePolicy;

/// 一次业务响应里与路由 Cookie 观测相关的全部输入。
pub(crate) struct BusinessCookieObservation<'a> {
    pub account: &'a gateway_core::account::ProviderAccount,
    pub headers: &'a [String],
    pub sent: Option<&'a RoutingCookie>,
    pub requested_model: &'a str,
    /// 首个 `response.created` 锁住的模型；调用方保证来自 decoder 的 created 访问器。
    pub model: &'a str,
    /// 本次请求实际发出的票据正文；作废只认这张票。
    pub request_state: Option<&'a str>,
    pub request_state_source: &'static str,
    pub response_state: Option<&'a str>,
    pub started_at: i64,
}

impl TurnStateService {
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// created 模型脱离请求模型：标坏已发 pair；revoke_ticket 打开时同时作废票、驱逐旧连接。
    /// `confirmed` 表示作废写库前已确认桶里装的正是这张票这把 pair：失效同一事务
    /// 会连票一起摘除，之后按票正文匹配的二次作废必然落空，但连接仍须驱逐。
    async fn detach_routing(
        &self,
        observation: &BusinessCookieObservation<'_>,
        revoke_ticket: bool,
        confirmed: bool,
    ) -> bool {
        if let Some(sent) = observation.sent {
            let _ = self
                .store
                .observe_routing_cookie(RoutingCookieObservation {
                    origin: self.endpoint.clone(),
                    observed_at: observation.started_at,
                    sent: Some(sent.clone()),
                    received: None,
                    reported_model: Some(observation.model.to_owned()),
                    deleted: true,
                })
                .await;
            // 标坏的 pair 同步从账号闸的种子表摘除，下一轮不再拿它定向打票。
            self.drop_pair_seed(observation.account.id().as_str(), sent);
        }
        if !revoke_ticket {
            return false;
        }
        let account_id = observation.account.id().clone();
        let Some(sent_state) = observation.request_state.filter(|value| !value.is_empty()) else {
            return false;
        };
        let revoked = self
            .store
            .observe_installed_model(
                &account_id,
                observation.requested_model,
                sent_state,
                observation.model,
                true,
                observation.sent,
            )
            .await
            .unwrap_or(false);
        if revoked || confirmed {
            // 旧连接里的票已经脱离实际模型，不能继续复用。
            self.pool.evict_account(account_id.as_str()).await;
        }
        revoked || confirmed
    }

    pub(crate) async fn observe_business_cookie(&self, observation: BusinessCookieObservation<'_>) {
        let BusinessCookieObservation {
            account,
            headers,
            sent,
            requested_model,
            model,
            request_state,
            request_state_source,
            response_state,
            started_at,
        } = observation;
        let mismatched = !model.eq_ignore_ascii_case(requested_model);
        // 作废前先读桶：Cookie 失效写库会连带摘除绑定票，之后按票正文匹配的
        // 二次作废会落空；这里先确认“装的就是这张票这把 pair”，供后续驱逐判断。
        let bucket = self.current(account.id(), requested_model).await;
        let confirmed = mismatched
            && request_state.is_some_and(|state| !state.is_empty())
            && bucket.as_ref().is_some_and(|bucket| {
                bucket.config.revokes_ticket_when_model_detaches()
                    && bucket
                        .current
                        .as_ref()
                        .is_some_and(|token| Some(token.value.as_str()) == request_state)
                    && sent.is_none_or(|sent| {
                        bucket
                            .installed_pair
                            .as_ref()
                            .is_none_or(|bound| bound.same_pair(sent))
                    })
            });
        let (cookie, deleted) = self
            .observe_cookie(
                account.id(),
                headers,
                sent,
                Some(model),
                started_at,
                mismatched,
            )
            .await;
        let Some(bucket) = bucket else {
            return;
        };
        // 首个 created 模型脱离请求模型时已发 pair 一律标坏；票的作废仍按
        // detect_actual_model/pair 模式开关执行。即使报头仍报请求模型、响应没有
        // 新票，这条路径也一样生效，之后请求不得再注入同一张票。
        if mismatched {
            self.detach_routing(
                &observation,
                bucket.config.revokes_ticket_when_model_detaches(),
                confirmed,
            )
            .await;
        }
        // 脱离模型的 pair 不作为本模型的可用凭据登记。
        let cookie = if mismatched { None } else { cookie };
        // 未托管桶只在响应真正下发完整 pair 时留观测行：这条记录是管理端展示
        // 端点与复制 Cookie 的唯一载体；没有 pair 的普通业务响应不刷日志。
        if !bucket.config.cookie_lock_enabled
            && !bucket.config.requires_route_pair()
            && cookie.is_none()
        {
            return;
        }
        let outcome = if mismatched {
            "cookie_model_mismatch"
        } else if deleted {
            "cookie_deleted"
        } else if cookie.is_none() {
            "missing_cookie"
        } else if cookie
            .as_ref()
            .is_some_and(|cookie| !bucket.config.allows_cookie_gateway(&cookie.pod))
        {
            "cookie_gateway_filtered"
        } else {
            "cookie_ready"
        };
        // 响应的 x-codex-turn-state 一并解析留存，记录可直接复制或套用为票；
        // 过期或未来签发的票只保留元数据，与主动观测一致。
        let observed_at = Utc::now().timestamp();
        let token = response_state.and_then(gateway_core::account::TurnStateToken::parse);
        self.persist(
            gateway_core::account::TurnStateObservation {
                observation_id: None,
                account_id: account.id().as_str().to_owned(),
                upstream_account_id: account.upstream_account_id().map(str::to_owned),
                upstream_user_id: account.upstream_user_id().map(str::to_owned),
                model: requested_model.to_owned(),
                observed_at,
                started_at: Some(started_at / 1000),
                source: "passive".to_owned(),
                request_state_source: Some(request_state_source.to_owned()),
                response_source: Some("response_created".to_owned()),
                probe_trigger: None,
                outcome: outcome.to_owned(),
                http_status: None,
                token_length: response_state.map(str::len),
                issued_at: token.as_ref().map(|token| token.issued_at),
                expires_at: None,
                endpoint: None,
                reported_model: Some(model.to_owned()),
                oailb_host: cookie.as_ref().map(|cookie| cookie.pod.clone()),
                cookie_issued_at: cookie.as_ref().map(|cookie| cookie.issued_at),
                cookie_expires_at: cookie.as_ref().map(|cookie| cookie.expires_at),
                cookie_origin: cookie.as_ref().map(|cookie| cookie.origin.clone()),
                cookie_name: cookie.as_ref().map(|cookie| cookie.name.clone()),
                cookie_value: cookie.as_ref().map(|cookie| cookie.value.clone()),
                cookie_cflb_name: cookie.as_ref().and_then(|cookie| {
                    (!cookie.cflb_name.is_empty()).then(|| cookie.cflb_name.clone())
                }),
                cookie_cflb_value: cookie.as_ref().and_then(|cookie| {
                    (!cookie.cflb_value.is_empty()).then(|| cookie.cflb_value.clone())
                }),
                pair: None,
                has_cookie: false,
                token: token
                    .as_ref()
                    .filter(|token| token.is_fresh(observed_at, bucket.config.ttl_seconds))
                    .map(|token| token.value.clone()),
                has_token: false,
                is_installed: false,
                hunt_attempts: None,
                hunt_seconds: None,
                egress: account.outbound_proxy().map_or_else(
                    || "direct".to_owned(),
                    gateway_core::account::OutboundProxy::endpoint,
                ),
                shape: None,
                effort: None,
                elapsed_ms: 0,
                probe_id: None,
                stop_mode: None,
                stop_reason: None,
                answer: None,
                answer_match: None,
            },
            None,
        )
        .await;
    }

    /// 解析本响应的路由 Cookie pair 并把观测事实持久化到共享池。
    ///
    /// 定向请求没收到任何 LB Cookie 时沿用已发 pair；同一响应必须同时给
    /// `__cflb` 与 `__oailb`/`__oai_lb` 两半才算新 pair，拿新的一半配旧的
    /// 另一半不制造凭据；显式删除、过期或畸形都判已发 pair 失效。
    /// `__cf_bm` 不属于 LB Cookie，只有它出现时 pair 不算被替换。
    /// `suppress_received` 打开时（模型脱离/created 无效等）新收到的 pair 只进观测
    /// 返回值，不登记进共享池——坏模型响应产出的凭据不能被当作可用凭据提升。
    pub(crate) async fn observe_cookie(
        &self,
        account: &ProviderAccountId,
        headers: &[String],
        sent: Option<&RoutingCookie>,
        model: Option<&str>,
        started_at: i64,
        suppress_received: bool,
    ) -> (Option<RoutingCookie>, bool) {
        let Ok(origin) = url::Url::parse(&self.endpoint) else {
            return (None, false);
        };
        let Some(host) = origin.host_str() else {
            return (None, false);
        };
        let Ok(policy) = CodexCookiePolicy::new(["__oailb", "__oai_lb", "__cflb"], [host]) else {
            return (None, false);
        };
        let now = Utc::now();
        let parsed = policy.parse_response_headers("", 0, &origin, headers, now);
        let mut oailb: Option<RoutingCookie> = None;
        let mut cflb: Option<(String, Option<chrono::DateTime<Utc>>)> = None;
        let mut deleted = false;
        let mut lb_seen = false;
        // 先按原始报头找 LB 名字：解析阶段丢掉的空值/畸形半边同样判已发 pair 失效，
        // 不能和“这响应根本没带 LB Cookie”混为一谈。
        let mut consumed = vec![false; parsed.inputs.len()];
        for header in headers {
            let Some((name, _)) = header.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if !matches!(name, "__cflb" | "__oailb" | "__oai_lb") {
                continue;
            }
            lb_seen = true;
            let input = parsed
                .inputs
                .iter()
                .enumerate()
                .find(|(index, input)| !consumed[*index] && input.name == name)
                .map(|(index, input)| {
                    consumed[index] = true;
                    input
                });
            let Some(input) = input else {
                // 名字命中但解析/校验没过：这半边不可用，pair 不完整。
                deleted = true;
                continue;
            };
            if !policy.may_replay(
                &origin,
                input
                    .domain_attribute
                    .as_deref()
                    .unwrap_or(host)
                    .trim_start_matches('.'),
                &input.path,
                input.domain_attribute.is_none(),
                input.secure,
            ) {
                deleted = true;
                continue;
            }
            match input.name.as_str() {
                "__cflb" => {
                    // __cflb 没有内嵌死线，HTTP 过期/清空属性直接判失效。
                    if input.delete || input.value.expose_secret().is_empty() {
                        deleted = true;
                    } else {
                        cflb = Some((input.value.expose_secret().to_owned(), input.expires_at));
                    }
                }
                _ => {
                    // __oailb/__oai_lb 以内嵌 JWT exp 为准：HTTP 过期标记不否决仍有效的
                    // JWT；真正清空（空值）或 JWT 本身不合法/已过期才算删除。
                    let value = input.value.expose_secret().to_owned();
                    match RoutingCookie::parse(&self.endpoint, &input.name, &value, now.timestamp())
                    {
                        Some(cookie) if !value.is_empty() => oailb = Some(cookie),
                        _ => deleted = true,
                    }
                }
            }
        }
        // 同一响应必须同时给两半；只来一半时既不制造 pair，也视为已发 pair 的路由失效。
        let pair = match (oailb, cflb) {
            (Some(cookie), Some((cflb_value, cflb_expiry))) => {
                cookie.with_cflb("__cflb", &cflb_value, cflb_expiry.map(|at| at.timestamp()))
            }
            _ => None,
        };
        if lb_seen && pair.is_none() {
            deleted = true;
        }
        let mut received = match (pair, lb_seen) {
            (Some(pair), _) => Some(pair),
            (None, true) => None,
            // 定向请求静默沿用已发 pair，并把这次成功记录回共享池。
            (None, false) => sent.cloned(),
        };
        if let Some(cookie) = received.as_mut() {
            cookie.observed_at = started_at;
            cookie.reported_model = model.unwrap_or_default().to_owned();
        }
        let observation = RoutingCookieObservation {
            origin: self.endpoint.clone(),
            observed_at: started_at,
            sent: sent.cloned(),
            received: if suppress_received {
                None
            } else {
                received.clone()
            },
            reported_model: model.map(str::to_owned),
            deleted,
        };
        if self
            .store
            .observe_routing_cookie(observation)
            .await
            .is_err()
        {
            tracing::warn!("routing cookie observation persistence failed");
        }
        // 已发 pair 被判失效（显式删除/半残或换上其它 pod）时同步摘除共享种子；
        // 与存储的失效判定条件一致，精确匹配不误伤同 pod 更新值。
        if let Some(sent) = sent
            && (deleted
                || received
                    .as_ref()
                    .is_some_and(|cookie| cookie.pod != sent.pod))
        {
            self.drop_pair_seed(account.as_str(), sent);
        }
        (received, deleted)
    }
}
