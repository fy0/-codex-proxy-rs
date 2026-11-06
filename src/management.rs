//! 插件独立管理页面；密钥只通过宿主模型桥按 ID 选择，不传入前端。
use crate::config::{Config, IMAGE_MODELS, Mode};
use gateway_plugin_sdk::call::data::AccountFacts;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::AccountFactsQuery,
        host::{
            ModelListRequest, ModelListResult, StateGetRequest, StateGetResult, StatePutRequest,
            StatePutResult,
        },
        management::{
            ManagementPage, ManagementRegistration, ManagementRequest, ManagementResource,
            ManagementResponse, ManagementRoute,
        },
    },
    client::{HostClient, SessionError, TypedCall, TypedReply},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const JSON: &str = "application/json";

pub fn registration() -> ManagementRegistration {
    ManagementRegistration {
        routes: vec![
            ManagementRoute {
                method: "GET".into(),
                path: "api/bootstrap".into(),
                request_content_types: vec![],
                response_content_types: vec![JSON.into()],
            },
            ManagementRoute {
                method: "POST".into(),
                path: "api/models".into(),
                request_content_types: vec![JSON.into()],
                response_content_types: vec![JSON.into()],
            },
            ManagementRoute {
                method: "GET".into(),
                path: "api/preferences".into(),
                request_content_types: vec![],
                response_content_types: vec![JSON.into()],
            },
            ManagementRoute {
                method: "POST".into(),
                path: "api/preferences".into(),
                request_content_types: vec![JSON.into()],
                response_content_types: vec![JSON.into()],
            },
        ],
        resources: ["web/index.html", "web/app.js", "web/app.css"]
            .into_iter()
            .map(|path| ManagementResource {
                path: path.into(),
                public: false,
            })
            .collect(),
        pages: vec![ManagementPage {
            id: "image-workbench".into(),
            title: "画图测试".into(),
            description: Some("按客户端 Key 测试 Codex 图像生成与账号范围".into()),
            entry: "web/index.html".into(),
            icon: None,
        }],
        callbacks: vec![],
    }
}
fn json_response(status: u16, body: Value) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    let bytes = serde_json::to_vec(&body)
        .map_err(|_| PluginFault::new(ErrorCode::Fault, "encode management response failed"))?;
    Ok(TypedReply::new(ManagementResponse {
        status,
        content_type: JSON.into(),
        headers: vec![],
    })
    .with_payload(bytes))
}
fn bad(message: &str) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    json_response(
        400,
        json!({"error":{"code":"invalid_request","message":message}}),
    )
}
async fn model_catalog(
    host: &HostClient,
    client_key_id: String,
) -> Result<ModelListResult, PluginFault> {
    let reply = host
        .call(
            "host.models.list",
            serde_json::to_value(ModelListRequest {
                client_key_id,
                protocol: "openai".into(),
                client_version: "cpr-image-workbench/0.6.0".into(),
            })
            .map_err(|_| PluginFault::new(ErrorCode::InvalidInput, "invalid model list"))?,
            Vec::new(),
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    if !reply.payload.is_empty() {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "unexpected model-list payload",
        ));
    }
    serde_json::from_value(reply.result)
        .map_err(|_| PluginFault::new(ErrorCode::Fault, "invalid model-list response"))
}
/// 管理页专属的非秘密账号展示资料；电子邮箱用于消歧，而不是认证标识。
fn account_option(account: &AccountFacts) -> Value {
    json!({
        "id": account.account_id,
        "name": account.name,
        "email": account.email,
        "enabled": account.enabled,
        "groupIds": account.group_ids,
    })
}

/// 仅返回非秘密的账号显示信息，供 UI 勾选配置；无法列出时允许手动输入 ID。
async fn account_choices(
    host: &HostClient,
) -> Result<(Vec<Value>, Vec<String>, bool), PluginFault> {
    let mut accounts = Vec::new();
    let mut groups = BTreeSet::new();
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    for _ in 0..6 {
        let page = host
            .account_facts(AccountFactsQuery {
                provider_id: Some("openai".to_owned()),
                cursor,
                limit: 200,
            })
            .await?;
        for account in page.accounts {
            if account.provider_id != "openai" {
                continue;
            }
            groups.extend(account.group_ids.iter().cloned());
            if accounts.len() < 600 {
                accounts.push(account_option(&account));
            }
        }
        cursor = page.next_cursor;
        match cursor.as_deref() {
            None => return Ok((accounts, groups.into_iter().collect(), false)),
            Some(next) if !next.is_empty() && seen.insert(next.to_owned()) => {}
            _ => {
                return Err(PluginFault::new(
                    ErrorCode::Rejected,
                    "account options pagination is invalid",
                ));
            }
        }
    }
    Ok((accounts, groups.into_iter().collect(), true))
}

