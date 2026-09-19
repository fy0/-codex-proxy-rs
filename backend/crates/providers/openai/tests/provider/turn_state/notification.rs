//! 飞书协议回归只向本地模拟服务器发送合成票，不验证真实机器人或真实上游。

use gateway_core::account::{TurnStateInstallation, TurnStateNotification, TurnStateToken};

use super::*;

#[tokio::test]
async fn notification_sends_installation_details_and_checks_feishu_business_status() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    let runtime = tempfile::tempdir().unwrap();
    let mut config = OpenAiConfig::default();
    config.api.base_url = server.uri();
    config.resolve_and_validate(runtime.path()).unwrap();
    let mut bundle =
        provider_openai::initialize(config, turn_state_provider_ports(Arc::clone(&store)))
            .await
            .unwrap();
    let registration = bundle
        .take_worker_contributions()
        .into_iter()
        .find_map(|item| match item {
            WorkerContribution::Registration(registration)
                if registration.id.owner() == "openai-turn-state" =>
            {
                Some(registration)
            }
            _ => None,
        })
        .unwrap();
    let WorkerRunnable::Scheduled { task, .. } = registration.runnable else {
        panic!("expected scheduled worker");
    };
    let issued = Utc::now().timestamp() - 5;
    let value = token_at(217, issued);
    for (status, code, delivered, attempts) in [
        (200, 0, true, 12),
        (200, 19024, false, 12),
        (503, 0, false, 12),
        (200, 0, true, 0),
    ] {
        Mock::given(method("POST"))
            .and(path("/feishu"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({"code": code})))
            .mount(&server)
            .await;
        store.seed_turn_notification(TurnStateNotification {
            account_id: "acct_notice".to_owned(),
            account_name: "test account".to_owned(),
            model: "gpt-5.4".to_owned(),
            webhook_url: format!("{}/feishu", server.uri()),
            token: TurnStateToken::parse(&value).unwrap(),
            installation: TurnStateInstallation {
                installed_at: issued + 5,
                issued_at: issued,
                token_length: 292,
                source: "probe".to_owned(),
                acquired_at: issued + 4,
                attempts,
                hunt_seconds: 125,
            },
            previous_installed_at: Some(issued + 5 - 2100),
            manual: true,
        });
        task.run_cycle(WorkerCycleContext::new(
            registration.id.clone(),
            None,
            CancellationToken::new(),
        ))
        .await
        .unwrap();
        assert_eq!(store.turn_notification_results().last(), Some(&delivered));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["msg_type"], "text");
        let text = body["content"]["text"].as_str().unwrap();
        for expected in [
            value.as_str(),
            "acct_notice",
            "gpt-5.4",
            "手动应用",
            "0小时35分0秒",
        ] {
            assert!(text.contains(expected));
        }
        if attempts == 0 {
            assert!(text.contains("本次获取耗时：历史未记录"));
            assert!(text.contains("尝试次数：历史未记录"));
        } else {
            assert!(text.contains("0小时2分5秒"));
            assert!(text.contains("尝试次数：12"));
        }
        task.run_cycle(WorkerCycleContext::new(
            registration.id.clone(),
            None,
            CancellationToken::new(),
        ))
        .await
        .unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        server.reset().await;
    }
}
