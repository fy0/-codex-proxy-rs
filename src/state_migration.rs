//! 工作台偏好从状态 Schema v1 到 v2 的无损迁移。
//! v2 只扩大 parallelism 允许范围（1..3 -> 1..32），已有记录全部满足 v2。
//! 使用宿主受管迁移接口保留记录和值，不读取密钥或修改后端生图任务。

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::host::{StateMigrationChange, StateMigrationRequest, StateMigrationResult},
    client::{TypedCall, TypedReply},
};

const WORKBENCH_NAMESPACE: &str = "image-workbench";
const FROM_VERSION: u32 = 1;
const TO_VERSION: u32 = 2;

fn plan(request: &StateMigrationRequest) -> Result<StateMigrationResult, PluginFault> {
    if request.namespace != WORKBENCH_NAMESPACE
        || request.from_schema_version != FROM_VERSION
        || request.to_schema_version != TO_VERSION
    {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "unsupported image workbench state migration",
        ));
    }

    // 不进行任何清空、重置、默认值覆盖；由 CPR 对每条 Keep 验证新 Schema。
    // image-jobs 的声明不变，不参与此次迁移。
    let changes = request
        .records
        .iter()
        .map(|record| StateMigrationChange::Keep {
            key: record.key.clone(),
        })
        .collect();

    Ok(StateMigrationResult { changes })
}

pub async fn migrate(
    call: TypedCall<StateMigrationRequest>,
) -> Result<TypedReply<StateMigrationResult>, PluginFault> {
    Ok(TypedReply::new(plan(&call.request)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(namespace: &str, from: u32, to: u32) -> StateMigrationRequest {
        StateMigrationRequest {
            namespace: namespace.into(),
            from_schema_version: from,
            to_schema_version: to,
            records: vec![gateway_plugin_sdk::call::host::StateMigrationRecord {
                key: "last-selection".into(),
                version: 7,
                value: json!({
                    "clientKeyId": "client-123",
                    "driverModel": "gpt-6-luna",
                    "imageModel": "gpt-image-2.5-sunburst",
                    "size": "3840x2160",
                    "quality": "high",
                    "count": 3,
                    "parallelism": 2
                }),
            }],
        }
    }

    #[test]
    fn original_preference_is_kept_without_mutation() {
        let before = request("image-workbench", 1, 2);
        let copy = before.records[0].value.clone();
        let migration = plan(&before).expect("valid v1 to v2 migration");
        assert_eq!(migration.changes.len(), 1);
        assert!(matches!(
            &migration.changes[0],
            StateMigrationChange::Keep { key } if key == "last-selection"
        ));
        assert_eq!(before.records[0].value, copy);
    }

    #[test]
    fn empty_batch_is_supported_and_unsupported_versions_rejected() {
        let mut empty = request("image-workbench", 1, 2);
        empty.records.clear();
        assert_eq!(plan(&empty).unwrap().changes.len(), 0);
        for bad in [
            request("image-jobs", 1, 2),
            request("image-workbench", 1, 1),
            request("image-workbench", 2, 3),
        ] {
            assert!(plan(&bad).is_err());
        }
    }

    #[test]
    fn manifest_declares_compatible_target_only_for_workbench() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../plugin.json")).unwrap();
        let declarations = manifest["state"].as_array().unwrap();
        let workbench = declarations
            .iter()
            .find(|entry| entry["namespace"] == WORKBENCH_NAMESPACE)
            .unwrap();
        assert_eq!(workbench["schemaVersion"], 2);
        assert_eq!(workbench["migratesFrom"], json!([1]));
        assert_eq!(
            workbench["schema"]["properties"]["parallelism"]["maximum"],
            32
        );
        let jobs = declarations
            .iter()
            .find(|entry| entry["namespace"] == "image-jobs")
            .unwrap();
        assert_eq!(jobs["schemaVersion"], 1);
        assert_eq!(jobs["migratesFrom"], json!([]));
    }
}
