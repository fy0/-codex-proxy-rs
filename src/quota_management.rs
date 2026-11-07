// Adapted from Mr-xiongyq/cpr-quota-guard, Apache-2.0
//! 插件页面只读宿主账号与额度事实，并通过原生实例接口提交自身配置

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use futures_util::{StreamExt, stream};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::{AccountFactsQuery, QuotaDetails},
        host::{AUTH_LIST, AuthListRequest, AuthListResult},
        management::{
            ManagementPage, ManagementRegistration, ManagementRequest, ManagementResource,
            ManagementResponse, ManagementRoute,
        },
        middleware::{MiddlewareHeader, http::Version},
    },
    client::{HostClient, HttpBody, HttpFrame, HttpRequest, TypedCall, TypedReply},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    config::Config,
    quota_guard::{self, AccountRule, Assessment, QuotaGuardConfig},
};
use gateway_plugin_sdk::call::data::QuotaFactsQuery;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_PARALLEL_READS: usize = 8;
fn now_ms() -> Result<i64, PluginFault> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|time| i64::try_from(time.as_millis()).ok())
        .ok_or_else(|| PluginFault::new(ErrorCode::Fault, "quota clock unavailable"))
}
async fn read_assessment(
    host: &HostClient,
    rule: &AccountRule,
    max_age: u64,
) -> Result<(Assessment, Option<QuotaDetails>), PluginFault> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        quota_guard::limit_host_call(host.quota_details(QuotaFactsQuery {
            account_id: rule.account_id.clone(),
        })),
    )
    .await
    {
        Ok(Ok(quota)) => Ok((
            quota_guard::assess(rule, &quota, now_ms()?, max_age),
            Some(quota),
        )),
        Ok(Err(fault)) if fault.code == ErrorCode::Cancelled => Err(fault),
        _ => Ok((quota_guard::unavailable(rule), None)),
    }
}

const JSON: &str = "application/json";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const INSTANCES_PATH: &str = "/api/admin/plugins/instances";
const UPDATE_PATH: &str = "/api/admin/plugins/instances/update";

pub(crate) fn registration() -> ManagementRegistration {
    ManagementRegistration {
        routes: [
            ("GET", "quota/overview"),
            ("GET", "quota/accounts"),
            ("POST", "quota/configuration"),
            ("POST", "quota/calibration"),
        ]
        .into_iter()
        .map(|(method, path)| ManagementRoute {
            method: method.into(),
            path: path.into(),
            request_content_types: if method == "POST" {
                vec![JSON.into()]
            } else {
                Vec::new()
            },
            response_content_types: vec![JSON.into()],
        })
        .collect(),
        resources: [
            "web/quota/index.html",
            "web/quota/assets/app.js",
            "web/quota/assets/app.css",
        ]
        .into_iter()
        .map(|path| ManagementResource {
            path: path.into(),
            public: false,
        })
        .collect(),
        pages: vec![ManagementPage {
            id: "quota-guard-dashboard".into(),
            title: "账号额度保护".into(),
            description: Some("账号 5 小时、每周与每日 USD 费用保护".into()),
            entry: "web/quota/index.html".into(),
            icon: None,
        }],
        callbacks: Vec::new(),
    }
}

enum ManagementError {
    Http(u16, String),
    Callback(PluginFault),
}
impl From<PluginFault> for ManagementError {
    fn from(value: PluginFault) -> Self {
        Self::Callback(value)
    }
}
fn invalid(message: impl Into<String>) -> ManagementError {
    ManagementError::Http(400, message.into())
}
fn conflict() -> ManagementError {
    ManagementError::Http(409, "配置已发生变化，请通过宿主刷新页面后重新确认".into())
}
fn unavailable() -> ManagementError {
    ManagementError::Http(502, "无法读取或保存宿主配置，请刷新页面确认当前状态".into())
}

