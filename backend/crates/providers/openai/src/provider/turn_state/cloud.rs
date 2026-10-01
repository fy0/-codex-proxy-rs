//! 云端打票：HTTPS 中继端点、有界响应、按稳定端点身份计失败。

use std::time::{Duration, Instant};

use chrono::Utc;
use futures::StreamExt;
use gateway_core::account::{
    CloudMintConfig, CloudMintTransport, OutboundProxy, RoutingCookie, TurnStateConfig,
    TurnStateToken,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, COOKIE, HeaderMap, HeaderValue};
use serde_json::Value;

use crate::transport::tls::build_reqwest_client_with_custom_ca;

/// 打票响应体上限：中继只回 JSON 摘要，无界读取不允许。
const MINT_BODY_LIMIT: usize = 1024 * 1024;

/// 从响应正文可安全摘录的事实；验收失败时仍随观测留存，便于定位错配来源。
#[derive(Default)]
pub(super) struct MintFacts {
    pub served_model: Option<String>,
    pub token_length: Option<usize>,
    pub issued_at: Option<i64>,
}

/// 一次云端打票的结构化结果；正文与凭据不进入 Debug/日志。
pub(super) struct CloudMintResult {
    pub status: Option<u16>,
    pub outcome: &'static str,
    pub token: Option<TurnStateToken>,
    pub pair: Option<RoutingCookie>,
    /// 有效到期：票死线、票声明到期与 pair 联合到期三者最小值。
    pub expires_at: Option<i64>,
    pub facts: MintFacts,
    pub elapsed_ms: u64,
}

impl CloudMintResult {
    fn fail(outcome: &'static str, started: Instant, status: Option<u16>) -> Self {
        Self {
            status,
            outcome,
            token: None,
            pair: None,
            expires_at: None,
            facts: MintFacts::default(),
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

fn sensitive(value: &str) -> HeaderValue {
    let mut value = HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static(""));
    value.set_sensitive(true);
    value
}

fn header_value(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// 时间戳接受 RFC3339 与整数 Unix 秒两种形状，其余一律拒绝。
fn parse_timestamp(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => chrono::DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|time| time.timestamp()),
        _ => None,
    }
}

/// 本模型票据对象；缺对象或字段不成形都算缺票。
fn ticket<'a>(body: &'a Value, model: &str) -> Option<&'a Value> {
    body.get("tickets")
        .and_then(Value::as_object)
        .and_then(|tickets| tickets.get(model))
}

/// 先摘录事实再验收：失败结果也能带上实际的 served_model/长度/签发时刻。
pub(super) fn mint_facts(body: &Value, model: &str) -> MintFacts {
    let ticket = ticket(body, model);
    MintFacts {
        served_model: ticket
            .and_then(|ticket| ticket.get("served_model"))
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
            .map(str::to_owned),
        token_length: ticket
            .and_then(|ticket| ticket.get("turn_state"))
            .and_then(Value::as_str)
            .map(str::len),
        issued_at: ticket.and_then(|ticket| parse_timestamp(ticket.get("issued_at"))),
    }
}