const PREFERENCE_NAMESPACE: &str = "image-workbench";
const PREFERENCE_KEY: &str = "last-selection";

async fn preference_read(host: &HostClient) -> Result<(Option<Value>, Option<u64>), PluginFault> {
    let reply = host
        .call(
            "host.state.get",
            serde_json::to_value(StateGetRequest {
                namespace: PREFERENCE_NAMESPACE.to_owned(),
                key: PREFERENCE_KEY.to_owned(),
            })
            .map_err(|_| PluginFault::new(ErrorCode::Fault, "encode preference get failed"))?,
            Vec::new(),
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    let response: StateGetResult = serde_json::from_value(reply.result)
        .map_err(|_| PluginFault::new(ErrorCode::Fault, "invalid preference state"))?;
    Ok(match response.record {
        Some(record) => (Some(record.value), Some(record.version)),
        None => (None, None),
    })
}

fn is_valid_preference(value: &Value) -> bool {
    let Some(fields) = value.as_object() else {
        return false;
    };
    if fields.len() != 7 {
        return false;
    }
    let text = |name: &str, max: usize| {
        fields
            .get(name)
            .and_then(Value::as_str)
            .is_some_and(|value| value.len() <= max && !value.chars().any(char::is_control))
    };
    if !text("clientKeyId", 256)
        || !text("driverModel", 128)
        || !text("imageModel", 128)
        || !text("size", 32)
    {
        return false;
    }
    let quality = fields.get("quality").and_then(Value::as_str);
    if !matches!(
        quality,
        Some("auto" | "low" | "medium" | "high" | "xhigh" | "max")
    ) {
        return false;
    }
    let count = fields.get("count").and_then(Value::as_u64);
    let parallelism = fields.get("parallelism").and_then(Value::as_u64);
    if !count.is_some_and(|v| (1..=10).contains(&v))
        || !parallelism.is_some_and(|v| (1..=3).contains(&v))
    {
        return false;
    }
    let size = fields.get("size").and_then(Value::as_str).unwrap_or("");
    // 上游当前 4K 限制，拒绝任意字符串绕过尺寸白名单。
    matches!(
        size,
        "auto"
            | "1024x1024"
            | "1536x1024"
            | "1024x1536"
            | "2048x2048"
            | "2048x1152"
            | "1152x2048"
            | "3840x2160"
            | "2160x3840"
    )
}

async fn preference_write(
    host: &HostClient,
    value: Value,
    expected_version: Option<u64>,
) -> Result<u64, PluginFault> {
    let reply = host
        .call(
            "host.state.put",
            serde_json::to_value(StatePutRequest {
                namespace: PREFERENCE_NAMESPACE.to_owned(),
                key: PREFERENCE_KEY.to_owned(),
                value,
                expected_version,
            })
            .map_err(|_| PluginFault::new(ErrorCode::Fault, "encode preference save failed"))?,
            Vec::new(),
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    let response: StatePutResult = serde_json::from_value(reply.result)
        .map_err(|_| PluginFault::new(ErrorCode::Fault, "invalid preference save response"))?;
    Ok(response.version)
}

pub async fn handle(
    call: TypedCall<ManagementRequest>,
    config: &Config,
) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    if !call.request.query.is_empty() {
        return bad("查询参数不受支持");
    }
    if call.payload.len() > 8 * 1024 {
        return bad("请求体过大");
    }
    match (call.request.method.as_str(), call.request.path.as_str()) {
        ("GET", "api/bootstrap") => {
            if !call.payload.is_empty() {
                return bad("GET 不允许正文");
            }
            let result = call
                .host
                .list_keys(gateway_plugin_sdk::call::host::KeyListRequest {
                    cursor: None,
                    limit: 200,
                })
                .await?;
            let (accounts, groups, account_options_truncated, account_options_error) =
                match account_choices(&call.host).await {
                    Ok((accounts, groups, truncated)) => (accounts, groups, truncated, false),
                    Err(_) => (Vec::new(), Vec::new(), false, true),
                };
            json_response(
                200,
                json!({
                    "keys": result.keys,
                    "keysNextCursor": result.next_cursor,
                    "driverModel": config.driver,
                    "imageModels": config.image_models,
                    "candidateImageModels": IMAGE_MODELS,
                    "enabled": config.has_image_scope(),
                    "accountCount": config.image_account_ids.len(),
                    "groupCount": config.image_group_ids.len(),
                    "accountOptions": accounts,
                    "groupOptions": groups,
                    "accountOptionsTruncated": account_options_truncated,
                    "accountOptionsError": account_options_error,
                    "configuration": {
                        "timeoutMs": config.timeout_ms,
                        "bridgeMode": match config.mode {
                            Mode::Auto => "auto",
                            Mode::Responses => "responses",
                        },
                        "driverModel": config.driver,
                        "imageModels": config.image_models,
                        "imageGroupIds": config.image_group_ids,
                        "imageAccountIds": config.image_account_ids,
                    }
                }),
            )
        }
        ("GET", "api/preferences") => {
            if !call.payload.is_empty() {
                return bad("GET 不接受请求体");
            }
            let (selection, version) = preference_read(&call.host).await?;
            json_response(200, json!({"selection": selection, "version": version}))
        }
        ("POST", "api/preferences") => {
            let request: Value = match serde_json::from_slice(&call.payload) {
                Ok(req) => req,
                Err(_) => return bad("偏好不是合法 JSON"),
            };
            let Some(selection) = request.get("selection") else {
                return bad("偏好缺少 selection");
            };
            if !is_valid_preference(selection) {
                return bad("偏好包含无效值");
            }
            let expected_version = match request.get("expectedVersion") {
                None | Some(Value::Null) => None,
                Some(value) => match value.as_u64() {
                    Some(version) => Some(version),
                    None => return bad("偏好状态版本无效"),
                },
            };
            let version = preference_write(&call.host, selection.clone(), expected_version).await?;
            json_response(200, json!({"version": version}))
        }
        ("POST", "api/models") => {
            let request: Value = match serde_json::from_slice(&call.payload) {
                Ok(req) => req,
                Err(_) => return bad("请求不是合法 JSON"),
            };
            let Some(id) = request
                .get("clientKeyId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= 256)
            else {
                return bad("clientKeyId 不合法");
            };
            let catalog = model_catalog(&call.host, id.to_owned()).await?;
            let images = crate::access::allowed_models_for_key(&call.host, config, id).await?;
            json_response(
                200,
                json!({
                    "models": catalog.models,
                    "imageModels": images,
                }),
            )
        }
        _ => json_response(
            404,
            json!({"error":{"code":"not_found","message":"路由不存在"}}),
        ),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_choices_include_email_without_changing_identity() {
        let account = AccountFacts {
            account_id: "acct_example".to_owned(),
            provider_id: "openai".to_owned(),
            name: "acct_example".to_owned(),
            email: Some("image.user@example.org".to_owned()),
            group_ids: vec!["group-images".to_owned()],
            enabled: true,
            updated_at_ms: 0,
        };
        let result = account_option(&account);
        assert_eq!(result["email"], "image.user@example.org");
        assert_eq!(result["id"], "acct_example");
        assert_eq!(result["groupIds"][0], "group-images");
        let missing_email = AccountFacts {
            email: None,
            ..account
        };
        assert!(account_option(&missing_email)["email"].is_null());
    }

    #[test]
    fn preference_schema_accepts_4k_and_rejects_bad_values() {
        let good = json!({
            "clientKeyId": "key-1",
            "driverModel": "gpt-6-luna",
            "imageModel": "gpt-image-2.6-sunburst",
            "size": "3840x2160",
            "quality": "high",
            "count": 10,
            "parallelism": 2
        });
        assert!(is_valid_preference(&good));
        let mut bad = good.clone();
        bad["size"] = json!("4096x4096");
        assert!(!is_valid_preference(&bad));
        let mut bad = good.clone();
        bad["parallelism"] = json!(100);
        assert!(!is_valid_preference(&bad));
        let mut bad = good.clone();
        bad["token"] = json!("secret");
        assert!(!is_valid_preference(&bad));
    }

    #[test]
    fn management_route_and_page_are_registered() {
        let registration = registration();
        assert_eq!(registration.routes.len(), 4);
        assert_eq!(registration.pages[0].entry, "web/index.html");
        assert_eq!(registration.pages[0].title, "画图测试");
    }
}
