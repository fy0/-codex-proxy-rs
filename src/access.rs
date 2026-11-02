//! 官方 SDK 的权限事实：只读、按请求查询，绝不扩大原 Client Key 的账号范围。
use std::collections::HashSet;

use gateway_plugin_sdk::{
    PluginFault,
    call::data::{AccessQuery, AccountFactsQuery, VerifyClientRequest},
    client::HostClient,
};

use crate::config::{Config, IMAGE_MODELS};

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
    let candidate_ids = account_ids(host, config).await?;
    if candidate_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut available = Vec::new();
    let driver_scope = if IMAGE_MODELS.iter().any(|model| config.use_responses(model)) {
        Some(access_set(host, &client_key_id, &candidate_ids, &config.driver).await?)
    } else {
        None
    };
    for model in IMAGE_MODELS {
        let mut allowed = access_set(host, &client_key_id, &candidate_ids, model).await?;
        if config.use_responses(model) {
            let Some(driver) = &driver_scope else {
                continue;
            };
            allowed.retain(|account| driver.contains(account));
        }
        if !allowed.is_empty() {
            available.push((*model).to_owned());
        }
    }
    Ok(available)
}

async fn access_set(
    host: &HostClient,
    client_key_id: &str,
    ids: &[String],
    model: &str,
) -> Result<HashSet<String>, PluginFault> {
    let facts = host
        .check_access(AccessQuery {
            client_key_id: client_key_id.to_owned(),
            account_ids: ids.to_vec(),
            upstream_model: Some(model.to_owned()),
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
    fn image_model_names_are_distinct() {
        assert_eq!(
            IMAGE_MODELS.iter().collect::<HashSet<_>>().len(),
            IMAGE_MODELS.len()
        );
    }
}
