//! 按当前 Client Key 的候选池收窄图片请求的账号选择；不构造凭据，不绕过 Core 租约。
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::AccountFactsQuery,
        policy::{AccountScheduleDecision, AccountScheduleRequest},
    },
    client::{HostClient, TypedCall, TypedReply},
};

use crate::config::Config;

const MARK_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_MARKS: usize = 4096;
const MAX_ACCOUNT_PAGES: usize = 100;

/// 只允许中间件在认证后的 request 阶段登记；客户端不能直接设置请求 ID。
#[derive(Default)]
pub struct ImageRequestRegistry {
    marks: Mutex<HashMap<String, Instant>>,
}
impl ImageRequestRegistry {
    pub fn mark(&self, request_id: &str) {
        if request_id.is_empty() {
            return;
        }
        let mut marks = self
            .marks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        marks.retain(|_, at| now.duration_since(*at) < MARK_TTL);
        if marks.len() >= MAX_MARKS {
            // 缺少明确淘汰顺序时清除过期后仍满的缓存，避免无界增长。
            marks.clear();
        }
        marks.insert(request_id.to_owned(), now);
    }

    pub fn contains(&self, request_id: &str) -> bool {
        let mut marks = self
            .marks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(at) = marks.get(request_id).copied() else {
            return false;
        };
        if at.elapsed() >= MARK_TTL {
            marks.remove(request_id);
            return false;
        }
        true
    }
}

fn policy_error() -> PluginFault {
    PluginFault::new(
        ErrorCode::Rejected,
        "image account policy cannot be verified",
    )
}

async fn group_accounts(
    host: &HostClient,
    groups: &[String],
) -> Result<HashSet<String>, PluginFault> {
    if groups.is_empty() {
        return Ok(HashSet::new());
    }
    let group_ids: HashSet<&str> = groups.iter().map(String::as_str).collect();
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut allowed = HashSet::new();
    for _ in 0..MAX_ACCOUNT_PAGES {
        let page = host
            .account_facts(AccountFactsQuery {
                provider_id: Some("openai".to_owned()),
                cursor,
                limit: 200,
            })
            .await
            .map_err(|_| policy_error())?;
        for account in page.accounts {
            if account.enabled
                && account.provider_id == "openai"
                && account
                    .group_ids
                    .iter()
                    .any(|group| group_ids.contains(group.as_str()))
            {
                allowed.insert(account.account_id);
            }
        }
        cursor = page.next_cursor;
        match cursor.as_deref() {
            None => return Ok(allowed),
            Some(next) if !next.is_empty() && seen_cursors.insert(next.to_owned()) => {}
            _ => return Err(policy_error()),
        }
    }
    // 无法遍历完整候选集时绝不使用不完整结果进行授权。
    Err(policy_error())
}

fn should_scope(req: &AccountScheduleRequest, registry: &ImageRequestRegistry) -> bool {
    req.provider == "openai"
        && (req.model.as_deref().is_some_and(Config::is_image_model)
            || registry.contains(&req.request_id))
}

fn candidate_allowed(id: &str, direct: &HashSet<&str>, group: &HashSet<String>) -> bool {
    direct.contains(id) || group.contains(id)
}

/// 仅对标记过的桥接请求及原生图片模型选号；其它模型委托原始调度。
pub async fn schedule(
    call: TypedCall<AccountScheduleRequest>,
    config: Arc<Config>,
    registry: Arc<ImageRequestRegistry>,
) -> Result<TypedReply<AccountScheduleDecision>, PluginFault> {
    let req = &call.request;
    if !should_scope(req, &registry) {
        return Ok(TypedReply::new(AccountScheduleDecision::Delegate));
    }
    if !config.has_image_scope() {
        return Ok(TypedReply::new(AccountScheduleDecision::Reject));
    }
    // 候选账号已经由 CPR 依据当前 Key、Provider、模型限制和并发冻结。
    let group_allowed = group_accounts(&call.host, &config.image_group_ids).await?;
    let allowed: HashSet<&str> = config
        .image_account_ids
        .iter()
        .map(String::as_str)
        .collect();
    let selected = req
        .candidates
        .iter()
        .filter(|candidate| candidate_allowed(&candidate.account_id, &allowed, &group_allowed))
        .min_by(|a, b| {
            // 在当前允许账号内选择负载比例较低者；相同时优先较高权重。
            let a_max = u64::from(a.maximum_concurrency.max(1));
            let b_max = u64::from(b.maximum_concurrency.max(1));
            let a_work = u64::from(a.in_flight).saturating_mul(b_max);
            let b_work = u64::from(b.in_flight).saturating_mul(a_max);
            a_work.cmp(&b_work).then_with(|| b.weight.cmp(&a.weight))
        });
    Ok(TypedReply::new(match selected {
        Some(candidate) => AccountScheduleDecision::Pick {
            account_id: candidate.account_id.clone(),
        },
        None => AccountScheduleDecision::Reject,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_selected_accounts_or_group_members_are_eligible() {
        let direct = HashSet::from(["account-B"]);
        let group = HashSet::from(["account-C".to_owned()]);
        assert!(!candidate_allowed("account-A", &direct, &group));
        assert!(candidate_allowed("account-B", &direct, &group));
        assert!(candidate_allowed("account-C", &direct, &group));
    }

    #[test]
    fn text_models_do_not_inherit_image_restrictions() {
        let registry = ImageRequestRegistry::default();
        let request = AccountScheduleRequest {
            request_id: "req-1".to_owned(),
            attempt_index: 1,
            provider: "openai".to_owned(),
            model: Some("gpt-6".to_owned()),
            candidates: Vec::new(),
        };
        assert!(!should_scope(&request, &registry));
        registry.mark("req-1");
        assert!(should_scope(&request, &registry));
        // 原生 Images 的 model_hint 可能为空；必须依赖认证后的请求标记。
        let native = AccountScheduleRequest {
            request_id: "req-1".to_owned(),
            attempt_index: 1,
            provider: "openai".to_owned(),
            model: None,
            candidates: Vec::new(),
        };
        assert!(should_scope(&native, &registry));
    }

    #[test]
    fn registry_preserves_only_recent_server_side_image_intents() {
        let registry = ImageRequestRegistry::default();
        assert!(!registry.contains("request-1"));
        registry.mark("request-1");
        assert!(registry.contains("request-1"));
        assert!(!registry.contains("request-2"));
    }
}
