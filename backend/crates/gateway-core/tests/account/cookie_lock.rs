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
    let mut cookie = RoutingCookie::parse(
        "https://chatgpt.com/backend-api/codex/responses",
        "__oailb",
        &value,
        now,
    )
    .unwrap();
    assert!(!cookie.is_usable("gpt-6-astra", now));
    cookie.reported_model = "gpt-6-astra".to_owned();
    assert!(cookie.is_usable("GPT-6-ASTRA", now));
    assert!(!cookie.is_usable("gpt-6-astra", now + 3600));
    assert!(!cookie.is_usable("gpt-5.6-luna", now));
    assert!(!format!("{cookie:?}").contains(&value));
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
    assert_eq!(cookie.status().gateway_id, "87");
}
