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
    endpoint: String,
    instructions: String,
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
        instructions: String,
        pool: Arc<CodexWebSocketPool>,
    ) -> Self {
        Self {
            repository: CodexCredentialRepository::new(Arc::clone(&store)),
            store,
            profile,
            endpoint,
            instructions,
            pool,
        }
    }

    pub(crate) fn template(path: Option<&std::path::Path>) -> Result<String, ()> {
        probe::load_template(path)
    }

    pub(crate) async fn current(
        &self,
        account: &ProviderAccountId,
        model: &str,
    ) -> Option<TurnStateBucket> {
        match self.store.turn_state_bucket(account, model).await {
            Ok(bucket) => bucket,
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
            Box::pin(async move {
                let config = service
                    .current(&account_id, &model)
                    .await
                    .map(|bucket| bucket.config)
                    .unwrap_or_default();
                let observation = TurnStateObservation {
                    account_id: account_id.as_str().to_owned(),
                    upstream_account_id,
                    upstream_user_id,
                    model,
                    observed_at: Utc::now().timestamp(),
                    started_at: None,
                    source: "passive".to_owned(),
                    outcome: String::new(),
                    http_status: None,
                    token_length: None,
                    issued_at: None,
                    egress,
                    shape: None,
                    effort,
                    elapsed_ms: 0,
                    probe_id: None,
                    stop_mode: None,
                    stop_reason: None,
                };
                service.observe(observation, response, &config).await;
            })
        })
    }

    async fn observe(
        &self,
        mut observation: TurnStateObservation,
        response: TurnStateResponse,
        config: &TurnStateConfig,
    ) {
        observation.http_status = response.status;
        observation.elapsed_ms = response.elapsed_ms;
        observation.token_length = response.value.as_ref().map(Vec::len);
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
        } else if observation.token_length != Some(config.target_length) {
            "length_miss"
        } else if token
            .as_ref()
            .is_some_and(|token| !token.is_fresh(observation.observed_at, config.ttl_seconds))
        {
            "expired_or_future"
        } else {
            "candidate"
        }
        .to_owned();
        let candidate = (observation.outcome == "candidate")
            .then_some(token)
            .flatten();
        self.persist(observation, candidate).await;
    }

    async fn persist(&self, observation: TurnStateObservation, candidate: Option<TurnStateToken>) {
        tracing::info!(
            account_id = observation.account_id,
            model = observation.model,
            source = observation.source,
            outcome = observation.outcome,
            http_status = observation.http_status,
            token_length = observation.token_length,
            issued_at = observation.issued_at,
            egress = observation.egress,
            shape = observation.shape,
            effort = observation.effort,
            elapsed_ms = observation.elapsed_ms,
            probe_id = observation.probe_id,
            stop_mode = observation.stop_mode,
            stop_reason = observation.stop_reason,
            "turn state observation"
        );
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
    ) -> ProbeOutcome {
        let mut observation = TurnStateObservation {
            account_id: bucket.account_id.clone(),
            upstream_account_id: bucket.upstream_account_id.clone(),
            upstream_user_id: bucket.upstream_user_id.clone(),
            model: bucket.model.clone(),
            observed_at: Utc::now().timestamp(),
            started_at: None,
            source: "probe".to_owned(),
            outcome: String::new(),
            http_status: None,
            token_length: None,
            issued_at: None,
            egress: "none".to_owned(),
            shape: None,
            effort: None,
            elapsed_ms: 0,
            probe_id: None,
            stop_mode: None,
            stop_reason: None,
        };
        let loaded = match self.store.load_current_credential(account_id).await {
            Ok(loaded) => loaded,
            Err(_) => return self.skip(observation, "credential_unavailable").await,
        };
        let account = &loaded.account;
        observation.upstream_account_id = account.upstream_account_id().map(str::to_owned);
        observation.upstream_user_id = account.upstream_user_id().map(str::to_owned);
        if !account.enabled() || !account.model_access().allows(&bucket.model) {
            return self
                .skip(observation, "account_disabled_or_model_denied")
                .await;
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
        let timezone = account
            .request_location()
            .map_or(bucket.config.timezone, |location| location.timezone);
        let request = probe::request(
            &bucket.model,
            timezone,
            &self.instructions,
            &self.profile,
            Utc::now(),
        );
        observation.shape = Some(request.shape.to_owned());
        observation.effort = Some(request.effort.to_owned());
        observation.probe_id = Some(request.id);
        let read_output = match bucket.config.stop_strategy {
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
        let result = client
            .post(&self.endpoint)
            .headers(request.headers)
            .bearer_auth(secret.access_token.expose_secret())
            .header("chatgpt-account-id", upstream_account_id)
            .header("content-encoding", "zstd")
            .body(body)
            .send()
            .await;
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
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            transport_error: result.is_err(),
        };
        // 只在明确选择回应策略时读取有界 SSE，任何策略都不保留响应正文。
        observation.stop_reason = Some(
            match result {
                Ok(upstream) if read_output && upstream.status().is_success() => {
                    tokio::time::timeout(Duration::from_secs(30), probe::wait_for_output(upstream))
                        .await
                        .unwrap_or("body_timeout")
                }
                Ok(_) => "headers",
                Err(_) => "transport_error",
            }
            .to_owned(),
        );
        response.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        drop(client);
        self.observe(observation, response, &bucket.config).await;
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
