//! 官方 SDK 的权限事实：只读、按请求查询，绝不扩大原 Client Key 的账号范围。
use std::collections::HashSet;

use gateway_plugin_sdk::{
    PluginFault,
    call::data::{AccessQuery, AccountFactsQuery, VerifyClientRequest},
    client::HostClient,
};

use crate::config::Config;

const MAX_PAGES: usize = 128;

pub async fn account_ids(host: &HostClient, config: &Config) -> Result<Vec<String>, PluginFault> {
    if !config.has_image_scope() {
        return Ok(Vec::new());
    }
    let groups = config
        .image_group_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let explicit = config
        .image_account_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut ids = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen = HashSet::new();
    for _ in 0..MAX_PAGES {
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
                && (explicit.contains(account.account_id.as_str())
                    || account
                        .group_ids
                        .iter()
                        .any(|group| groups.contains(group.as_str())))
            {
                ids.push(account.account_id);
            }
        }
        cursor = page.next_cursor;
        match cursor.as_deref() {
            None => {
                ids.sort();
                ids.dedup();
                return Ok(ids);
            }
            Some(next) if !next.is_empty() && seen.insert(next.to_owned()) => {}
            _ => break,
        }
    }
    Err(gateway_plugin_sdk::PluginFault::new(
        gateway_plugin_sdk::ErrorCode::Rejected,
        "unable to read complete account scope",
    ))
}

async fn key_id(host: &HostClient, authorization: &str) -> Result<String, PluginFault> {
    let identity = host
        .verify_client(VerifyClientRequest {
            authorization: authorization.to_owned(),
        })
        .await?;
    Ok(identity.client_key_id)
}

/// 给出当前 Key 真正能路由到白名单的图片模型；兼容桥接须满足主控模型的相同账号权限。
pub async fn allowed_models(
    host: &HostClient,
    config: &Config,
    authorization: &str,
) -> Result<Vec<String>, PluginFault> {
    let client_key_id = key_id(host, authorization).await?;
    allowed_models_for_key(host, config, &client_key_id).await
}

/// 管理页面只有 Key 的 ID，不持有 Key 明文。直接向官方 ACL 查询这个 Key 的授权。
pub async fn allowed_models_for_key(
    host: &HostClient,
    config: &Config,
    client_key_id: &str,
) -> Result<Vec<String>, PluginFault> {
    let candidate_ids = account_ids(host, config).await?;
    if candidate_ids.is_empty() {
        return Ok(Vec::new());
    }
    // 图片操作的 Core 模型提示不是普通对话模型。此前以
    // upstream_model=gpt-image-* 查询普通 model_access，会将合法 Images
    // 账号错误过滤为空。正确做法是先按 Key 的账号范围查询（None），
    // 仅 Responses 桥接再检查真实驱动模型的访问范围。
    let key_scope = access_set(host, client_key_id, &candidate_ids, None).await?;
    let driver_scope = if config
        .image_models
        .iter()
        .any(|model| config.use_responses(model))
    {
        Some(access_set(host, client_key_id, &candidate_ids, Some(&config.driver)).await?)
    } else {
        None
    };
    Ok(eligible_image_models(
        config,
        &key_scope,
        driver_scope.as_ref(),
    ))
}

/// 后台任务总是使用 Responses；按本次选中的驱动模型检查同一账号交集。
pub async fn allowed_models_for_driver(
    host: &HostClient,
    config: &Config,
    client_key_id: &str,
    driver: &str,
) -> Result<Vec<String>, PluginFault> {
    let candidates = account_ids(host, config).await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let scope = access_set(host, client_key_id, &candidates, None).await?;
    if scope.is_empty() {
        return Ok(Vec::new());
    }
    let drivers = access_set(host, client_key_id, &candidates, Some(driver)).await?;
    if drivers.is_disjoint(&scope) {
        Ok(Vec::new())
    } else {
        Ok(config.image_models.clone())
    }
}

fn eligible_image_models(
    config: &Config,
    scope: &HashSet<String>,
    drivers: Option<&HashSet<String>>,
) -> Vec<String> {
    config
        .image_models
        .iter()
        .filter(|model| {
            !scope.is_empty()
                && (!config.use_responses(model)
                    || drivers.is_some_and(|allowed| !allowed.is_disjoint(scope)))
        })
        .cloned()
        .collect()
}

async fn access_set(
    host: &HostClient,
    client_key_id: &str,
    ids: &[String],
    model: Option<&str>,
) -> Result<HashSet<String>, PluginFault> {
    let facts = host
        .check_access(AccessQuery {
            client_key_id: client_key_id.to_owned(),
            account_ids: ids.to_vec(),
            upstream_model: model.map(str::to_owned),
        })
        .await?;
    Ok(facts
        .accounts
        .into_iter()
        .filter(|entry| entry.allowed)
        .map(|entry| entry.account_id)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_images_are_visible_when_key_has_matching_accounts() {
        let config = Config::from_configuration(&serde_json::json!({
            "imageModels":["gpt-image-2","gpt-image-1"],
            "imageAccountIds":["acc-1"]
        }))
        .unwrap();
        let authorized = HashSet::from(["acc-1".to_owned()]);
        let driver_unavailable = HashSet::new();
        // Native Images does not require gpt-image-* to pass ordinary model_access.
        assert_eq!(
            eligible_image_models(&config, &authorized, Some(&driver_unavailable)),
            vec!["gpt-image-2".to_owned()]
        );
        assert!(eligible_image_models(&config, &HashSet::new(), Some(&authorized)).is_empty());
    }

    #[test]
    fn response_bridge_checks_driver_access_only_for_same_accounts() {
        let config = Config::from_configuration(&serde_json::json!({
            "bridgeMode":"responses", "imageModels":["gpt-image-2"],
        }))
        .unwrap();
        let allowed = HashSet::from(["account-a".to_owned()]);
        let other_driver = HashSet::from(["account-b".to_owned()]);
        assert!(eligible_image_models(&config, &allowed, Some(&other_driver)).is_empty());
        assert_eq!(
            eligible_image_models(&config, &allowed, Some(&allowed)),
            vec!["gpt-image-2".to_owned()]
        );
    }
}
