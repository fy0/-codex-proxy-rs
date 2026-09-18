//! 飞书网络副作用由后台任务执行；失败日志不携带 Webhook、响应正文或 state。

use std::time::Duration;

use futures::StreamExt;
use gateway_core::{
    account::{ProviderAccountId, TurnStateNotification},
    lifecycle::CancellationToken,
};
use serde::Deserialize;

use super::TurnStateService;

#[derive(Deserialize)]
struct FeishuResponse {
    code: i64,
}

impl TurnStateService {
    pub(super) async fn notify_installations(&self, cancellation: &CancellationToken) {
        let notices = match self.store.claim_turn_state_notifications().await {
            Ok(notices) => notices,
            Err(_) => {
                tracing::warn!("turn state notification queue unavailable");
                return;
            }
        };
        if notices.is_empty() {
            return;
        }
        let client = match reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(client) => client,
            Err(_) => return,
        };
        futures::stream::iter(notices)
            .for_each_concurrent(4, |notice| {
                let client = &client;
                async move {
                    let Ok(account) = ProviderAccountId::new(notice.account_id.clone()) else {
                        return;
                    };
                    let delivered = tokio::select! {
                        () = cancellation.cancelled() => return,
                        result = send(client, &notice) => result,
                    };
                    if !delivered {
                        tracing::warn!(
                            account_id = notice.account_id,
                            model = notice.model,
                            "turn state Feishu notification failed"
                        );
                    }
                    if self
                        .store
                        .finish_turn_state_notification(
                            &account,
                            &notice.model,
                            notice.token.issued_at,
                            delivered,
                        )
                        .await
                        .is_err()
                    {
                        tracing::warn!("turn state notification completion unavailable");
                    }
                }
            })
            .await;
    }
}

async fn send(client: &reqwest::Client, notice: &TurnStateNotification) -> bool {
    let installation = &notice.installation;
    let interval = notice.previous_installed_at.map_or_else(
        || "首次安装".to_owned(),
        |previous| duration(installation.installed_at.saturating_sub(previous).max(0) as u64),
    );
    let text = format!(
        "state 安装成功\n账号：{} ({})\n模型：{}\n安装方式：{}\n获取来源：{}\n距上次安装：{}\n本次获取耗时：{}\n尝试次数：{}\n签发时间：{}\n长度：{}\n完整 state：\n{}",
        notice.account_name,
        notice.account_id,
        notice.model,
        if notice.manual {
            "手动应用"
        } else {
            "自动安装"
        },
        if installation.source == "probe" {
            "主动探测"
        } else {
            "被动采集"
        },
        interval,
        duration(installation.hunt_seconds),
        installation.attempts,
        chrono::DateTime::from_timestamp(notice.token.issued_at, 0).map_or_else(
            || notice.token.issued_at.to_string(),
            |date| date.to_rfc3339()
        ),
        installation.token_length,
        notice.token.value,
    );
    let Ok(response) = client
        .post(&notice.webhook_url)
        .json(&serde_json::json!({"msg_type": "text", "content": {"text": text}}))
        .send()
        .await
    else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    // 飞书 HTTP 200 仍可能表示业务失败，且错误正文不能进入日志。
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            return false;
        };
        if body.len() + chunk.len() > 8192 {
            return false;
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice::<FeishuResponse>(&body).is_ok_and(|response| response.code == 0)
}

fn duration(seconds: u64) -> String {
    format!(
        "{}小时{}分{}秒",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}
