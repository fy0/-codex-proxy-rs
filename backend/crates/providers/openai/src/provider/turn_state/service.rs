//! 令牌只在准确的账号/模型桶内流转；凭据读取不调用任何刷新入口。

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use chrono::Utc;
use gateway_core::account::{
    OutboundProxy, ProviderAccount, ProviderAccountId, ProviderAccountStore, TurnStateBucket,
    TurnStateConfig, TurnStateObservation, TurnStateStopStrategy, TurnStateToken,
};
use secrecy::ExposeSecret;

use crate::credential::{CodexCredentialRepository, parse_access_token_expiration};
use crate::transport::{
    CodexWebSocketPool, TurnStateObserver, TurnStateResponse, profile::CodexWireProfileState,
    tls::build_reqwest_client_with_custom_ca,
};

use super::probe;

pub(crate) struct TurnStateService {
    pub(super) store: Arc<dyn ProviderAccountStore>,
    repository: CodexCredentialRepository,
    profile: CodexWireProfileState,
    pub(super) endpoint: String,
    pool: Arc<CodexWebSocketPool>,
}

pub(super) enum ProbeOutcome {
    Installed,
    Miss,
    Skipped,
}

impl TurnStateService {
    pub(crate) fn new(
        store: Arc<dyn ProviderAccountStore>,
        profile: CodexWireProfileState,
        endpoint: String,
        pool: Arc<CodexWebSocketPool>,
    ) -> Self {
        Self {
            repository: CodexCredentialRepository::new(Arc::clone(&store)),
            store,
            profile,
            endpoint,
            pool,
        }
    }

    pub(crate) async fn current(
        &self,
        account: &ProviderAccountId,
        model: &str,
    ) -> Option<TurnStateBucket> {
        match self.store.turn_state_bucket(account, model).await {
            Ok(bucket) => bucket.map(|mut bucket| {
                bucket
                    .routing_cookies
                    .retain(|cookie| cookie.origin == self.endpoint);
                bucket
            }),
            Err(_) => {
                tracing::warn!(
                    account_id = account.as_str(),
                    model,
                    "turn state lookup failed"
                );
                None
            }
        }
    }

    pub(crate) fn observer(
        self: &Arc<Self>,
        account: &ProviderAccount,
        model: &str,
        effort: Option<String>,
        request_state_source: &'static str,
        request_state: Option<String>,
    ) -> TurnStateObserver {
        let service = Arc::clone(self);
        let account_id = account.id().clone();
        let upstream_account_id = account.upstream_account_id().map(str::to_owned);
        let upstream_user_id = account.upstream_user_id().map(str::to_owned);
        let model = model.to_owned();
        let egress = account
            .outbound_proxy()
            .map_or_else(|| "direct".to_owned(), OutboundProxy::endpoint);
        Arc::new(move |response| {
            let service = Arc::clone(&service);
            let account_id = account_id.clone();
            let upstream_account_id = upstream_account_id.clone();
            let upstream_user_id = upstream_user_id.clone();
            let model = model.clone();
            let egress = egress.clone();
            let effort = effort.clone();
            let request_state = request_state.clone();
            Box::pin(async move {
                let bucket = service.current(&account_id, &model).await;
                let latest_issued_at = bucket.as_ref().and_then(|bucket| {
                    bucket
                        .current_issued_at
                        .max(bucket.candidate.as_ref().map(|token| token.issued_at))
                });
                let config = bucket.map(|bucket| bucket.config).unwrap_or_default();
                if config.cookie_lock_enabled && !config.enabled {
                    return;
                }
                let observation = TurnStateObservation {
                    observation_id: None,
                    is_installed: false,
                    hunt_attempts: None,
                    hunt_seconds: None,
                    account_id: account_id.as_str().to_owned(),
                    upstream_account_id,
                    upstream_user_id,
                    model,
                    observed_at: Utc::now().timestamp(),
                    started_at: None,
                    source: "passive".to_owned(),
                    request_state_source: Some(request_state_source.to_owned()),
                    response_source: None,
                    probe_trigger: None,
                    outcome: String::new(),
                    http_status: None,
                    token_length: None,
                    issued_at: None,
                    reported_model: None,
                    oailb_host: None,
                    cookie_issued_at: None,
                    cookie_expires_at: None,
                    token: None,
                    has_token: false,
                    egress,
                    shape: None,
                    effort,
                    elapsed_ms: 0,
                    probe_id: None,
                    stop_mode: None,
                    stop_reason: None,
                    answer: None,
                    answer_match: None,
                };
                service
                    .observe(
                        observation,
                        response,
                        &config,
                        request_state.as_deref(),
                        latest_issued_at,
                    )
                    .await;
            })
        })
    }