/// 本地验收：先验票再验 pair，任何一项不过都不写候选槽。
/// `now` 由调用方在响应解析完成后取，长途 RPC 不再拿请求发起时刻判定。
///
/// 返回 `(票据, pair, 有效到期)`；票死线（签发+取小后的 TTL）、票声明到期、
/// 顶层 pair 到期与 JWT exp 四个死线全部必填且须在 `now` 之后。
pub(super) fn validate_mint(
    body: &Value,
    endpoint: &CloudMintConfig,
    config: &TurnStateConfig,
    model: &str,
    origin: &str,
    now: i64,
) -> Result<(TurnStateToken, RoutingCookie, i64), &'static str> {
    let ticket = ticket(body, model).ok_or("missing_header")?;
    let state = ticket
        .get("turn_state")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or("missing_header")?;
    let ticket_len = ticket
        .get("ticket_len")
        .and_then(Value::as_u64)
        .ok_or("invalid_length")? as usize;
    if state.len() != endpoint.ticket_length
        || ticket_len != endpoint.ticket_length
        || endpoint.ticket_length != config.target_length
    {
        return Err("invalid_length");
    }
    let served = ticket
        .get("served_model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .ok_or("missing_model")?;
    if !served.eq_ignore_ascii_case(model) {
        return Err("model_mismatch");
    }
    let claimed_issued = parse_timestamp(ticket.get("issued_at")).ok_or("invalid_token")?;
    // 票声明的到期必填：远端不声明时无法判定死线，缺省不放宽。
    let ticket_expiry = parse_timestamp(ticket.get("expires_at")).ok_or("invalid_token")?;
    if ticket_expiry <= now {
        return Err("expired_or_future");
    }
    let token = TurnStateToken::parse(state).ok_or("invalid_token")?;
    if token.issued_at != claimed_issued {
        return Err("invalid_token");
    }
    let effective_ttl = config.effective_ttl(Some(endpoint));
    if !token.mint_fresh(now, effective_ttl) {
        return Err("expired_or_future");
    }
    let cookies = body.get("cookies").and_then(Value::as_object);
    let cflb = cookies
        .and_then(|cookies| cookies.get("__cflb"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or("missing_cookie")?;
    // `__oailb` 与别名 `__oai_lb` 都能配对，pair 里记下实际用的名字。
    let (oailb_name, oailb) = ["__oailb", "__oai_lb"]
        .into_iter()
        .find_map(|name| {
            cookies
                .and_then(|cookies| cookies.get(name))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| (name, value))
        })
        .ok_or("missing_cookie")?;
    // 顶层 expires_at 是 pair 联合到期，必填；JWT exp 仍然独立生效。
    let pair_expiry = parse_timestamp(body.get("expires_at")).ok_or("missing_cookie")?;
    if pair_expiry <= now {
        return Err("expired_or_future");
    }
    // 响应 gateway 必填，且与 JWT pod、端点目标、桶白名单归一到同一 unified-N。
    let gateway = body
        .get("gateway")
        .and_then(Value::as_str)
        .ok_or("cookie_gateway_filtered")?;
    let pair = RoutingCookie::parse(origin, oailb_name, oailb, now)
        .and_then(|cookie| cookie.with_cflb("__cflb", cflb, Some(pair_expiry)))
        .ok_or("missing_cookie")?;
    if pair.expires_at <= now {
        return Err("expired_or_future");
    }
    let pod_label = pair.gateway_label();
    if RoutingCookie::normalize_gateway_label(gateway).as_deref() != Some(pod_label.as_str()) {
        return Err("cookie_gateway_filtered");
    }
    if let Some(target) = endpoint.gateway_target()
        && target != pod_label
    {
        return Err("cookie_gateway_filtered");
    }
    if !config.allows_cookie_gateway(&pair.pod) {
        return Err("cookie_gateway_filtered");
    }
    let expires_at = (token.issued_at + effective_ttl as i64)
        .min(ticket_expiry)
        .min(pair.expires_at);
    if expires_at <= now {
        return Err("expired_or_future");
    }
    Ok((token, pair, expires_at))
}

/// 打一张云端票。失败结果只保留状态码、分类与可摘录事实，不留正文或端点细节。
#[allow(clippy::too_many_arguments)]
pub(super) async fn mint(
    endpoint: &CloudMintConfig,
    config: &TurnStateConfig,
    model: &str,
    access_token: &str,
    upstream_account_id: &str,
    seed: Option<&RoutingCookie>,
    origin: &str,
    deadline: Instant,
) -> CloudMintResult {
    let started = Instant::now();
    // key_env 只保存变量名；值缺失时该端点本轮不可用。
    let Ok(relay_key) = std::env::var(&endpoint.key_env) else {
        return CloudMintResult::fail("endpoint_unavailable", started, None);
    };
    let remaining = deadline.saturating_duration_since(started);
    let timeout = Duration::from_millis(endpoint.timeout_ms).min(remaining);
    if timeout.is_zero() || relay_key.is_empty() {
        return CloudMintResult::fail("endpoint_unavailable", started, None);
    }
    let mut builder = reqwest::Client::builder()
        .use_native_tls()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(timeout.min(Duration::from_secs(15)))
        .timeout(timeout)
        .pool_max_idle_per_host(0);
    if !endpoint.proxy_url.is_empty() {
        let proxy = OutboundProxy::parse(&endpoint.proxy_url)
            .ok()
            .and_then(|proxy| reqwest::Proxy::all(proxy.expose_url()).ok());
        let Some(proxy) = proxy else {
            return CloudMintResult::fail("endpoint_unavailable", started, None);
        };
        builder = builder.proxy(proxy);
    }
    let Ok(client) = build_reqwest_client_with_custom_ca(builder) else {
        return CloudMintResult::fail("endpoint_unavailable", started, None);
    };
    let mut headers = HeaderMap::new();
    headers.insert("x-relay-key", sensitive(&relay_key));
    let gateway = endpoint
        .gateway_target()
        .unwrap_or_else(|| "any".to_owned());
    headers.insert("x-relay-mint", header_value(&gateway));
    headers.insert("x-mint-model", header_value(model));
    headers.insert(
        "x-mint-transport",
        header_value(match endpoint.transport {
            CloudMintTransport::Sse => "sse",
            CloudMintTransport::Websocket => "websocket",
        }),
    );
    headers.insert(
        "x-mint-len",
        header_value(&endpoint.ticket_length.to_string()),
    );
    headers.insert(
        "x-mint-ttl",
        header_value(&endpoint.ttl_seconds.to_string()),
    );
    headers.insert(AUTHORIZATION, sensitive(&format!("Bearer {access_token}")));
    headers.insert("chatgpt-account-id", header_value(upstream_account_id));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let request_now = Utc::now().timestamp();
    if let Some(seed) = seed.filter(|seed| seed.is_route_valid(request_now)) {
        headers.insert(COOKIE, sensitive(&seed.header()));
    }
    let sent = client.post(&endpoint.url).headers(headers);
    let response = match tokio::time::timeout_at(deadline.into(), sent.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return CloudMintResult::fail(
                if error.is_timeout() {
                    "timeout"
                } else {
                    "transport_error"
                },
                started,
                error.status().map(|status| status.as_u16()),
            );
        }
        Err(_) => return CloudMintResult::fail("timeout", started, None),
    };
    let status = response.status().as_u16();
    if !response.status().is_success() {
        return CloudMintResult::fail("http_error", started, Some(status));
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    let mut over_limit = false;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => {
                body.extend_from_slice(&chunk);
                if body.len() > MINT_BODY_LIMIT {
                    over_limit = true;
                    break;
                }
            }
            Err(_) => return CloudMintResult::fail("transport_error", started, Some(status)),
        }
    }
    if over_limit {
        return CloudMintResult::fail("body_limit", started, Some(status));
    }
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        return CloudMintResult::fail("invalid_response", started, Some(status));
    };
    // 验收时刻取响应解析完成之后：90 秒的 RPC 不能用请求发起时刻判新鲜度。
    let now = Utc::now().timestamp();
    let facts = mint_facts(&body, model);
    match validate_mint(&body, endpoint, config, model, origin, now) {
        Ok((token, pair, expires_at)) => CloudMintResult {
            status: Some(status),
            outcome: "candidate",
            token: Some(token),
            pair: Some(pair),
            expires_at: Some(expires_at),
            facts,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        },
        Err(outcome) => CloudMintResult {
            facts,
            ..CloudMintResult::fail(outcome, started, Some(status))
        },
    }
}
