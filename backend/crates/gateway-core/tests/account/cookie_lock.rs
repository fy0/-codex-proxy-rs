use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_core::account::RoutingCookie;
use serde_json::json;

fn jwt(host: &str, iat: i64, exp: i64, alg: &str) -> String {
    format!(
        "{}.{}.c2ln",
        URL_SAFE_NO_PAD.encode(json!({"alg":alg}).to_string()),
        URL_SAFE_NO_PAD.encode(json!({"host":host,"iat":iat,"exp":exp}).to_string())
    )
}

#[test]
fn routing_cookie_rejects_expired_future_and_invalid_scope() {
    let now = 1_800_000_000;
    let host = "chat.gateway.unified-185.api.openai.com";
    let value = jwt(host, now, now + 3600, "ES256");
    let cookie = RoutingCookie::parse(
        "https://chatgpt.com/backend-api/codex/responses",
        "__oailb",
        &value,
        now,
    )
    .unwrap();
    // 只有 __oailb 半边的 Cookie 不是可回放 pair。
    assert!(!cookie.has_pair());
    assert!(!cookie.is_route_valid(now));
    assert!(!cookie.is_usable("gpt-6-astra", now));
    assert_eq!(cookie.header(), format!("__oailb={value}"));
    let mut cookie = cookie.with_cflb("__cflb", "synthetic-cflb", None).unwrap();
    assert!(cookie.has_pair());
    assert!(!cookie.is_usable("gpt-6-astra", now));
    cookie.reported_model = "gpt-6-astra".to_owned();
    assert!(cookie.is_usable("GPT-6-ASTRA", now));
    assert!(!cookie.is_usable("gpt-6-astra", now + 3600));
    assert!(!cookie.is_usable("gpt-5.6-luna", now));
    assert_eq!(
        cookie.header(),
        format!("__cflb=synthetic-cflb; __oailb={value}")
    );
    assert!(!format!("{cookie:?}").contains(&value));
    assert!(!format!("{cookie:?}").contains("synthetic-cflb"));
    for (host, iat, exp, alg) in [
        (host, now + 1, now + 3600, "ES256"),
        (host, now - 3600, now, "ES256"),
        ("evil.example", now, now + 3600, "ES256"),
        (host, now, now + 3600, "none"),
    ] {
        assert!(
            RoutingCookie::parse("origin", "__oailb", &jwt(host, iat, exp, alg), now).is_none()
        );
    }
    // 签发寿命由上游决定，本地不限制 JWT 声明的时长。
    assert!(
        RoutingCookie::parse(
            "origin",
            "__oailb",
            &jwt(host, now, now + 7200, "ES256"),
            now
        )
        .is_some()
    );
    assert!(RoutingCookie::parse("origin", "session-token", &value, now).is_none());
    assert!(RoutingCookie::parse("origin", "__oai_lb", &value, now).is_some());
}

#[test]
fn routing_cookie_exposes_gateway_id_and_expiry_metadata() {
    let now = 1_800_000_000;
    let cookie = RoutingCookie::parse(
        "origin",
        "__oailb",
        &jwt(
            "chat.gateway.unified-87.api.openai.com",
            now,
            now + 3600,
            "ES256",
        ),
        now,
    )
    .unwrap();
    assert_eq!(cookie.gateway_id(), "87");
    assert_eq!(cookie.expires_at, now + 3600);
    assert_eq!(cookie.oailb_expires_at, Some(now + 3600));
    assert_eq!(cookie.status().gateway_id, "87");
    // __cflb 属性寿命更短时收窄联合到期；更长时仍以 __oailb JWT exp 为准。
    let narrowed = RoutingCookie::parse(
        "origin",
        "__oailb",
        &jwt(
            "chat.gateway.unified-87.api.openai.com",
            now,
            now + 3600,
            "ES256",
        ),
        now,
    )
    .unwrap()
    .with_cflb("__cflb", "v", Some(now + 10))
    .unwrap();
    assert_eq!(narrowed.expires_at, now + 10);
    assert_eq!(narrowed.oailb_expires_at, Some(now + 3600));
    let wider = RoutingCookie::parse(
        "origin",
        "__oailb",
        &jwt(
            "chat.gateway.unified-87.api.openai.com",
            now,
            now + 3600,
            "ES256",
        ),
        now,
    )
    .unwrap()
    .with_cflb("__cflb", "v", Some(now + 7200))
    .unwrap();
    assert_eq!(wider.expires_at, now + 3600);
    // 非法 __cflb 值（CRLF/分号/逗号）拒绝拼接。
    assert!(
        RoutingCookie::parse("origin", "__oailb", &cookie.value, now)
            .unwrap()
            .with_cflb("__cflb", "a;b", None)
            .is_none()
    );
}