pub(crate) async fn handle(
    call: TypedCall<ManagementRequest>,
    original: &Value,
    quota_config: &QuotaGuardConfig,
    artifact_sha256: &str,
) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    let result = match (call.request.method.as_str(), call.request.path.as_str()) {
        ("GET", "quota/overview") if call.request.query.is_empty() => {
            overview(&call, original, quota_config, artifact_sha256).await
        }
        ("GET", "quota/accounts") => accounts(&call).await,
        ("POST", "quota/configuration") if call.request.query.is_empty() => {
            save_configuration(&call, original, artifact_sha256).await
        }
        ("POST", "quota/calibration") if call.request.query.is_empty() => {
            save_calibration(&call, original, artifact_sha256).await
        }
        _ => Err(invalid("页面请求的路径或参数不受支持")),
    };
    let (status, body) = match result {
        Ok(body) => (200, body),
        Err(ManagementError::Http(status, message)) => (status, json!({"message":message})),
        Err(ManagementError::Callback(fault)) if fault.code == ErrorCode::Cancelled => {
            return Err(fault);
        }
        Err(ManagementError::Callback(_)) => {
            (502, json!({"message":"无法读取宿主数据，请重新加载"}))
        }
    };
    let payload = serde_json::to_vec(&body)
        .map_err(|_| PluginFault::new(ErrorCode::Fault, "无法生成页面响应"))?;
    Ok(TypedReply::new(ManagementResponse {
        status,
        content_type: JSON.into(),
        headers: Vec::new(),
    })
    .with_payload(payload))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Instance {
    id: String,
    name: String,
    artifact_sha256: String,
    enabled: bool,
    configuration: Value,
    bindings: Value,
    revision: u64,
}

