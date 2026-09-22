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
fn routing_cookie_rejects_expired_future_invalid_scope_and_unbounded_lifetimes() {
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
        (host, now, now + 3601, "ES256"),
        ("evil.example", now, now + 3600, "ES256"),
        (host, now, now + 3600, "none"),
    ] {
        assert!(
            RoutingCookie::parse("origin", "__oailb", &jwt(host, iat, exp, alg), now).is_none()
        );
    }
    assert!(RoutingCookie::parse("origin", "session-token", &value, now).is_none());
    assert!(RoutingCookie::parse("origin", "__oai_lb", &value, now).is_some());
}
