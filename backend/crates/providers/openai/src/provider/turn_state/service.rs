//! 令牌只在准确的账号/模型桶内流转；凭据读取不调用任何刷新入口。
//!
//! 同一账号的打票请求由账号闸串行化：排队期间同键已完成的请求直接复用结果，
//! 任务预算与死线在账号会话内跨模型共享。

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use chrono::Utc;
use gateway_core::account::{
    CloudMintConfig, CloudMintStrategy, OutboundProxy, ProviderAccount, ProviderAccountId,
    ProviderAccountStore, RoutingCookie, RoutingCookieObservation, TurnStateBucket,
    TurnStateConfig, TurnStateObservation, TurnStateStopStrategy, TurnStateToken,
};
use secrecy::ExposeSecret;
use tokio::sync::Mutex as AsyncMutex;

use crate::credential::{CodexCredentialRepository, parse_access_token_expiration};
use crate::transport::{
    CodexWebSocketPool, TurnStateObserver, TurnStateResponse, profile::CodexWireProfileState,
    tls::build_reqwest_client_with_custom_ca,
};

use super::{cloud, probe};

pub(crate) struct TurnStateService {
    pub(super) store: Arc<dyn ProviderAccountStore>,
    repository: CodexCredentialRepository,
    profile: CodexWireProfileState,
    pub(super) endpoint: String,
    pub(super) pool: Arc<CodexWebSocketPool>,
    /// 账号级串行闸：同账号的打票请求合并/排队，互不并发裸打。
    gates: Mutex<HashMap<String, Arc<AccountGate>>>,
    /// 云端端点的连续失败与冷却；键为端点稳定身份（name+url+key_env）。
    endpoints: Mutex<HashMap<String, EndpointState>>,
    cloud_rr: AtomicU64,
}

/// 打票结果：`Miss` 携带 HTTP 状态供账号任务分类重试；`None` 表示传输/本地验收未成形。
#[derive(Clone, Copy)]
pub(super) enum ProbeOutcome {
    Installed,
    Miss(Option<u16>),
    Skipped,
}

/// 一次账号任务跨多个模型桶共享的探测会话：预算、死线、种子 pair 与中止标记。
/// 全部用原子量与短临界区互斥量，保证 `&ProbeSession` 穿越 `await` 仍是 Send。
pub(super) struct ProbeSession {
    pub(super) deadline: Instant,
    budget: AtomicU32,
    /// 键为 `unified-N` 标签，条目随凭据修订打标；种子表挂在账号闸上，
    /// 同一账号并发的多个会话共享，凭据换版后旧修订的种子不再命中。
    seeds: Arc<Mutex<BTreeMap<String, (u64, RoutingCookie)>>>,
    credential_revision: AtomicU64,
    /// 401/403 说明账号凭据已不可用，会话内后续模型直接中止。
    aborted: AtomicBool,
}

impl ProbeSession {
    pub(super) fn budget_left(&self) -> bool {
        self.budget.load(Ordering::Relaxed) > 0
            && !self.aborted.load(Ordering::Relaxed)
            && Instant::now() < self.deadline
    }

    fn take_attempt(&self) -> bool {
        if !self.budget_left() {
            return false;
        }
        self.budget
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |budget| {
                budget.checked_sub(1)
            })
            .is_ok()
    }

    pub(super) fn abort(&self) {
        self.aborted.store(true, Ordering::Relaxed);
    }

    pub(super) fn aborted(&self) -> bool {
        self.aborted.load(Ordering::Relaxed)
    }

    /// 挑选种子 pair：同修订、路由有效、桶白名单允许、网关与目标一致；模型不查，跨模型复用。
    fn seed_for(
        &self,
        target: Option<&str>,
        config: &TurnStateConfig,
        now: i64,
    ) -> Option<RoutingCookie> {
        let revision = self.credential_revision.load(Ordering::Relaxed);
        self.seeds
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .find(|(stamped, pair)| {
                *stamped == revision
                    && pair.is_route_valid(now)
                    && config.allows_cookie_gateway(&pair.pod)
                    && target.is_none_or(|target| pair.gateway_label() == target)
            })
            .map(|(_, pair)| pair.clone())
    }

    fn remember(&self, pair: RoutingCookie) {
        let now = Utc::now().timestamp();
        if pair.is_route_valid(now) {
            let revision = self.credential_revision.load(Ordering::Relaxed);
            self.seeds
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(pair.gateway_label(), (revision, pair));
        }
    }

    /// 凭据修订变化时旧修订的种子即失效；顺手清扫表内不再命中的条目。
    fn swap_credential_revision(&self, revision: u64) {
        if self.credential_revision.swap(revision, Ordering::Relaxed) != revision {
            self.seeds
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retain(|_, (stamped, _)| *stamped == revision);
        }
    }

    pub(super) fn exhausted(&self) -> bool {
        !self.budget_left()
    }
}

