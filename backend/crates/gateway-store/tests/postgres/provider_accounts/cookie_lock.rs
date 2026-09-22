use super::*;
use gateway_core::account::{RoutingCookie, RoutingCookieObservation};

fn cookie(pod: &str, origin: &str) -> RoutingCookie {
    let now = Utc::now().timestamp();
    RoutingCookie {
        origin: origin.to_owned(),
        pod: pod.to_owned(),
        name: "__oailb".to_owned(),
        value: "synthetic-cookie".to_owned(),
        issued_at: now,
        expires_at: now + 3600,
        observed_at: 0,
        reported_model: String::new(),
    }
}

fn observation(
    sent: Option<RoutingCookie>,
    received: Option<RoutingCookie>,
    time: i64,
    model: &str,
) -> RoutingCookieObservation {
    RoutingCookieObservation {
        origin: "endpoint".to_owned(),
        observed_at: time,
        sent,
        received,
        reported_model: Some(model.to_owned()),
        deleted: false,
    }
}

#[tokio::test]
async fn pool_shares_models_revokes_switched_pods_and_fences_late_responses() {
    let Some(database) = TestDatabase::create("cookie_pool").await else {
        return;
    };
    let store = PgProviderAccountRepository::new(database.pool.clone());
    let first = cookie("pod-1", "endpoint");
    store
        .observe_routing_cookie(observation(None, Some(first.clone()), 100, "gpt-6-astra"))
        .await
        .unwrap();
    assert!(
        store.routing_cookies().await.unwrap()[0].is_usable("gpt-6-astra", Utc::now().timestamp())
    );
    store
        .observe_routing_cookie(observation(Some(first.clone()), None, 300, "gpt-5.6-luna"))
        .await
        .unwrap();
    store
        .observe_routing_cookie(observation(None, Some(first.clone()), 200, "gpt-6-astra"))
        .await
        .unwrap();
    assert!(
        !store.routing_cookies().await.unwrap()[0].is_usable("gpt-6-astra", Utc::now().timestamp())
    );
    let second = cookie("pod-2", "endpoint");
    store
        .observe_routing_cookie(observation(
            Some(first.clone()),
            Some(second),
            400,
            "gpt-6-astra",
        ))
        .await
        .unwrap();
    store
        .observe_routing_cookie(observation(None, Some(first), 350, "gpt-6-astra"))
        .await
        .unwrap();
    let pool = store.routing_cookies().await.unwrap();
    assert_eq!(pool.len(), 1);
    assert_eq!(pool[0].pod, "pod-2");
    let mut deleted = observation(Some(pool[0].clone()), None, 500, "gpt-6-astra");
    deleted.deleted = true;
    store.observe_routing_cookie(deleted).await.unwrap();
    assert!(store.routing_cookies().await.unwrap().is_empty());
    database.close().await;
}