    async fn observe(
        &self,
        mut observation: TurnStateObservation,
        response: TurnStateResponse,
        config: &TurnStateConfig,
        request_state: Option<&str>,
        latest_issued_at: Option<i64>,
    ) {
        observation.response_source = Some(response.source.to_owned());
        observation.http_status = response.status;
        observation.elapsed_ms = response.elapsed_ms;
        observation.token_length = response.value.as_ref().map(Vec::len);
        observation.reported_model = response.reported_model.clone();
        let token = response
            .value
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(TurnStateToken::parse);
        observation.issued_at = token.as_ref().map(|token| token.issued_at);
        observation.outcome = if response.transport_error {
            "transport_error"
        } else if response.value.is_none() {
            "missing_header"
        } else if token.is_none() {
            "invalid_token"
        } else if response
            .status
            .is_some_and(|status| !(200..300).contains(&status) && status != 101)
        {
            "http_error"
        } else if token
            .as_ref()
            .is_some_and(|token| !token.is_fresh(observation.observed_at, config.ttl_seconds))
        {
            "expired_or_future"
        } else if observation.token_length != Some(config.target_length) {
            "length_miss"
        } else if token
            .as_ref()
            .is_some_and(|token| Some(token.value.as_str()) == request_state)
        {
            "reused_state"
        } else if token
            .as_ref()
            .is_some_and(|token| !token.is_newer_than(latest_issued_at))
        {
            "not_newer"
        } else {
            "candidate"
        }
        .to_owned();
        // 非目标票可以留存，但过期或未来签发的票只能保留诊断元数据。
        observation.token = token
            .as_ref()
            .filter(|token| token.is_fresh(observation.observed_at, config.ttl_seconds))
            .map(|token| token.value.clone());
        let candidate = (observation.outcome == "candidate")
            .then_some(token)
            .flatten();
        let mut revoked = false;
        if config.detect_actual_model
            && let Some(reported) = observation.reported_model.clone()
            && let Some(sent) = request_state.filter(|value| !value.is_empty())
            && let Ok(account) = ProviderAccountId::new(observation.account_id.clone())
        {
            let model = observation.model.clone();
            match self
                .store
                .observe_installed_model(
                    &account,
                    &model,
                    sent,
                    &reported,
                    config.revokes_ticket_when_model_detaches(),
                )
                .await
            {
                Ok(true) => {
                    observation.outcome = "model_detached".to_owned();
                    revoked = true;
                }
                Ok(false) => {}
                Err(_) => tracing::warn!(
                    account_id = observation.account_id,
                    model = observation.model,
                    "turn state model observation failed"
                ),
            }
        }
        let account_id = observation.account_id.clone();
        self.persist(observation, candidate).await;
        if revoked {
            // 旧连接里的票已经脱离实际模型，不能继续复用。
            self.pool.evict_account(&account_id).await;
        }
    }

    pub(super) async fn persist(
        &self,
        observation: TurnStateObservation,
        candidate: Option<TurnStateToken>,
    ) {
        // 是否记录由存储在同一事务内按最新开关判断，避免停用后仍输出观察日志。
        if self
            .store
            .observe_turn_state(observation, candidate)
            .await
            .is_err()
        {
            tracing::warn!("turn state observation persistence failed");
        }
    }

    pub(super) async fn install(&self, account: &ProviderAccountId, model: &str) -> bool {
        match self.store.install_turn_state(account, model).await {
            Ok(true) => {
                // 旧 WS 握手已经固化令牌，安装后立即失效旧连接。
                self.pool.evict_account(account.as_str()).await;
                tracing::info!(account_id = account.as_str(), model, "turn state installed");
                true
            }
            Ok(false) => false,
            Err(_) => {
                tracing::warn!(
                    account_id = account.as_str(),
                    model,
                    "turn state installation failed"
                );
                false
            }
        }
    }