/// 打票去重键：同一账号内同模型、同网关、同凭据修订的并发请求只放行一个。
/// 裸打与拿到 pair 后的定向请求用不同键，但同键等待方总能拿到精确结果。
#[derive(Clone, PartialEq, Eq, Hash)]
struct AttemptKey {
    model: String,
    gateway: String,
    revision: u64,
}

/// 已完成尝试的精确结果随键登记，等待方拿到的就是排队那次的结果。
#[derive(Default)]
struct GateState {
    completed: HashMap<AttemptKey, (u64, ProbeOutcome)>,
}

/// 账号级闸：`busy` 串行化打票，`generation` 让等待方识别排队期间已完成的同键请求；
/// `seeds` 让同账号并发的多个会话共享可用种子 pair，杜绝各模型重复裸打。
struct AccountGate {
    generation: AtomicU64,
    busy: AsyncMutex<GateState>,
    seeds: Arc<Mutex<BTreeMap<String, (u64, RoutingCookie)>>>,
}

impl AccountGate {
    fn snapshot(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn finished(&self, state: &mut GateState, key: AttemptKey, outcome: ProbeOutcome) {
        // 结果表按硬上限截断：键随模型/网关/凭据修订漂移，不留无界历史。
        if state.completed.len() >= 512 {
            state.completed.clear();
        }
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        state.completed.insert(key, (generation, outcome));
    }

    fn completed_after(
        &self,
        state: &GateState,
        key: &AttemptKey,
        before: u64,
    ) -> Option<ProbeOutcome> {
        state
            .completed
            .get(key)
            .filter(|(done, _)| *done > before)
            .map(|(_, outcome)| *outcome)
    }

    /// 已判失效的 pair 从共享种子表摘除，下一轮不再拿它定向打票；
    /// 按 pair 全字段精确匹配，不误伤同 pod 上更新的凭据。
    fn drop_seed(&self, sent: &RoutingCookie) {
        self.seeds
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|_, (_, pair)| !pair.same_pair(sent));
    }
}

/// 云端端点的内存失败计数；进程重启后冷却清零是可接受的。
struct EndpointState {
    failures: u32,
    cooling_until: Option<Instant>,
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
            gates: Mutex::new(HashMap::new()),
            endpoints: Mutex::new(HashMap::new()),
            cloud_rr: AtomicU64::new(0),
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

    /// 本轮账号任务的共享会话：预算与死线取各桶配置和硬上限的较小值；
    /// 种子 pair 挂在账号闸上，并发会话之间可以复用。
    pub(super) fn new_session(
        &self,
        account_id: &ProviderAccountId,
        buckets: &[TurnStateBucket],
    ) -> ProbeSession {
        let budget = buckets
            .iter()
            .map(|bucket| bucket.config.task_budget())
            .min()
            .unwrap_or(24);
        let timeout = buckets
            .iter()
            .map(|bucket| bucket.config.task_timeout())
            .min()
            .unwrap_or(Duration::from_secs(75));
        ProbeSession {
            deadline: Instant::now() + timeout,
            budget: AtomicU32::new(budget),
            seeds: self.account_gate(account_id.as_str()).seeds.clone(),
            credential_revision: AtomicU64::new(0),
            aborted: AtomicBool::new(false),
        }
    }

    /// 观测判定已发 pair 失效时，同步摘除账号闸上的共享种子。
    pub(super) fn drop_pair_seed(&self, account_id: &str, sent: &RoutingCookie) {
        self.account_gate(account_id).drop_seed(sent);
    }