async fn own_instance(
    call: &TypedCall<ManagementRequest>,
    original: &Value,
    artifact_sha256: &str,
) -> Result<Instance, ManagementError> {
    let data = admin_request(&call.host, "GET", INSTANCES_PATH, None).await?;
    let instances: Vec<Instance> = serde_json::from_value(data).map_err(|_| unavailable())?;
    let instance = instances
        .into_iter()
        .find(|instance| instance.id == call.context.instance_id)
        .ok_or_else(conflict)?;
    // 页面属于不可变制品与配置，不能用旧页面覆盖另一个版本或已发布的新配置
    if !instance.enabled
        || instance.artifact_sha256 != artifact_sha256
        || instance.configuration != *original
    {
        return Err(conflict());
    }
    Ok(instance)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountView {
    account_id: String,
    name: Option<String>,
    email: Option<String>,
    enabled: Option<bool>,
    assessment: Assessment,
    quota: Option<QuotaView>,
    daily_usage: quota_guard::DailyUsage,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QuotaView {
    observed_at_ms: Option<i64>,
    windows: Vec<WindowView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowView {
    window_seconds: u64,
    used_percent: Option<f64>,
    reset_at_ms: Option<i64>,
}

fn quota_view(account_id: &str, quota: QuotaDetails) -> Option<QuotaView> {
    if quota.schema_version != 1 || quota.account_id != account_id {
        return None;
    }
    Some(QuotaView {
        observed_at_ms: quota.observed_at_ms,
        windows: quota
            .windows
            .into_iter()
            .filter(|window| {
                window
                    .limit_id
                    .as_deref()
                    .is_some_and(|id| id.trim().eq_ignore_ascii_case("codex"))
                    && matches!(window.window_seconds, Some(18_000 | 604_800))
            })
            .map(|window| WindowView {
                window_seconds: window.window_seconds.unwrap_or_default(),
                used_percent: window
                    .used_percent
                    .filter(|value| value.is_finite() && (0.0..=100.0).contains(value)),
                reset_at_ms: window.reset_at_ms,
            })
            .collect(),
    })
}

async fn overview(
    call: &TypedCall<ManagementRequest>,
    original: &Value,
    quota_config: &QuotaGuardConfig,
    artifact_sha256: &str,
) -> Result<Value, ManagementError> {
    let instance = own_instance(call, original, artifact_sha256).await?;
    quota_guard::refresh_daily_overview(&call.host, quota_config).await?;
    let wanted: HashSet<_> = quota_config
        .accounts
        .iter()
        .map(|rule| rule.account_id.as_str())
        .collect();
    let mut facts = HashMap::new();
    let mut cursor = None;
    while facts.len() < wanted.len() {
        let page = call
            .host
            .account_facts(AccountFactsQuery {
                provider_id: Some("openai".into()),
                cursor: cursor.clone(),
                limit: 200,
            })
            .await?;
        for account in page.accounts {
            if wanted.contains(account.account_id.as_str()) {
                facts.insert(account.account_id.clone(), account);
            }
        }
        match page.next_cursor {
            Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
            None => break,
            _ => return Err(unavailable()),
        }
    }
    let facts = Arc::new(facts);
    let host = call.host.clone();
    let max_age = quota_config.max_observation_age_seconds;
    let timezone = quota_config.daily_timezone;
    let accounts = stream::iter(quota_config.accounts.clone())
        .map(move |rule| {
            let facts = Arc::clone(&facts);
            let host = host.clone();
            async move {
                let (mut assessment, quota) = read_assessment(&host, &rule, max_age).await?;
                let daily_usage = quota_guard::daily_usage(&rule, now_ms()?, max_age, timezone);
                quota_guard::add_daily(&mut assessment, &daily_usage);
                let fact = facts.get(&rule.account_id);
                Ok::<_, PluginFault>(AccountView {
                    account_id: rule.account_id.clone(),
                    name: fact.map(|value| value.name.clone()),
                    email: fact.and_then(|value| value.email.clone()),
                    enabled: fact.map(|value| value.enabled),
                    assessment,
                    quota: quota.and_then(|value| quota_view(&rule.account_id, value)),
                    daily_usage,
                })
            }
        })
        .buffered(MAX_PARALLEL_READS)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    Ok(
        json!({"revision":instance.revision,"configuration":quota_config,"evaluatedAtMs":now_ms()?,"accounts":accounts}),
    )
}

async fn accounts(call: &TypedCall<ManagementRequest>) -> Result<Value, ManagementError> {
    let mut cursor = None;
    let mut limit = None;
    for pair in call
        .request
        .query
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        let (key, raw) = pair
            .split_once('=')
            .ok_or_else(|| invalid("账号分页参数不合法"))?;
        match key {
            "cursor" if cursor.is_none() => {
                let value = decode_query(raw)?;
                if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                    return Err(invalid("账号分页游标不合法"));
                }
                cursor = Some(value);
            }
            "limit" if limit.is_none() => {
                let value: u16 = raw
                    .parse()
                    .map_err(|_| invalid("每页账号数量必须在 1 到 100 之间"))?;
                if !(1..=100).contains(&value) {
                    return Err(invalid("每页账号数量必须在 1 到 100 之间"));
                }
                limit = Some(value);
            }
            _ => return Err(invalid("账号分页参数不合法")),
        }
    }
    // 运行投影只含账号类型与非秘密资料，避免将没有额度窗口的 URL + Key 账号加入候选
    let query = AuthListRequest {
        provider_id: Some("openai".into()),
        cursor,
        limit: limit.unwrap_or(100),
    };
    let reply = call
        .host
        .call(
            AUTH_LIST,
            json!({}),
            serde_json::to_vec(&query).map_err(|_| unavailable())?,
        )
        .await
        .map_err(|error| ManagementError::from(error.into_plugin_fault()))?;
    if reply.result != json!({}) {
        return Err(unavailable());
    }
    let page: AuthListResult = serde_json::from_slice(&reply.payload).map_err(|_| unavailable())?;
    let items: Vec<_> = page.accounts.into_iter()
        .filter(|account| account.authentication_kind == "oauth")
        .map(|account| json!({
        "accountId":account.account_id,"name":account.name,"email":account.email,"enabled":account.enabled,
    })).collect();
    Ok(json!({"items":items,"nextCursor":page.next_cursor}))
}

fn decode_query(value: &str) -> Result<String, ManagementError> {
    let mut bytes = Vec::new();
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'+' => b' ',
            b'%' => {
                let high = input
                    .next()
                    .and_then(|value| char::from(value).to_digit(16));
                let low = input
                    .next()
                    .and_then(|value| char::from(value).to_digit(16));
                match (high, low) {
                    (Some(high), Some(low)) => (high * 16 + low) as u8,
                    _ => return Err(invalid("账号分页游标编码不合法")),
                }
            }
            _ => byte,
        });
    }
    String::from_utf8(bytes).map_err(|_| invalid("账号分页游标编码不合法"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveConfiguration {
    expected_revision: u64,
    configuration: Value,
}

fn write_quota_configuration(original: &Value, quota: &QuotaGuardConfig) -> Result<Value, String> {
    let mut next = original.clone();
    let object = next.as_object_mut().ok_or("插件配置格式不合法")?;
    object.insert(
        "quotaGuard".into(),
        serde_json::to_value(&quota.accounts).map_err(|_| "额度保护配置无法编码")?,
    );
    object.insert(
        "maxObservationAgeSeconds".into(),
        json!(quota.max_observation_age_seconds),
    );
    object.insert("dailyTimezone".into(), json!(quota.daily_timezone));
    Config::from_configuration(&next)?;
    Ok(next)
}

fn merge_configuration(original: &Value, quota: &QuotaGuardConfig) -> Result<Value, String> {
    let previous = Config::from_configuration(original)?.quota_guard;
    let mut next = quota.clone();
    for rule in &mut next.accounts {
        // 普通设置保存不能伪造校准时间，也不因旧 UI 省略字段而丢失已保存基准。
        // 更改时区明确使旧校准失效；以后切回旧时区也不会将它复活。
        rule.daily_calibration = if previous.daily_timezone == next.daily_timezone {
            previous
                .accounts
                .iter()
                .find(|old| old.account_id == rule.account_id)
                .and_then(|old| old.daily_calibration.clone())
        } else {
            None
        };
    }
    write_quota_configuration(original, &next)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveCalibration {
    expected_revision: u64,
    account_id: String,
    amount_usd: Value,
}

fn calibrated_configuration(
    original: &Value,
    account_id: &str,
    amount: &Value,
    now_ms: i64,
) -> Result<Value, String> {
    let mut quota = Config::from_configuration(original)?.quota_guard;
    let rule = quota
        .accounts
        .iter_mut()
        .find(|rule| rule.account_id == account_id)
        .ok_or("账号尚未保存到额度保护配置，请先保存设置")?;
    rule.daily_calibration = match amount {
        Value::Null => None,
        Value::String(value) => Some(quota_guard::DailyCalibration::new(
            value,
            now_ms,
            quota.daily_timezone,
        )?),
        _ => return Err("校准金额必须为非负十进制字符串，或使用 null 清除".into()),
    };
    write_quota_configuration(original, &quota)
}

async fn save_configuration(
    call: &TypedCall<ManagementRequest>,
    original: &Value,
    artifact_sha256: &str,
) -> Result<Value, ManagementError> {
    let input: SaveConfiguration =
        serde_json::from_slice(&call.payload).map_err(|_| invalid("配置保存请求格式不合法"))?;
    let next_quota = QuotaGuardConfig::from_value(input.configuration).map_err(invalid)?;
    let next_config = merge_configuration(original, &next_quota).map_err(invalid)?;
    persist_configuration(
        call,
        original,
        artifact_sha256,
        next_config,
        input.expected_revision,
    )
    .await
}

async fn save_calibration(
    call: &TypedCall<ManagementRequest>,
    original: &Value,
    artifact_sha256: &str,
) -> Result<Value, ManagementError> {
    let input: SaveCalibration =
        serde_json::from_slice(&call.payload).map_err(|_| invalid("费用校准请求格式不合法"))?;
    let next_config =
        calibrated_configuration(original, &input.account_id, &input.amount_usd, now_ms()?)
            .map_err(invalid)?;
    persist_configuration(
        call,
        original,
        artifact_sha256,
        next_config,
        input.expected_revision,
    )
    .await
}

async fn persist_configuration(
    call: &TypedCall<ManagementRequest>,
    original: &Value,
    artifact_sha256: &str,
    next_config: Value,
    expected_revision: u64,
) -> Result<Value, ManagementError> {
    let instance = own_instance(call, original, artifact_sha256).await?;
    if expected_revision != instance.revision {
        return Err(conflict());
    }
    // 始终使用宿主上下文的自身 ID，并保留绑定、启停和制品；并发变化交给宿主 CAS 拒绝。
    let update = json!({"id":call.context.instance_id,"instance":{
        "name":instance.name,"artifactSha256":instance.artifact_sha256,"enabled":instance.enabled,
        "configuration":next_config,"bindings":instance.bindings,"expectedRevision":expected_revision,
    }});
    admin_request(&call.host, "POST", UPDATE_PATH, Some(update)).await?;
    Ok(json!({"saved":true,"reloadRequired":true}))
}

async fn admin_request(
    host: &HostClient,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, ManagementError> {
    let (body, headers) = match body {
        Some(value) => (
            HttpBody::from_bytes(serde_json::to_vec(&value).map_err(|_| unavailable())?),
            vec![MiddlewareHeader {
                name: "content-type".into(),
                value: JSON.as_bytes().to_vec(),
            }],
        ),
        None => (HttpBody::empty(), Vec::new()),
    };
    let mut response = host
        .dispatch_http(HttpRequest {
            settings: Value::Null,
            method: method.into(),
            uri: path.into(),
            version: Version::Http11,
            headers,
            timeout_ms: Some(30_000),
            body,
        })
        .await?;
    let mut bytes = Vec::new();
    loop {
        match response.body.read().await {
            Ok(Some(HttpFrame::Data(data)))
                if data.len() <= MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) =>
            {
                bytes.extend(data)
            }
            Ok(Some(HttpFrame::Trailers(_))) => {}
            Ok(None) => break,
            result => {
                let _ = response.body.close().await;
                return Err(result.err().map_or_else(unavailable, ManagementError::from));
            }
        }
    }
    if response.status == 409 {
        return Err(conflict());
    }
    if !(200..300).contains(&response.status) {
        return Err(unavailable());
    }
    let result: Value = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    if result.get("code").and_then(Value::as_u64) != Some(200) {
        return Err(unavailable());
    }
    result.get("data").cloned().ok_or_else(unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_daily_rules_preserves_image_configuration_and_old_window_rules() {
        let original = json!({
            "driverModel":"gpt-5.6-luna","bridgeMode":"responses","timeoutMs":480000,
            "imageAccountIds":["images"],"imageGroupIds":["group"],"imageModels":["gpt-image-2"],
            "imageStorageDir":"/tmp/previous-images","backgroundConcurrency":2,
            "concurrencyPerAccount":8,
            "quotaGuard":[{"accountId":"guard","fiveHourLimitPercent":80,"weeklyLimitPercent":90}],
            "maxObservationAgeSeconds":300
        });
        let quota = QuotaGuardConfig::from_value(json!({
            "accounts":[{"accountId":"guard","fiveHourLimitPercent":80,
                "weeklyLimitPercent":90,"dailyLimitUsd":"12.345678901234"}],
            "maxObservationAgeSeconds":120
        }))
        .unwrap();
        let merged = merge_configuration(&original, &quota).unwrap();
        for (key, value) in original.as_object().unwrap() {
            if key != "quotaGuard" && key != "maxObservationAgeSeconds" {
                assert_eq!(&merged[key], value, "{key}");
            }
        }
        assert_eq!(merged["quotaGuard"][0]["dailyLimitUsd"], "12.345678901234");
        assert_eq!(
            merged["quotaGuard"][0]["fiveHourLimitPercent"].as_f64(),
            Some(80.0)
        );
        assert_eq!(
            merged["quotaGuard"][0]["weeklyLimitPercent"].as_f64(),
            Some(90.0)
        );
        assert_eq!(merged["maxObservationAgeSeconds"], 120);
        assert!(original["quotaGuard"][0].get("dailyLimitUsd").is_none());
    }

    #[test]
    fn manual_calibration_uses_saved_timezone_and_preserves_other_configuration() {
        let original = json!({
            "driverModel":"gpt-6-luna", "imageModels":["gpt-image-2.5-sunburst"],
            "imageAccountIds":["image"], "concurrencyPerAccount":8,
            "dailyTimezone":"Asia/Shanghai", "maxObservationAgeSeconds":300,
            "quotaGuard":[{"accountId":"a","dailyLimitUsd":"150"}, {"accountId":"b","dailyLimitUsd":"80"}]
        });
        let at = 5 * 86_400_000 + 123_456;
        let calibrated = calibrated_configuration(&original, "a", &json!("134.1300"), at).unwrap();
        let stamp = &calibrated["quotaGuard"][0]["dailyCalibration"];
        assert_eq!(stamp["amountUsd"], "134.13");
        assert_eq!(stamp["atMs"], at);
        assert_eq!(stamp["timezone"], "Asia/Shanghai");
        assert_eq!(stamp["windowStartMs"], 5 * 86_400_000 - 8 * 3_600_000);
        assert_eq!(stamp["windowEndMs"], 6 * 86_400_000 - 8 * 3_600_000);
        for field in [
            "driverModel",
            "imageModels",
            "imageAccountIds",
            "concurrencyPerAccount",
            "dailyTimezone",
        ] {
            assert_eq!(calibrated[field], original[field]);
        }
        assert_eq!(calibrated["quotaGuard"][1]["dailyLimitUsd"], "80");
        assert!(calibrated["quotaGuard"][1]["dailyCalibration"].is_null());
        let cleared = calibrated_configuration(&calibrated, "a", &Value::Null, at).unwrap();
        assert!(cleared["quotaGuard"][0]["dailyCalibration"].is_null());
        for invalid in [json!(1), json!("-1"), json!("NaN")] {
            assert!(calibrated_configuration(&original, "a", &invalid, at).is_err());
        }
        assert!(calibrated_configuration(&original, "missing", &json!("1"), at).is_err());
    }

    #[test]
    fn normal_saves_preserve_authoritative_calibration_and_timezone_changes_clear_it() {
        let original = json!({
            "quotaGuard":[{"accountId":"a","dailyLimitUsd":"150"}],
            "dailyTimezone":"UTC"
        });
        let saved = calibrated_configuration(&original, "a", &json!("20"), 123_456).unwrap();
        let mut candidate = QuotaGuardConfig::from_value(json!({
            "accounts":[{"accountId":"a","dailyLimitUsd":"170"}],
            "dailyTimezone":"UTC"
        }))
        .unwrap();
        let merged = merge_configuration(&saved, &candidate).unwrap();
        assert_eq!(
            merged["quotaGuard"][0]["dailyCalibration"],
            saved["quotaGuard"][0]["dailyCalibration"]
        );
        // A normal settings request cannot manufacture another cutover timestamp.
        candidate.accounts[0].daily_calibration =
            Some(quota_guard::DailyCalibration::new("900", 234_567, chrono_tz::UTC).unwrap());
        assert_eq!(
            merge_configuration(&saved, &candidate).unwrap()["quotaGuard"][0]["dailyCalibration"],
            saved["quotaGuard"][0]["dailyCalibration"]
        );
        candidate.daily_timezone = chrono_tz::Asia::Shanghai;
        let changed = merge_configuration(&saved, &candidate).unwrap();
        assert!(changed["quotaGuard"][0]["dailyCalibration"].is_null());
        candidate.daily_timezone = chrono_tz::UTC;
        assert!(
            merge_configuration(&changed, &candidate).unwrap()["quotaGuard"][0]["dailyCalibration"]
                .is_null()
        );
    }

    #[test]
    fn calibration_request_rejects_client_timestamps() {
        assert!(
            serde_json::from_value::<SaveCalibration>(json!({
                "expectedRevision":1, "accountId":"a", "amountUsd":"1", "atMs":1
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SaveCalibration>(json!({
                "expectedRevision":1, "accountId":"a"
            }))
            .is_err()
        );
        assert!(
            registration()
                .routes
                .iter()
                .any(|route| route.method == "POST" && route.path == "quota/calibration")
        );
    }

    #[test]
    fn unknown_daily_usage_is_visible_without_excluding_the_account() {
        let rule: AccountRule = serde_json::from_value(json!({
            "accountId":"daily-only","fiveHourLimitPercent":null,
            "weeklyLimitPercent":null,"dailyLimitUsd":"1"
        }))
        .unwrap();
        let mut assessment = quota_guard::unavailable(&rule);
        assert!(!assessment.excluded);
        let usage = quota_guard::daily_usage(&rule, 1_000, 300, chrono_tz::UTC);
        quota_guard::add_daily(&mut assessment, &usage);
        let value = serde_json::to_value(assessment).unwrap();
        assert_eq!(value["excluded"], false);
        assert_eq!(value["reasons"][0]["code"], "daily_usage_unknown");
        assert_eq!(value["reasons"][0]["windowSeconds"], 86_400);
    }
}