    pub(super) async fn probe(
        &self,
        bucket: &TurnStateBucket,
        account_id: &ProviderAccountId,
        manual: bool,
    ) -> ProbeOutcome {
        let mut observation = TurnStateObservation {
            observation_id: None,
            is_installed: false,
            hunt_attempts: None,
            hunt_seconds: None,
            account_id: bucket.account_id.clone(),
            upstream_account_id: bucket.upstream_account_id.clone(),
            upstream_user_id: bucket.upstream_user_id.clone(),
            model: bucket.model.clone(),
            observed_at: Utc::now().timestamp(),
            started_at: None,
            source: "probe".to_owned(),
            request_state_source: Some("none".to_owned()),
            response_source: None,
            probe_trigger: Some(if manual { "manual" } else { "scheduled" }.to_owned()),
            outcome: String::new(),
            http_status: None,
            token_length: None,
            issued_at: None,
            reported_model: None,
            oailb_host: None,
            cookie_issued_at: None,
            cookie_expires_at: None,
            token: None,
            has_token: false,
            egress: "none".to_owned(),
            shape: None,
            effort: None,
            elapsed_ms: 0,
            probe_id: None,
            stop_mode: None,
            stop_reason: None,
            answer: None,
            answer_match: None,
        };
        let loaded = match self.store.load_current_credential(account_id).await {
            Ok(loaded) => loaded,
            Err(_) => return self.skip(observation, "credential_unavailable").await,
        };
        let account = &loaded.account;
        observation.upstream_account_id = account.upstream_account_id().map(str::to_owned);
        observation.upstream_user_id = account.upstream_user_id().map(str::to_owned);
        if !account.enabled() || !account.model_access().allows(&bucket.model) {
            return ProbeOutcome::Skipped;
        }
        let credential = match self.repository.decode_runtime_credential(&loaded) {
            Ok(credential) => credential,
            Err(_) => return self.skip(observation, "credential_invalid").await,
        };
        let Some(secret) = credential.authentication.oauth() else {
            return self.skip(observation, "oauth_required").await;
        };
        let expires = parse_access_token_expiration(secret.access_token.expose_secret())
            .map(SystemTime::from)
            .or(account.access_token_expires_at());
        if expires.is_none_or(|time| time <= SystemTime::now() + Duration::from_secs(30)) {
            return self
                .skip(observation, "access_token_expired_or_unknown")
                .await;
        }
        let Some(upstream_account_id) = account.upstream_account_id() else {
            return self.skip(observation, "missing_account_identity").await;
        };
        let mut exits = Vec::new();
        if bucket.config.include_account_proxy {
            exits.push(account.outbound_proxy().cloned());
        }
        if bucket.config.include_direct && !exits.contains(&None) {
            exits.push(None);
        }
        let proxies = match self
            .store
            .turn_state_proxies(&bucket.config.proxy_ids)
            .await
        {
            Ok(proxies) => proxies,
            Err(_) => return self.skip(observation, "proxy_pool_unavailable").await,
        };
        for proxy in proxies {
            if !exits.contains(&Some(proxy.clone())) {
                exits.push(Some(proxy));
            }
        }
        if exits.is_empty() {
            return self.skip(observation, "proxy_pool_empty").await;
        }
        let proxy = exits[probe::random_index(exits.len())].as_ref();
        observation.egress = proxy.map_or_else(|| "direct".to_owned(), OutboundProxy::endpoint);
        // 探测画像属于模型桶，不覆盖账号的业务画像或出口位置。
        let request = probe::request(&bucket.model, &bucket.config, &self.profile, Utc::now());
        observation.shape = Some(request.shape.to_owned());
        observation.effort = Some(request.effort.to_owned());
        observation.probe_id = Some(request.id);
        let read_output = bucket.config.cookie_lock_enabled
            || match bucket.config.stop_strategy {
                TurnStateStopStrategy::Headers => false,
                TurnStateStopStrategy::FirstOutput => true,
                TurnStateStopStrategy::Mixed => probe::random_index(2) == 1,
            };
        observation.stop_mode = Some(
            if read_output {
                "first_output"
            } else {
                "headers"
            }
            .to_owned(),
        );
        let started = Instant::now();
        observation.started_at = Some(Utc::now().timestamp());
        let mut builder = reqwest::Client::builder()
            .use_native_tls()
            .http1_only()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(0);
        if let Some(proxy) = proxy {
            let Ok(proxy) = reqwest::Proxy::all(proxy.expose_url()) else {
                return self.skip(observation, "proxy_configuration_error").await;
            };
            builder = builder.proxy(proxy);
        }
        let Ok(client) = build_reqwest_client_with_custom_ca(builder) else {
            return self.skip(observation, "client_configuration_error").await;
        };
        let Some(body) = serde_json::to_vec(&request.body)
            .ok()
            .and_then(|body| zstd::stream::encode_all(std::io::Cursor::new(body), 3).ok())
        else {
            return self.skip(observation, "template_error").await;
        };
        // state 探测保持裸请求；Cookie 续约只携带池内路由凭证。
        let sent_cookie = bucket
            .config
            .cookie_lock_enabled
            .then(|| bucket.renewal_cookie(Utc::now().timestamp()))
            .flatten()
            .filter(|cookie| cookie.origin == self.endpoint);
        let request_started_at = Utc::now().timestamp_millis();
        let mut outbound = client
            .post(&self.endpoint)
            .headers(request.headers)
            .bearer_auth(secret.access_token.expose_secret())
            .header("chatgpt-account-id", upstream_account_id)
            .header("content-encoding", "zstd")
            .body(body);
        if let Some(cookie) = sent_cookie {
            outbound = outbound.header("cookie", cookie.header());
        }
        let result = outbound.send().await;
        let cookie_headers = result
            .as_ref()
            .ok()
            .map(|response| crate::transport::response_meta::set_cookie_headers(response.headers()))
            .unwrap_or_default();
        observation.observed_at = Utc::now().timestamp();
        let mut response = TurnStateResponse {
            status: result
                .as_ref()
                .ok()
                .map(|response| response.status().as_u16()),
            value: result
                .as_ref()
                .ok()
                .and_then(|response| response.headers().get("x-codex-turn-state"))
                .map(|value| value.as_bytes().to_vec()),
            reported_model: result.as_ref().ok().and_then(|response| {
                crate::transport::response_meta::reported_model(response.headers())
            }),
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            transport_error: result.is_err(),
            source: "http_headers",
        };
        // 只在明确选择回应策略时读取有界 SSE，任何策略都不保留响应正文。
        // 头块未声明模型时，流内事件的模型声明补进同一份观测。
        let mut body_model: Option<String> = None;
        let mut created_model = None;
        observation.stop_reason = Some(
            match result {
                Ok(upstream) if read_output && upstream.status().is_success() => {
                    match tokio::time::timeout(
                        Duration::from_secs(30),
                        probe::wait_for_output(
                            upstream,
                            bucket.config.cookie_lock_enabled,
                            Some(request.expect),
                        ),
                    )
                    .await
                    {
                        Ok(output) => {
                            created_model = output.created_model;
                            body_model = output.reported_model;
                            observation.answer = output.answer;
                            observation.answer_match = output.answer_match;
                            output.reason
                        }
                        Err(_) => "body_timeout",
                    }
                }
                Ok(_) => "headers",
                Err(_) => "transport_error",
            }
            .to_owned(),
        );
        if response.reported_model.is_none() {
            response.reported_model = body_model;
        }
        response.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        drop(client);
        if bucket.config.cookie_lock_enabled {
            let successful = response
                .status
                .is_some_and(|status| (200..300).contains(&status));
            let (cookie, deleted) = if successful {
                self.observe_cookie(
                    &cookie_headers,
                    sent_cookie,
                    created_model.as_deref(),
                    request_started_at,
                )
                .await
            } else {
                (None, false)
            };
            observation.http_status = response.status;
            observation.elapsed_ms = response.elapsed_ms;
            observation.response_source = Some("response_created".to_owned());

            observation.oailb_host = cookie.as_ref().map(|cookie| cookie.pod.clone());
            observation.cookie_issued_at = cookie.as_ref().map(|cookie| cookie.issued_at);
            observation.cookie_expires_at = cookie.as_ref().map(|cookie| cookie.expires_at);
            let usable = !deleted
                && cookie
                    .as_ref()
                    .is_some_and(|cookie| cookie.is_usable(&bucket.model, Utc::now().timestamp()))
                && cookie
                    .as_ref()
                    .is_some_and(|cookie| bucket.cookie_is_selectable(cookie))
                && created_model
                    .as_deref()
                    .is_some_and(|model| model.eq_ignore_ascii_case(&bucket.model));
            observation.outcome = if response.transport_error {
                "transport_error"
            } else if !successful {
                "http_error"
            } else if deleted {
                "cookie_deleted"
            } else if created_model.is_none() {
                "missing_model"
            } else if cookie.is_none() {
                "missing_cookie"
            } else if cookie
                .as_ref()
                .is_some_and(|cookie| !bucket.cookie_is_selectable(cookie))
            {
                "cookie_gateway_filtered"
            } else if usable {
                "cookie_ready"
            } else {
                "cookie_model_mismatch"
            }
            .to_owned();
            observation.reported_model = created_model;
            self.persist(observation, None).await;
            return if usable {
                ProbeOutcome::Installed
            } else {
                ProbeOutcome::Miss
            };
        }
        let latest_issued_at = bucket
            .current_issued_at
            .max(bucket.candidate.as_ref().map(|token| token.issued_at));
        self.observe(
            observation,
            response,
            &bucket.config,
            None,
            latest_issued_at,
        )
        .await;
        if self.install(account_id, &bucket.model).await {
            ProbeOutcome::Installed
        } else {
            ProbeOutcome::Miss
        }
    }

    async fn skip(&self, mut observation: TurnStateObservation, reason: &str) -> ProbeOutcome {
        observation.outcome = reason.to_owned();
        self.persist(observation, None).await;
        ProbeOutcome::Skipped
    }
}