    fn account_gate(&self, account_id: &str) -> Arc<AccountGate> {
        let mut gates = self.gates.lock().unwrap_or_else(|error| error.into_inner());
        // 账号闸表按硬上限截断：只剩表自身引用（不在飞行中）的旧闸可以清掉。
        if gates.len() >= 4096 && !gates.contains_key(account_id) {
            gates.retain(|_, gate| Arc::strong_count(gate) > 1);
        }
        gates
            .entry(account_id.to_owned())
            .or_insert_with(|| {
                Arc::new(AccountGate {
                    generation: AtomicU64::new(0),
                    busy: AsyncMutex::new(GateState::default()),
                    seeds: Arc::new(Mutex::new(BTreeMap::new())),
                })
            })
            .clone()
    }

    /// 端点冷却判断：冷却中不可选；冷却到期后清零重试。
    fn endpoint_available(&self, endpoint: &CloudMintConfig) -> bool {
        let mut states = self
            .endpoints
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(state) = states.get_mut(endpoint.endpoint_key().as_str()) else {
            return true;
        };
        if state
            .cooling_until
            .is_some_and(|until| Instant::now() < until)
        {
            return false;
        }
        if state.cooling_until.is_some() {
            state.cooling_until = None;
            state.failures = 0;
        }
        true
    }

    /// 成功清零、失败累加；连续失败到阈值进入冷却。端点表同样按硬上限截断。
    fn record_endpoint(&self, endpoint: &CloudMintConfig, config: &TurnStateConfig, ok: bool) {
        let mut states = self
            .endpoints
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // 端点身份随配置漂移，表满时丢弃已结束冷却的旧身份。
        if states.len() >= 64 && !states.contains_key(endpoint.endpoint_key().as_str()) {
            states.retain(|_, state| {
                state
                    .cooling_until
                    .is_some_and(|until| Instant::now() < until)
            });
        }
        let state = states
            .entry(endpoint.endpoint_key())
            .or_insert(EndpointState {
                failures: 0,
                cooling_until: None,
            });
        if ok {
            state.failures = 0;
            state.cooling_until = None;
        } else {
            state.failures += 1;
            if state.failures >= config.cloud_failure_threshold {
                state.cooling_until =
                    Some(Instant::now() + Duration::from_secs(config.cloud_cooldown_seconds));
            }
        }
    }

