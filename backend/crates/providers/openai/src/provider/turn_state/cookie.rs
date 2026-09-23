//! 路由 Cookie 仅来自同一上游响应，模型声明与请求开始水位一起提交。

use chrono::Utc;
use gateway_core::account::{RoutingCookie, RoutingCookieObservation};
use secrecy::ExposeSecret;

use super::TurnStateService;
use crate::credential::CodexCookiePolicy;

impl TurnStateService {
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) async fn observe_business_cookie(
        &self,
        account: &gateway_core::account::ProviderAccount,
        headers: &[String],
        sent: Option<&RoutingCookie>,
        requested_model: &str,
        model: &str,
        started_at: i64,
    ) {
        let (cookie, deleted) = self
            .observe_cookie(headers, sent, Some(model), started_at)
            .await;
        let Some(bucket) = self.current(account.id(), requested_model).await else {
            return;
        };
        if !bucket.config.cookie_lock_enabled {
            return;
        }
        let outcome = if deleted {
            "cookie_deleted"
        } else if cookie.is_none() {
            "missing_cookie"
        } else if cookie
            .as_ref()
            .is_some_and(|cookie| !bucket.cookie_is_selectable(cookie))
        {
            "cookie_gateway_filtered"
        } else if model.eq_ignore_ascii_case(requested_model) {
            "cookie_ready"
        } else {
            "cookie_model_mismatch"
        };
        self.persist(
            gateway_core::account::TurnStateObservation {
                observation_id: None,
                account_id: account.id().as_str().to_owned(),
                upstream_account_id: account.upstream_account_id().map(str::to_owned),
                upstream_user_id: account.upstream_user_id().map(str::to_owned),
                model: requested_model.to_owned(),
                observed_at: Utc::now().timestamp(),
                started_at: Some(started_at / 1000),
                source: "passive".to_owned(),
                request_state_source: None,
                response_source: Some("response_created".to_owned()),
                probe_trigger: None,
                outcome: outcome.to_owned(),
                http_status: None,
                token_length: None,
                issued_at: None,
                reported_model: Some(model.to_owned()),
                oailb_host: cookie.as_ref().map(|cookie| cookie.pod.clone()),
                cookie_expires_at: cookie.as_ref().map(|cookie| cookie.expires_at),
                token: None,
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
            },
            None,
        )
        .await;
    }

    pub(crate) async fn observe_cookie(
        &self,
        headers: &[String],
        sent: Option<&RoutingCookie>,
        model: Option<&str>,
        started_at: i64,
    ) -> (Option<RoutingCookie>, bool) {
        let Ok(origin) = url::Url::parse(&self.endpoint) else {
            return (None, false);
        };
        let Some(host) = origin.host_str() else {
            return (None, false);
        };
        let Ok(policy) = CodexCookiePolicy::new(["__oailb", "__oai_lb"], [host]) else {
            return (None, false);
        };
        let now = Utc::now();
        let parsed = policy.parse_response_headers("", 0, &origin, headers, now);
        let mut received = None;
        let mut deleted = false;
        for input in parsed.inputs {
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
                continue;
            }
            if input.delete {
                deleted = true;
                continue;
            }
            if let Some(mut cookie) = RoutingCookie::parse(
                &self.endpoint,
                &input.name,
                input.value.expose_secret(),
                now.timestamp(),
            ) {
                if let Some(expiry) = input.expires_at {
                    cookie.expires_at = cookie.expires_at.min(expiry.timestamp());
                }
                cookie.observed_at = started_at;
                cookie.reported_model = model.unwrap_or_default().to_owned();
                received = Some(cookie);
                deleted = false;
            } else {
                deleted = true;
            }
        }
        let observed = received.as_ref().or(sent).cloned();
        if (received.is_some() || sent.is_some())
            && self
                .store
                .observe_routing_cookie(RoutingCookieObservation {
                    origin: self.endpoint.clone(),
                    observed_at: started_at,
                    sent: sent.cloned(),
                    received,
                    reported_model: model.map(str::to_owned),
                    deleted,
                })
                .await
                .is_err()
        {
            tracing::warn!("routing cookie observation persistence failed");
            return (None, deleted);
        }
        (observed, deleted)
    }
}
