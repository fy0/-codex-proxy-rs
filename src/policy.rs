//! 认证后图像请求意图；仅根据真实操作及工具结构登记，不信任客户端私有 Header。
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::config::Config;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::{AccessQuery, AccountFactsQuery},
        policy::{AccountScheduleDecision, AccountScheduleRequest},
    },
    client::{HostClient, TypedCall, TypedReply},
};
use serde_json::Value;

const TTL: Duration = Duration::from_secs(12 * 60);
const MAX_ENTRIES: usize = 4096;
#[derive(Clone)]
pub struct ImageIntent {
    pub client_key_id: String,
    pub image_model: String,
    at: Instant,
}

#[derive(Default)]
pub struct ImageRegistry {
    entries: Mutex<HashMap<String, ImageIntent>>,
}

impl ImageRegistry {
    pub fn mark(&self, request_id: &str, client_key_id: &str, image_model: &str) {
        if request_id.is_empty() || client_key_id.is_empty() || !Config::is_image_model(image_model)
        {
            return;
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, entry| entry.at.elapsed() < TTL);
        if entries.len() >= MAX_ENTRIES {
            entries.clear();
        }
        entries.insert(
            request_id.to_owned(),
            ImageIntent {
                client_key_id: client_key_id.to_owned(),
                image_model: image_model.to_owned(),
                at: Instant::now(),
            },
        );
    }
    pub fn get(&self, request_id: &str) -> Option<ImageIntent> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match entries.get(request_id) {
            Some(entry) if entry.at.elapsed() < TTL => Some(entry.clone()),
            _ => {
                entries.remove(request_id);
                None
            }
        }
    }
}

/// 从协议语义识别实际生图操作，避免客户端私有 header 伪造。
pub fn image_intent(operation: &str, body: &[u8]) -> Option<String> {
    if body.len() > 4 * 1024 * 1024 {
        // 大正文中若存在生图工具声明而没有内部校验标记，
        // 不应静默委托普通对话调度。保守拒绝而不是漏拦。
        let image_tool = b"\"image_generation\"";
        if operation == "generate_image"
            || body
                .windows(image_tool.len())
                .any(|window| window == image_tool)
        {
            return Some("__unverified_image_intent__".to_owned());
        }
        return None;
    }
    let payload: Value = serde_json::from_slice(body).ok()?;
    if operation == "generate_image" {
        return payload
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| Some("gpt-image-1".to_owned()));
    }
    let tools = payload.get("tools")?.as_array()?;
    tools
        .iter()
        .find(|t| t.get("type").and_then(Value::as_str) == Some("image_generation"))
        .map(|tool| {
            tool.get("model")
                .and_then(Value::as_str)
                .unwrap_or("gpt-image-2")
                .to_owned()
        })
}

async fn group_accounts(
    host: &HostClient,
    groups: &[String],
) -> Result<HashSet<String>, PluginFault> {
    if groups.is_empty() {
        return Ok(HashSet::new());
    }
    let requested = groups.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut ids = HashSet::new();
    let mut cursor = None;
    let mut seen = HashSet::new();
    for _ in 0..128 {
        let page = host
            .account_facts(AccountFactsQuery {
                provider_id: Some("openai".to_owned()),
                cursor,
                limit: 200,
            })
            .await?;
        for account in page.accounts {
            if account.enabled
                && account.provider_id == "openai"
                && account
                    .group_ids
                    .iter()
                    .any(|id| requested.contains(id.as_str()))
            {
                ids.insert(account.account_id);
            }
        }
        cursor = page.next_cursor;
        match cursor.as_deref() {
            None => return Ok(ids),
            Some(next) if !next.is_empty() && seen.insert(next.to_owned()) => {}
            _ => break,
        }
    }
    Err(PluginFault::new(
        ErrorCode::Rejected,
        "image group accounts unavailable",
    ))
}

pub async fn schedule(
    call: TypedCall<AccountScheduleRequest>,
    config: Arc<Config>,
    registry: Arc<ImageRegistry>,
) -> Result<TypedReply<AccountScheduleDecision>, PluginFault> {
    let req = &call.request;
    if req.provider != "openai" {
        return Ok(TypedReply::new(AccountScheduleDecision::Delegate));
    }
    let intent = registry.get(&req.request_id);
    // 如果因绑定或载荷协议无法登记的图像模型已进入调度，至少按白名单收窄。
    let image_model = intent
        .as_ref()
        .map(|item| item.image_model.as_str())
        .or_else(|| req.model.as_deref().filter(|m| Config::is_image_model(m)));
    let Some(model) = image_model else {
        return Ok(TypedReply::new(AccountScheduleDecision::Delegate));
    };
    if !config.enabled_image_model(model) || !config.has_image_scope() {
        return Ok(TypedReply::new(AccountScheduleDecision::Reject));
    }
    let group = group_accounts(&call.host, &config.image_group_ids).await?;
    let explicit = config
        .image_account_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let candidates = req
        .candidates
        .iter()
        .filter(|c| explicit.contains(c.account_id.as_str()) || group.contains(&c.account_id))
        .map(|c| c.account_id.clone())
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(TypedReply::new(AccountScheduleDecision::Reject));
    }
    let mut approved = candidates.iter().cloned().collect::<HashSet<_>>();
    // 对 Responses 主控模型的账号，额外要求相同 Key 对实际图片模型拥有权限。
    if let Some(intent) = intent.as_ref() {
        let check = call
            .host
            .check_access(AccessQuery {
                client_key_id: intent.client_key_id.clone(),
                account_ids: candidates,
                upstream_model: Some(model.to_owned()),
            })
            .await?;
        let permitted = check
            .accounts
            .into_iter()
            .filter(|entry| entry.allowed)
            .map(|entry| entry.account_id)
            .collect::<HashSet<_>>();
        approved.retain(|id| permitted.contains(id));
    }
    if approved.is_empty() {
        return Ok(TypedReply::new(AccountScheduleDecision::Reject));
    }
    let excluded = req
        .candidates
        .iter()
        .filter(|entry| !approved.contains(&entry.account_id))
        .map(|entry| entry.account_id.clone())
        .collect::<Vec<_>>();
    Ok(TypedReply::new(if excluded.is_empty() {
        AccountScheduleDecision::Delegate
    } else {
        AccountScheduleDecision::DelegateFiltered {
            exclude_account_ids: excluded,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_tool_and_native_images_without_secret_headers() {
        let registry = ImageRegistry::default();
        assert_eq!(
            image_intent("generate_image", br#"{"model":"gpt-image-2"}"#),
            Some("gpt-image-2".into())
        );
        assert_eq!(
            image_intent(
                "generate",
                br#"{"model":"gpt-6","tools":[{"type":"image_generation","model":"gpt-image-1"}]}"#
            ),
            Some("gpt-image-1".into())
        );
        assert_eq!(image_intent("generate", br#"{"model":"gpt-6"}"#), None);
        registry.mark("request-1", "key-1", "gpt-image-2");
        let mut large_image = Vec::with_capacity(4 * 1024 * 1024 + 64);
        large_image.extend_from_slice(br#"{"tools":[{"type":"image_generation"}],"input":""#);
        large_image.resize(4 * 1024 * 1024 + 5, b'a');
        assert_eq!(
            image_intent("generate", &large_image),
            Some("__unverified_image_intent__".to_owned())
        );
        assert_eq!(
            registry.get("request-1").unwrap().image_model,
            "gpt-image-2"
        );
    }
}