    /// 端点选择：随机策略在可用子集内取随机下标，轮询用原子计数取模。
    fn pick_endpoint<'a>(
        &self,
        config: &TurnStateConfig,
        eligible: &[&'a CloudMintConfig],
    ) -> &'a CloudMintConfig {
        match config.strategy {
            CloudMintStrategy::Random => eligible[probe::random_index(eligible.len())],
            CloudMintStrategy::RoundRobin => {
                let index = self.cloud_rr.fetch_add(1, Ordering::Relaxed);
                eligible[index as usize % eligible.len()]
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
        sent_cookie: Option<RoutingCookie>,
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
            let sent_cookie = sent_cookie.clone();
            Box::pin(async move {
                let bucket = service.current(&account_id, &model).await;
                let latest_issued_at = bucket.as_ref().and_then(|bucket| {
                    bucket
                        .current_issued_at
                        .max(bucket.candidate.as_ref().map(|token| token.issued_at))
                });
                let config = bucket.map(|bucket| bucket.config).unwrap_or_default();
                // 纯 Cookie 锁定才跳过被动观测；绑定 pair 的桶（声明模型/云端打票）
                // 在关闭自动打票时仍可能注入手动安装的票，观测不能丢。
                if config.cookie_lock_enabled && !config.enabled && !config.requires_route_pair() {
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
                    expires_at: None,
                    endpoint: None,
                    reported_model: None,
                    oailb_host: None,
                    cookie_issued_at: None,
                    cookie_expires_at: None,
                    cookie_origin: None,
                    cookie_name: None,
                    cookie_value: None,
                    cookie_cflb_name: None,
                    cookie_cflb_value: None,
                    pair: None,
                    has_cookie: false,
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
                        sent_cookie.as_ref(),
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
        sent_cookie: Option<&RoutingCookie>,
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
            "invalid_length"
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
        } else if observation.answer_match == Some(false) {
            "answer_mismatch"
        } else if config.requires_route_pair() {
            // 声明模型/云端桶不接受纯报头候选：候选必须来自打过完整 pair 的探测。
            "headers_only"
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
        // pair 绑定模式（声明模型/云端打票）的作废只认首个 response.created，
        // 由 observe_business_cookie 负责；这里只保留报头权威的传统路径，
        // 避免头块声明先于 created 事件误作废票。
        if config.detect_actual_model
            && !config.requires_route_pair()
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
                    sent_cookie,
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

    /// 主动打票。账号闸内先复用同键结果与已装票，再决定走云端还是本地出口。
    pub(super) async fn probe(
        &self,
        bucket: &TurnStateBucket,
        account_id: &ProviderAccountId,
        manual: bool,
        session: &ProbeSession,
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
            expires_at: None,
            endpoint: None,
            reported_model: None,
            oailb_host: None,
            cookie_issued_at: None,
            cookie_expires_at: None,
            cookie_origin: None,
            cookie_name: None,
            cookie_value: None,
            cookie_cflb_name: None,
            cookie_cflb_value: None,
            pair: None,
            has_cookie: false,
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
        if session.aborted() {
            return self.skip(observation, "session_aborted").await;
        }
        let gate = self.account_gate(account_id.as_str());
        // 拿锁前快照代次：排队期间同键已完成的请求直接复用其精确结果。
        let before = gate.snapshot();
        let mut state = gate.busy.lock().await;
        // 等待期间可能已装好或凭据翻新，桶与账号都按持锁后的最新状态走。
        let bucket = self
            .current(account_id, &bucket.model)
            .await
            .unwrap_or_else(|| bucket.clone());
        let bucket = &bucket;
        let now = Utc::now().timestamp();
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
        let revision = account.revision().get();
        session.swap_credential_revision(revision);
        // 未到期不主动重打；缓存放身份与开关校验之后，且桶必须归属刚加载的
        // 当前账号，避免账号换绑后旧身份桶的票被当成缓存命中。手动打票跳过。
        if !manual
            && bucket.matches_account(account, &bucket.model)
            && bucket.installed_token(now).is_some()
        {
            return ProbeOutcome::Installed;
        }
        // 云端端点优先：存在可用端点时本轮不打本地出口；全部冷却才回落本地。
        let endpoint = (!bucket.config.cloud_mints.is_empty())
            .then(|| {
                bucket
                    .config
                    .cloud_mints
                    .iter()
                    .filter(|endpoint| self.endpoint_available(endpoint))
                    .collect::<Vec<_>>()
            })
            .and_then(|eligible| {
                (!eligible.is_empty()).then(|| self.pick_endpoint(&bucket.config, &eligible))
            });
        let target = endpoint.and_then(CloudMintConfig::gateway_target);
        // 需 pair 的模式（声明模型或配置了云端端点）用路由 pair 做种子；
        // 端点全部冷却回落本地时同一条路径仍生效；旧 Cookie 锁定沿用临期续约语义。
        // installed_pair 是路由凭据而非账号认证凭据，跨凭据修订仍可作种子：
        // 实际请求始终用上面刚加载的当前凭据，只有会话内共享种子表按修订隔离。
        let seed = if bucket.config.requires_route_pair() {
            bucket
                .seed_pair(now)
                .filter(|pair| {
                    target
                        .as_deref()
                        .is_none_or(|target| pair.gateway_label() == target)
                        && bucket.config.allows_cookie_gateway(&pair.pod)
                })
                .or_else(|| session.seed_for(target.as_deref(), &bucket.config, now))
        } else if bucket.config.cookie_lock_enabled {
            bucket.renewal_cookie(now)
        } else {
            None
        };
        // 键里的网关只取端点配置的目标；裸打与拿到种子后的定向请求共用同一键，
        // 排队方不会因首轮打到了 pair 而换键重发。
        let gateway_key = target.unwrap_or_else(|| "any".to_owned());
        let key = AttemptKey {
            model: bucket.model.clone(),
            gateway: gateway_key,
            revision,
        };
        if let Some(done) = gate.completed_after(&state, &key, before) {
            return done;
        }
        if !session.take_attempt() {
            return self.skip(observation, "budget_exhausted").await;
        }
        let access_token = secret.access_token.expose_secret().to_owned();
        let upstream_account_id = upstream_account_id.to_owned();
        let outcome = match endpoint {
            Some(endpoint) => {
                self.cloud_attempt(
                    observation,
                    bucket,
                    account,
                    &access_token,
                    &upstream_account_id,
                    endpoint,
                    seed.as_ref(),
                    session,
                )
                .await
            }
            None => {
                self.local_attempt(
                    observation,
                    bucket,
                    account,
                    &access_token,
                    &upstream_account_id,
                    seed,
                    session,
                )
                .await
            }
        };
        gate.finished(&mut state, key, outcome);
        outcome
    }

    /// 云端打票：失败一律记入端点失败计数；成功票与 pair 通过候选槽原子安装。
    #[allow(clippy::too_many_arguments)]
    async fn cloud_attempt(
        &self,
        mut observation: TurnStateObservation,
        bucket: &TurnStateBucket,
        account: &ProviderAccount,
        access_token: &str,
        upstream_account_id: &str,
        endpoint: &CloudMintConfig,
        seed: Option<&RoutingCookie>,
        session: &ProbeSession,
    ) -> ProbeOutcome {
        observation.egress = endpoint.name.clone();
        observation.endpoint = Some(endpoint.name.clone());
        observation.source = "cloud_mint".to_owned();
        // 请求发起毫秒水位：pair 归属与在途旧响应隔离都以它为准，不能用打完时刻。
        let request_started_at = Utc::now().timestamp_millis();
        observation.started_at = Some(request_started_at / 1000);
        let result = cloud::mint(
            endpoint,
            &bucket.config,
            &bucket.model,
            access_token,
            upstream_account_id,
            seed,
            &self.endpoint,
            session.deadline,
        )
        .await;
        observation.observed_at = Utc::now().timestamp();
        observation.elapsed_ms = result.elapsed_ms;
        observation.http_status = result.status;
        observation.stop_reason = Some("cloud_mint".to_owned());
        // 可摘录的响应事实随失败观测留存，正文与凭据不落库。
        observation.token_length = result.facts.token_length;
        observation.issued_at = result.facts.issued_at;
        observation.reported_model = result.facts.served_model.clone();
        if matches!(result.status, Some(401 | 403)) {
            // 账号凭据被拒说明整轮都不可用，不再消耗剩余预算。
            session.abort();
        }
        if result.outcome != "candidate" {
            self.record_endpoint(endpoint, &bucket.config, false);
            observation.outcome = result.outcome.to_owned();
            self.persist(observation, None).await;
            return ProbeOutcome::Miss(result.status);
        }
        self.record_endpoint(endpoint, &bucket.config, true);
        let (Some(token), Some(mut pair)) = (result.token, result.pair) else {
            self.record_endpoint(endpoint, &bucket.config, false);
            observation.outcome = "invalid_response".to_owned();
            self.persist(observation, None).await;
            return ProbeOutcome::Miss(result.status);
        };
        // 桶候选与共享池登记同一份 pair：归属水位取请求发起时刻，
        // 声明模型取本地验收过的 served_model（与请求模型等价）。
        pair.observed_at = request_started_at;
        pair.reported_model = result
            .facts
            .served_model
            .clone()
            .unwrap_or_else(|| bucket.model.clone());
        let stored = pair.clone();
        let _ = self
            .store
            .observe_routing_cookie(RoutingCookieObservation {
                origin: self.endpoint.clone(),
                observed_at: stored.observed_at,
                sent: seed.cloned(),
                received: Some(stored),
                reported_model: Some(bucket.model.clone()),
                deleted: false,
            })
            .await;
        session.remember(pair.clone());
        observation.token_length = Some(token.value.len());
        observation.issued_at = Some(token.issued_at);
        observation.expires_at = result.expires_at;
        observation.reported_model = Some(bucket.model.clone());
        observation.oailb_host = Some(pair.pod.clone());
        observation.cookie_issued_at = Some(pair.issued_at);
        observation.cookie_expires_at = Some(pair.expires_at);
        observation.cookie_origin = Some(pair.origin.clone());
        observation.cookie_name = Some(pair.name.clone());
        observation.cookie_value = Some(pair.value.clone());
        observation.cookie_cflb_name = (!pair.cflb_name.is_empty()).then(|| pair.cflb_name.clone());
        observation.cookie_cflb_value =
            (!pair.cflb_value.is_empty()).then(|| pair.cflb_value.clone());
        observation.pair = Some(pair);
        observation.token = Some(token.value.clone());
        observation.outcome = "candidate".to_owned();
        self.persist(observation, Some(token)).await;
        if self.install(account.id(), &bucket.model).await {
            ProbeOutcome::Installed
        } else {
            ProbeOutcome::Miss(result.status)
        }
    }

    /// 本地探测出口：声明模型/旧 Cookie 锁定/报头验收共用一次有界出站。
    /// `sent_cookie` 由调用方按模式选好：声明模型注入种子 pair，旧锁定注入续约 pair。
    #[allow(clippy::too_many_arguments)]
    async fn local_attempt(
        &self,
        mut observation: TurnStateObservation,
        bucket: &TurnStateBucket,
        account: &ProviderAccount,
        access_token: &str,
        upstream_account_id: &str,
        sent_cookie: Option<RoutingCookie>,
        session: &ProbeSession,
    ) -> ProbeOutcome {
        // 配置了云端打票的桶在全部端点冷却时回退本地探测；pair 绑定要求这里
        // 也按首个 created + 完整 pair 验收，即使 stop_strategy 仍是默认 Headers。
        let declared = bucket.config.requires_route_pair();
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
        // 声明模型模式读到首个 created 即停；题库期望只对旧模式生效。
        let read_output = declared
            || bucket.config.cookie_lock_enabled
            || match bucket.config.stop_strategy {
                TurnStateStopStrategy::Headers => false,
                TurnStateStopStrategy::FirstOutput => true,
                TurnStateStopStrategy::Mixed => probe::random_index(2) == 1,
                TurnStateStopStrategy::DeclaredModel => true,
            };
        observation.stop_mode = Some(
            if declared {
                "declared_model"
            } else if read_output {
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
            .timeout(
                Duration::from_secs(30).min(session.deadline.saturating_duration_since(started)),
            )
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
        let request_started_at = Utc::now().timestamp_millis();
        let mut outbound = client
            .post(&self.endpoint)
            .headers(request.headers)
            .bearer_auth(access_token)
            .header("chatgpt-account-id", upstream_account_id)
            .header("content-encoding", "zstd")
            .body(body);
        if let Some(cookie) = &sent_cookie {
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
        let mut created_id = None;
        let mut created_seen = false;
        let mut created_event_ok = false;
        observation.stop_reason = Some(
            match result {
                Ok(upstream) if read_output && upstream.status().is_success() => {
                    match tokio::time::timeout_at(
                        session.deadline.into(),
                        probe::wait_for_output(
                            upstream,
                            declared || bucket.config.cookie_lock_enabled,
                            (!declared).then_some(request.expect),
                        ),
                    )
                    .await
                    {
                        Ok(output) => {
                            created_seen = output.created_seen;
                            created_event_ok = output.created_event_ok;
                            created_id = output.created_id;
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
        // 401/403 说明凭据不可用：中止整轮账号任务，剩余预算不再消耗。
        if matches!(response.status, Some(401 | 403)) {
            session.abort();
        }
        if declared {
            return self
                .grade_declared(
                    observation,
                    bucket,
                    account,
                    &response,
                    &cookie_headers,
                    sent_cookie.as_ref(),
                    created_seen,
                    created_event_ok,
                    created_id.as_deref(),
                    created_model.as_deref(),
                    session,
                    request_started_at,
                )
                .await;
        }
        if bucket.config.cookie_lock_enabled {
            let successful = response
                .status
                .is_some_and(|status| (200..300).contains(&status));
            // 探测题的期望答案必须命中，Cookie 才能进入可回放池。
            let answer_matched = observation.answer_match == Some(true);
            let (cookie, deleted) = if successful {
                // 期望答案未命中或模型脱离时，响应产出的 pair 不进入可回放池。
                let suppress = !answer_matched
                    || !created_model
                        .as_deref()
                        .is_some_and(|model| model.eq_ignore_ascii_case(&bucket.model));
                self.observe_cookie(
                    account.id(),
                    &cookie_headers,
                    sent_cookie.as_ref(),
                    created_model.as_deref(),
                    request_started_at,
                    suppress,
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
            observation.cookie_origin = cookie.as_ref().map(|cookie| cookie.origin.clone());
            observation.cookie_name = cookie.as_ref().map(|cookie| cookie.name.clone());
            observation.cookie_value = cookie.as_ref().map(|cookie| cookie.value.clone());
            observation.cookie_cflb_name = cookie.as_ref().and_then(|cookie| {
                (!cookie.cflb_name.is_empty()).then(|| cookie.cflb_name.clone())
            });
            observation.cookie_cflb_value = cookie.as_ref().and_then(|cookie| {
                (!cookie.cflb_value.is_empty()).then(|| cookie.cflb_value.clone())
            });
            let usable = answer_matched
                && !deleted
                && cookie
                    .as_ref()
                    .is_some_and(|cookie| cookie.is_usable(&bucket.model, Utc::now().timestamp()))
                && cookie
                    .as_ref()
                    .is_some_and(|cookie| bucket.config.allows_cookie_gateway(&cookie.pod))
                && created_model
                    .as_deref()
                    .is_some_and(|model| model.eq_ignore_ascii_case(&bucket.model));
            observation.outcome = if response.transport_error {
                "transport_error"
            } else if matches!(response.status, Some(401 | 403)) {
                "account_ended"
            } else if !successful {
                "http_error"
            } else if deleted {
                "cookie_deleted"
            } else if !answer_matched {
                "answer_mismatch"
            } else if created_model.is_none() {
                "missing_model"
            } else if cookie.is_none() {
                "missing_cookie"
            } else if cookie
                .as_ref()
                .is_some_and(|cookie| !bucket.config.allows_cookie_gateway(&cookie.pod))
            {
                "cookie_gateway_filtered"
            } else if usable {
                "cookie_ready"
            } else {
                "cookie_model_mismatch"
            }
            .to_owned();
            observation.reported_model = created_model;
            if let Some(cookie) = cookie.as_ref().filter(|_| usable) {
                session.remember(cookie.clone());
            }
            // Cookie 结果单独判定；同一响应里的 state 只留在记录上，不进入候选或安装。
            if let Some(raw) = response
                .value
                .as_deref()
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
            {
                observation.token_length = Some(raw.len());
                if let Some(token) = TurnStateToken::parse(raw) {
                    observation.issued_at = Some(token.issued_at);
                    if token.is_fresh(observation.observed_at, bucket.config.ttl_seconds) {
                        observation.token = Some(token.value);
                    }
                }
            }
            self.persist(observation, None).await;
            return if usable {
                ProbeOutcome::Installed
            } else {
                ProbeOutcome::Miss(response.status)
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
            sent_cookie.as_ref(),
            latest_issued_at,
        )
        .await;
        if self.install(account.id(), &bucket.model).await {
            ProbeOutcome::Installed
        } else {
            ProbeOutcome::Miss(response.status)
        }
    }

    /// 声明模型验收：首个 `response.created` 是权威模型；票与完整 pair 都合格才落候选。
    /// 验收失败也照常记录响应事实（模型、票长、签发时间），不留空观测。
    #[allow(clippy::too_many_arguments)]
    async fn grade_declared(
        &self,
        mut observation: TurnStateObservation,
        bucket: &TurnStateBucket,
        account: &ProviderAccount,
        response: &TurnStateResponse,
        cookie_headers: &[String],
        sent_cookie: Option<&RoutingCookie>,
        created_seen: bool,
        created_event_ok: bool,
        created_id: Option<&str>,
        created_model: Option<&str>,
        session: &ProbeSession,
        request_started_at: i64,
    ) -> ProbeOutcome {
        let successful = response
            .status
            .is_some_and(|status| (200..300).contains(&status));
        // created 缺失/非法/脱离请求模型的响应，产出的 pair 不能登记为可用凭据；
        // 已发 pair 的失效判定（deleted）仍照常走观测提交。
        let created_usable = successful
            && created_seen
            && created_event_ok
            && created_id.is_some()
            && created_model.is_some_and(|model| model.eq_ignore_ascii_case(&bucket.model));
        let (cookie, deleted) = if successful {
            self.observe_cookie(
                account.id(),
                cookie_headers,
                sent_cookie,
                created_model,
                request_started_at,
                !created_usable,
            )
            .await
        } else {
            (None, false)
        };
        // 验收失败也照常记录响应事实：模型、票长与签发时刻先于判定填入。
        let token = response
            .value
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(TurnStateToken::parse);
        observation.http_status = response.status;
        observation.elapsed_ms = response.elapsed_ms;
        observation.response_source = Some("response_created".to_owned());
        observation.token_length = response.value.as_ref().map(Vec::len);
        observation.issued_at = token.as_ref().map(|token| token.issued_at);
        observation.reported_model = created_model
            .map(str::to_owned)
            .or_else(|| response.reported_model.clone());
        observation.oailb_host = cookie.as_ref().map(|cookie| cookie.pod.clone());
        observation.cookie_issued_at = cookie.as_ref().map(|cookie| cookie.issued_at);
        observation.cookie_expires_at = cookie.as_ref().map(|cookie| cookie.expires_at);
        observation.cookie_origin = cookie.as_ref().map(|cookie| cookie.origin.clone());
        observation.cookie_name = cookie.as_ref().map(|cookie| cookie.name.clone());
        observation.cookie_value = cookie.as_ref().map(|cookie| cookie.value.clone());
        observation.cookie_cflb_name = cookie
            .as_ref()
            .and_then(|cookie| (!cookie.cflb_name.is_empty()).then(|| cookie.cflb_name.clone()));
        observation.cookie_cflb_value = cookie
            .as_ref()
            .and_then(|cookie| (!cookie.cflb_value.is_empty()).then(|| cookie.cflb_value.clone()));
        let now = Utc::now().timestamp();
        let pair_ok = cookie
            .as_ref()
            .is_some_and(|cookie| cookie.is_route_valid(now))
            && cookie
                .as_ref()
                .is_some_and(|cookie| bucket.config.allows_cookie_gateway(&cookie.pod));
        // 首个 created 必须完整：事件名/类型、响应 ID、模型声明缺一不可。
        let outcome = if response.transport_error {
            "transport_error"
        } else if matches!(response.status, Some(401 | 403)) {
            "account_ended"
        } else if !successful {
            "http_error"
        } else if !created_seen {
            "missing_model"
        } else if !created_event_ok || created_id.is_none() {
            "invalid_created"
        } else if created_model.is_none() {
            "missing_model"
        } else if !created_model.is_some_and(|model| model.eq_ignore_ascii_case(&bucket.model)) {
            "model_mismatch"
        } else if deleted {
            "cookie_deleted"
        } else if cookie.is_none() {
            "missing_cookie"
        } else if !pair_ok {
            "cookie_gateway_filtered"
        } else {
            "pair_ready"
        }
        .to_owned();
        if outcome != "pair_ready" {
            observation.outcome = outcome;
            self.persist(observation, None).await;
            return ProbeOutcome::Miss(response.status);
        }
        let pair = cookie.expect("pair_ready implies cookie");
        session.remember(pair.clone());
        // 票与 pair 绑定落候选；后续 `install` 原子转正并顺带装 pair。
        let latest_issued_at = bucket
            .current_issued_at
            .max(bucket.candidate.as_ref().map(|token| token.issued_at));
        let (outcome, candidate) = match token {
            None if response.value.is_none() => ("missing_header", None),
            None => ("invalid_token", None),
            Some(_)
                if response
                    .value
                    .as_ref()
                    .is_some_and(|value| value.len() != bucket.config.target_length) =>
            {
                ("invalid_length", None)
            }
            Some(token) if !token.is_fresh(now, bucket.config.ttl_seconds) => {
                ("expired_or_future", None)
            }
            Some(token) if !token.is_newer_than(latest_issued_at) => ("not_newer", None),
            Some(token) => ("candidate", Some(token)),
        };
        // 有效到期取票死线与 pair 到期的较小值；安装后按这个死线轮换。
        observation.expires_at = candidate
            .as_ref()
            .map(|token| (token.issued_at + bucket.config.ttl_seconds as i64).min(pair.expires_at));
        observation.pair = candidate.as_ref().map(|_| pair);
        observation.token = candidate
            .as_ref()
            .filter(|token| token.is_fresh(now, bucket.config.ttl_seconds))
            .map(|token| token.value.clone());
        observation.outcome = outcome.to_owned();
        self.persist(observation, candidate).await;
        if outcome == "candidate" && self.install(account.id(), &bucket.model).await {
            ProbeOutcome::Installed
        } else {
            ProbeOutcome::Miss(response.status)
        }
    }

    async fn skip(&self, mut observation: TurnStateObservation, reason: &str) -> ProbeOutcome {
        observation.outcome = reason.to_owned();
        self.persist(observation, None).await;
        ProbeOutcome::Skipped
    }
}
