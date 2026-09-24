//! `gateway-admin` 账号端口的 PostgreSQL adapter。

use std::collections::BTreeMap;
use std::sync::Arc;

use gateway_core::provider_ports::ProviderCooldownPort;

use super::*;
use crate::postgres::ObservabilityQueryBudget;

/// Admin 账号用例所需的公共账号、留存观测与 revision 事务能力。
///
/// 三个 PostgreSQL adapter 都保持私有，调用方只能取得 [`AccountStore`] 暴露的领域能力。
#[derive(Clone)]
pub struct PgAdminAccountStore {
    pool: PgPool,
    accounts: PgProviderAccountRepository,
    observability: PgObservabilityRepository,
    control_plane: PgControlPlaneRepository,
    cooldowns: Option<Arc<dyn ProviderCooldownPort>>,
    query_budget: ObservabilityQueryBudget,
}

impl PgAdminAccountStore {
    #[must_use]
    pub fn new(
        pool: PgPool,
        cooldowns: Option<Arc<dyn ProviderCooldownPort>>,
        query_budget: ObservabilityQueryBudget,
    ) -> Self {
        Self {
            pool: pool.clone(),
            accounts: PgProviderAccountRepository::new(pool.clone()),
            observability: PgObservabilityRepository::new(pool.clone(), None, query_budget.clone()),
            control_plane: PgControlPlaneRepository::new(pool),
            cooldowns,
            query_budget,
        }
    }

    async fn usage_observations(
        &self,
        range: ObservabilityRange,
        account_ids: &[String],
    ) -> AdminStoreResult<Vec<ProviderAccountUsageObservation>> {
        if account_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut observations = Vec::with_capacity(account_ids.len());
        for account_ids in account_ids.chunks(ADMIN_USAGE_CHUNK_SIZE) {
            let query = ProviderAccountUsageQuery::for_accounts(range, account_ids.to_vec())
                .and_then(|query| {
                    if range.end.signed_duration_since(range.start) <= TimeDelta::hours(24) {
                        query.with_hourly_request_buckets()
                    } else {
                        Ok(query)
                    }
                })
                .map_err(|error| admin_store_error(ENTITY, error))?;
            observations.extend(
                self.observability
                    .provider_account_usage(query)
                    .await
                    .map_err(|error| admin_store_error(ENTITY, error))?,
            );
        }
        Ok(observations)
    }

    async fn usage_by_windows(
        &self,
        windows: &[AccountUsageWindowQuery],
    ) -> AdminStoreResult<Vec<AccountUsageWindowResult>> {
        if windows.is_empty() {
            return Ok(Vec::new());
        }
        let account_ids = windows
            .iter()
            .map(|window| window.account_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        validate_admin_account_ids(&account_ids)
            .map_err(|error| admin_store_error(ENTITY, error))?;
        for window in windows {
            require_nonempty(ENTITY, "quota window key", &window.key)
                .map_err(|error| admin_store_error(ENTITY, error))?;
            ObservabilityRange::new(window.range.start, window.range.end)
                .map_err(|error| admin_store_error(ENTITY, error))?;
        }
        let keys = windows
            .iter()
            .map(|window| window.key.clone())
            .collect::<Vec<_>>();
        let starts = windows
            .iter()
            .map(|window| window.range.start)
            .collect::<Vec<_>>();
        let ends = windows
            .iter()
            .map(|window| window.range.end)
            .collect::<Vec<_>>();
        let account_ids = windows
            .iter()
            .map(|window| window.account_id.clone())
            .collect::<Vec<_>>();
        let rows = self
            .query_budget
            .run("load account usage windows", async {
                sqlx::query(sqlx::AssertSqlSafe(account_usage_by_windows_sql()))
                    .bind(account_ids)
                    .bind(keys)
                    .bind(starts)
                    .bind(ends)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|_| postgres_unavailable("load provider account quota window usage"))
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let mut usage_rows = Vec::with_capacity(windows.len());
        let mut costs_by_window = BTreeMap::<(String, String), Vec<AccountCost>>::new();
        let mut model_costs = BTreeMap::<(String, String, String), Vec<AccountCost>>::new();
        let mut models_by_window = BTreeMap::<(String, String), Vec<AccountModelUsage>>::new();
        for row in &rows {
            let model_grouping = window_usage_value::<i32>(row, "model_grouping")?;
            let currency_grouping = window_usage_value::<i32>(row, "currency_grouping")?;
            match (model_grouping, currency_grouping) {
                (1, 1) => usage_rows.push(row),
                (1, 0) if window_usage_value::<Option<String>>(row, "cost_currency")?.is_some() => {
                    let (key, cost) = admin_account_usage_window_cost(row)?;
                    costs_by_window.entry(key).or_default().push(cost);
                }
                (0, 1) if window_usage_value::<Option<String>>(row, "model")?.is_some() => {
                    let ((account_id, window_key, _), usage) =
                        admin_account_usage_window_model(row)?;
                    models_by_window
                        .entry((account_id, window_key))
                        .or_default()
                        .push(usage);
                }
                (0, 0)
                    if window_usage_value::<Option<String>>(row, "model")?.is_some()
                        && window_usage_value::<Option<String>>(row, "cost_currency")?
                            .is_some() =>
                {
                    let (key, cost) = admin_account_usage_window_model_cost(row)?;
                    model_costs.entry(key).or_default().push(cost);
                }
                (0 | 1, 0 | 1) => {}
                _ => {
                    return Err(AdminStoreError::new(
                        AdminStoreErrorKind::Unavailable,
                        ENTITY,
                        "account usage window query returned an invalid grouping marker",
                    ));
                }
            }
        }
        let mut results = usage_rows
            .into_iter()
            .map(admin_account_usage_window)
            .collect::<AdminStoreResult<Vec<_>>>()?;
        for result in &mut results {
            result.usage.costs = costs_by_window
                .remove(&(result.account_id.clone(), result.key.clone()))
                .unwrap_or_default();
            result.usage.models = models_by_window
                .remove(&(result.account_id.clone(), result.key.clone()))
                .unwrap_or_default();
            for model in &mut result.usage.models {
                model.costs = model_costs
                    .remove(&(
                        result.account_id.clone(),
                        result.key.clone(),
                        model.model.clone(),
                    ))
                    .unwrap_or_default();
            }
            result.usage.models.sort_by(|left, right| {
                right
                    .request_count
                    .cmp(&left.request_count)
                    .then_with(|| left.model.cmp(&right.model))
            });
        }
        Ok(results)
    }

    async fn required_scope(
        &self,
        account_id: &str,
    ) -> AdminStoreResult<ProviderAccountAdminScope> {
        let record = self
            .accounts
            .load_provider_account(account_id)
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?
            .ok_or_else(|| {
                admin_store_error(
                    ENTITY,
                    StoreError::NotFound {
                        entity: ENTITY,
                        id: account_id.to_owned(),
                    },
                )
            })?;
        Ok(ProviderAccountAdminScope {
            provider_kind: record.summary.provider_kind,
        })
    }

    async fn commit_prepared_import(
        &self,
        prepared: PreparedCredentialImport,
        settings: Option<AccountImportSettings>,
        context: &MutationContext,
        action: &str,
        outbound_proxy: Option<gateway_admin::model::proxies::ImportProxyBinding>,
    ) -> AdminStoreResult<CredentialImportResult> {
        let provider_kind = prepared.provider_kind.as_str().to_owned();
        let accounts = prepared
            .credentials
            .into_iter()
            .map(prepared_account)
            .collect::<StoreResult<Vec<_>>>()
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let mut changed_fields = vec!["credentials".to_owned()];
        if settings
            .as_ref()
            .is_some_and(|settings| settings.model_access.is_some())
            || accounts
                .iter()
                .any(|account| account.model_access.is_some())
        {
            changed_fields.push("model_access".to_owned());
        }
        if let Some(settings) = &settings {
            changed_fields
                .extend(["enabled", "concurrency_limit", "weight", "group_ids"].map(str::to_owned));
            if settings.notes.is_some() {
                changed_fields.push("notes".to_owned());
            }
        }
        let imported = self
            .accounts
            .import_provider_accounts(ImportProviderAccounts {
                settings,
                outbound_proxy,
                scope: ProviderAccountAdminScope {
                    provider_kind: provider_kind.clone(),
                },
                accounts,
                audit: mutation_audit(
                    context,
                    action,
                    "provider_account",
                    &provider_kind,
                    changed_fields,
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(CredentialImportResult {
            config_revision: admin_revision(imported.config_revision)?,
            credential_ids: imported
                .account_ids
                .into_iter()
                .map(CoreProviderAccountId::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| {
                    AdminStoreError::new(
                        AdminStoreErrorKind::Unavailable,
                        ENTITY,
                        "provider account import returned an invalid account ID",
                    )
                })?,
        })
    }

    async fn commit_prepared_rotation(
        &self,
        prepared: PreparedCredentialRotationFacts,
        settings: Option<UpdateAccount>,
        context: &MutationContext,
        action: &str,
    ) -> AdminStoreResult<CredentialMutationResult> {
        let account_id = prepared.account_id.clone();
        let scope = ProviderAccountAdminScope {
            provider_kind: prepared.provider_kind.as_str().to_owned(),
        };
        let mut changed_fields = vec!["credentials".to_owned()];
        if let Some(settings) = &settings {
            changed_fields
                .extend(["enabled", "concurrency_limit", "weight", "groups"].map(str::to_owned));
            if settings.model_access.is_some() {
                changed_fields.push("model_access".to_owned());
            }
            if settings.outbound_proxy.is_some() {
                changed_fields.push("outbound_proxy".to_owned());
            }
            if settings.notes.is_some() {
                changed_fields.push("notes".to_owned());
            }
        }
        let rotation = self
            .accounts
            .rotate_provider_account(RotateProviderAccount {
                settings,
                scope,
                profile: UpdateProviderAccount {
                    id: account_id.as_str().to_owned(),
                    name: prepared.name,
                    email: prepared.email,
                    plan_type: prepared.plan_type,
                },
                replacement_identity: prepared.replacement_identity,
                credential: ProviderCredentialUpdate {
                    account_id: account_id.as_str().to_owned(),
                    expected_revision: store_revision(prepared.expected_credential_revision)?,
                    provider_credentials_json: provider_document_json(prepared.provider_material)
                        .map_err(|error| admin_store_error(ENTITY, error))?,
                    has_refresh_token: prepared.has_refresh_token,
                    access_token_expires_at: prepared.access_token_expires_at,
                    next_refresh_at: prepared.next_refresh_at,
                    preserve_profile: prepared.preserve_profile,
                },
                audit: mutation_audit(
                    context,
                    action,
                    "provider_account",
                    account_id.as_str(),
                    changed_fields,
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(CredentialMutationResult {
            config_revision: admin_revision(rotation.config_revision)?,
            account_id,
            credential_revision: Some(admin_revision(rotation.credential_revision)?),
        })
    }

    async fn account_groups_by_account(
        &self,
        account_ids: &[String],
    ) -> AdminStoreResult<BTreeMap<String, Vec<AccountGroupRef>>> {
        if account_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let rows = sqlx::query_as::<_, (String, String, String, String, bool)>(
            "select m.provider_account_id, g.id, g.name, g.color, g.enabled
             from account_group_accounts m
             join account_groups g on g.id = m.account_group_id
             where m.provider_account_id = any($1::text[])
             order by m.provider_account_id, g.id",
        )
        .bind(account_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| {
            admin_store_error(
                ENTITY,
                postgres_unavailable("load account group references"),
            )
        })?;
        let mut groups = BTreeMap::<String, Vec<AccountGroupRef>>::new();
        for (account_id, group_id, name, color, enabled) in rows {
            let id = AccountGroupId::new(group_id).map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Invalid,
                    ENTITY,
                    "persisted account group ID is invalid",
                )
            })?;
            groups.entry(account_id).or_default().push(AccountGroupRef {
                id,
                name,
                color: gateway_admin::model::account_groups::AccountGroupColor::parse(&color)
                    .ok_or_else(|| {
                        AdminStoreError::new(
                            AdminStoreErrorKind::Invalid,
                            ENTITY,
                            "persisted account group color is invalid",
                        )
                    })?,
                enabled,
            });
        }
        Ok(groups)
    }
}

#[async_trait]
impl AccountStore for PgAdminAccountStore {
    async fn turn_state_token(
        &self,
        account_id: &CoreProviderAccountId,
        model: &str,
        issued_at: i64,
        observation_id: Option<i64>,
    ) -> AdminStoreResult<Option<gateway_core::account::TurnStateToken>> {
        self.accounts
            .copyable_turn_state(account_id, model, issued_at, observation_id)
            .await
            .map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Unavailable,
                    ENTITY,
                    "turn state unavailable",
                )
            })
    }

    async fn remove_turn_state(
        &self,
        account_id: &CoreProviderAccountId,
        model: &str,
        issued_at: i64,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        let mut transaction = self.pool.begin().await.map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Unavailable,
                ENTITY,
                "turn state transaction unavailable",
            )
        })?;
        // 保留签发时间水位，阻止同一张旧票被在途响应或后台任务重新安装。
        let removed = sqlx::query("update account_turn_states set turn_state_override = null, attached_model = null, manual_override = false, next_probe_at = null where account_id = $1 and model = $2 and current_issued_at = $3 and turn_state_override is not null")
            .bind(account_id.as_str()).bind(model).bind(issued_at)
            .execute(&mut *transaction).await.map_err(|_| {
                AdminStoreError::new(AdminStoreErrorKind::Unavailable, ENTITY, "turn state removal unavailable")
            })?;
        if removed.rows_affected() == 0 {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Conflict,
                ENTITY,
                "installed state changed or already removed",
            ));
        }
        let result: StoreResult<Revision> = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            append_admin_audit_event_in_transaction(
                &mut transaction,
                mutation_audit(
                    context,
                    "remove_turn_state",
                    "provider_account",
                    account_id.as_str(),
                    vec!["turn_state_override".to_owned()],
                ),
                revision,
            )
            .await?;
            Ok(revision)
        }
        .await;
        let revision =
            super::repository::finish_admin_transaction(transaction, result, "remove turn state")
                .await
                .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(AccountUpdateResult {
            account_id: account_id.clone(),
            config_revision: admin_revision(revision)?,
        })
    }

    async fn apply_turn_state(
        &self,
        account_id: &CoreProviderAccountId,
        model: &str,
        issued_at: i64,
        observation_id: Option<i64>,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        let mut transaction = self.pool.begin().await.map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Unavailable,
                ENTITY,
                "turn state transaction unavailable",
            )
        })?;
        // 指定观测 ID 时只操作该行，不能被同秒签发的当前候选替代。
        let installed = observation_id.is_none()
            && super::turn_state::install_candidate(
                &mut transaction,
                account_id,
                model,
                Some(issued_at),
            )
            .await
            .map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Unavailable,
                    ENTITY,
                    "turn state installation unavailable",
                )
            })?
            || super::turn_state::install_observed_turn_state(
                &mut transaction,
                account_id,
                model,
                issued_at,
                observation_id,
            )
            .await
            .map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Unavailable,
                    ENTITY,
                    "turn state installation unavailable",
                )
            })?;
        if !installed {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Conflict,
                ENTITY,
                "turn state expired or changed",
            ));
        }
        let result: StoreResult<Revision> = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            append_admin_audit_event_in_transaction(
                &mut transaction,
                mutation_audit(
                    context,
                    "apply_turn_state",
                    "provider_account",
                    account_id.as_str(),
                    vec!["turn_state_override".to_owned()],
                ),
                revision,
            )
            .await?;
            Ok(revision)
        }
        .await;
        let revision =
            super::repository::finish_admin_transaction(transaction, result, "apply turn state")
                .await
                .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(AccountUpdateResult {
            account_id: account_id.clone(),
            config_revision: admin_revision(revision)?,
        })
    }

    async fn turn_state_cookie(
        &self,
        pod: &str,
    ) -> AdminStoreResult<Option<gateway_core::account::RoutingCookie>> {
        self.accounts.routing_cookie_value(pod).await.map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Unavailable,
                ENTITY,
                "routing cookie unavailable",
            )
        })
    }

    async fn recorded_routing_cookie(
        &self,
        account_id: &gateway_core::account::ProviderAccountId,
        model: &str,
        observation_id: i64,
    ) -> AdminStoreResult<Option<gateway_core::account::RoutingCookie>> {
        self.accounts
            .recorded_routing_cookie(account_id.as_str(), model, observation_id)
            .await
            .map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Unavailable,
                    ENTITY,
                    "recorded routing cookie unavailable",
                )
            })
    }

    async fn restore_routing_cookie(
        &self,
        cookie: &gateway_core::account::RoutingCookie,
    ) -> AdminStoreResult<()> {
        self.accounts.restore_routing_cookie(cookie).await.map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Unavailable,
                ENTITY,
                "restore routing cookie unavailable",
            )
        })
    }

    async fn account_replay_cookies(
        &self,
        account_id: &gateway_core::account::ProviderAccountId,
    ) -> AdminStoreResult<Vec<(String, String)>> {
        self.accounts
            .account_replay_cookies(account_id.as_str())
            .await
            .map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Unavailable,
                    ENTITY,
                    "account cookies unavailable",
                )
            })
    }

    async fn apply_turn_state_cookie(
        &self,
        account_id: &CoreProviderAccountId,
        model: &str,
        pod: &str,
        issued_at: Option<i64>,
        name: &str,
        value: &str,
        expires_at: i64,
        observation_id: i64,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        let mut transaction = self.pool.begin().await.map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Unavailable,
                ENTITY,
                "routing cookie transaction unavailable",
            )
        })?;
        // 固定精确到具体一条 Cookie：把它的签发时间一并记下，续约换值后固定即失效。
        // 调用方给 issued_at 时绑定观测到的那条，省略则取池内当前值。
        let applied = sqlx::query("update account_turn_states set cookie_override_pod = $3, cookie_override_issued_at = $4, cookie_override_name = $5, cookie_override_value = $6, cookie_override_expires_at = $7, cookie_override_observation_id = $8, next_probe_at = null where account_id = $1 and model = $2 and (config->>'cookieLockEnabled')::boolean and $5 <> '' and $6 <> ''")
            .bind(account_id.as_str())
            .bind(model)
            .bind(pod)
            .bind(issued_at)
            .bind(name)
            .bind(value)
            .bind(expires_at)
            .bind(observation_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Unavailable,
                    ENTITY,
                    "routing cookie apply unavailable",
                )
            })?;
        if applied.rows_affected() == 0 {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Conflict,
                ENTITY,
                "routing cookie expired or model mismatched",
            ));
        }
        let result: StoreResult<Revision> = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            append_admin_audit_event_in_transaction(
                &mut transaction,
                mutation_audit(
                    context,
                    "apply_turn_state_cookie",
                    "provider_account",
                    account_id.as_str(),
                    vec!["cookie_override_pod".to_owned()],
                ),
                revision,
            )
            .await?;
            Ok(revision)
        }
        .await;
        let revision = super::repository::finish_admin_transaction(
            transaction,
            result,
            "apply turn state cookie",
        )
        .await
        .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(AccountUpdateResult {
            account_id: account_id.clone(),
            config_revision: admin_revision(revision)?,
        })
    }

    async fn remove_turn_state_cookie(
        &self,
        account_id: &CoreProviderAccountId,
        model: &str,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        let mut transaction = self.pool.begin().await.map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Unavailable,
                ENTITY,
                "routing cookie transaction unavailable",
            )
        })?;
        let removed = sqlx::query("update account_turn_states set cookie_override_pod = null, cookie_override_issued_at = null, cookie_override_name = null, cookie_override_value = null, cookie_override_expires_at = null, cookie_override_observation_id = null, next_probe_at = null where account_id = $1 and model = $2 and cookie_override_value is not null")
            .bind(account_id.as_str())
            .bind(model)
            .execute(&mut *transaction)
            .await
            .map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Unavailable,
                    ENTITY,
                    "routing cookie removal unavailable",
                )
            })?;
        if removed.rows_affected() == 0 {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Conflict,
                ENTITY,
                "routing cookie override is already removed",
            ));
        }
        let result: StoreResult<Revision> = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            append_admin_audit_event_in_transaction(
                &mut transaction,
                mutation_audit(
                    context,
                    "remove_turn_state_cookie",
                    "provider_account",
                    account_id.as_str(),
                    vec!["cookie_override_pod".to_owned()],
                ),
                revision,
            )
            .await?;
            Ok(revision)
        }
        .await;
        let revision = super::repository::finish_admin_transaction(
            transaction,
            result,
            "remove turn state cookie",
        )
        .await
        .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(AccountUpdateResult {
            account_id: account_id.clone(),
            config_revision: admin_revision(revision)?,
        })
    }

    async fn request_turn_state_probe(
        &self,
        account_id: &CoreProviderAccountId,
        model: &str,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        let mut transaction = self.pool.begin().await.map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Unavailable,
                ENTITY,
                "turn state transaction unavailable",
            )
        })?;
        let result: StoreResult<Revision> = async {
            let config = serde_json::to_value(gateway_core::account::TurnStateConfig::default())
                .map_err(|_| postgres_unavailable("encode turn state configuration"))?;
            sqlx::query("insert into account_turn_states(account_id, model, config, upstream_account_id, upstream_user_id, manual_probe_requested_at) select id, $2, $3, upstream_account_id, upstream_user_id, $4 from provider_accounts where id = $1 and enabled and provider_kind = 'openai' and authentication_kind = 'oauth' on conflict (account_id, model) do update set manual_probe_requested_at = coalesce(account_turn_states.manual_probe_requested_at, excluded.manual_probe_requested_at) returning account_id")
                .bind(account_id.as_str()).bind(model).bind(config).bind(Utc::now().timestamp())
                .fetch_one(&mut *transaction).await.map_err(|_| postgres_unavailable("request turn state probe"))?;
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            append_admin_audit_event_in_transaction(&mut transaction, mutation_audit(context, "request_turn_state_probe", "provider_account", account_id.as_str(), vec!["turn_state_probe".to_owned()]), revision).await?;
            Ok(revision)
        }.await;
        let revision = super::repository::finish_admin_transaction(
            transaction,
            result,
            "request turn state probe",
        )
        .await
        .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(AccountUpdateResult {
            account_id: account_id.clone(),
            config_revision: admin_revision(revision)?,
        })
    }

    async fn turn_state_status(
        &self,
        account_id: Option<&str>,
    ) -> AdminStoreResult<Vec<gateway_core::account::TurnStateStatus>> {
        self.accounts
            .turn_state_statuses(account_id)
            .await
            .map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Unavailable,
                    ENTITY,
                    "turn state status unavailable",
                )
            })
    }

    async fn configure_turn_state(
        &self,
        account_id: &CoreProviderAccountId,
        model: &str,
        config: gateway_core::account::TurnStateConfig,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        if !config.is_valid() {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "invalid turn state configuration",
            ));
        }
        let proxy_count = sqlx::query_scalar::<_, i64>(
            "select count(*) from outbound_proxies where id = any($1::text[])",
        )
        .bind(&config.proxy_ids)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Unavailable,
                ENTITY,
                "proxy selection unavailable",
            )
        })?;
        if proxy_count as usize != config.proxy_ids.iter().collect::<BTreeSet<_>>().len() {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "selected proxy does not exist",
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Unavailable,
                ENTITY,
                "turn state transaction unavailable",
            )
        })?;
        let result: StoreResult<Revision> = async {
            let config = serde_json::to_value(config).map_err(|_| postgres_unavailable("encode turn state configuration"))?;
            // 自动探测与业务策略独立，保存配置不能撤销仍有效的已安装票。
            sqlx::query("insert into account_turn_states(account_id, model, config, upstream_account_id, upstream_user_id) select id, $2, $3, upstream_account_id, upstream_user_id from provider_accounts where id = $1 on conflict (account_id, model) do update set config = excluded.config, next_probe_at = null, manual_override = case when length(account_turn_states.turn_state_override) = (excluded.config->>'targetLength')::integer and account_turn_states.current_issued_at + (excluded.config->>'ttlSeconds')::bigint > extract(epoch from now()) then account_turn_states.manual_override else false end, turn_state_override = case when length(account_turn_states.turn_state_override) <> (excluded.config->>'targetLength')::integer or account_turn_states.current_issued_at + (excluded.config->>'ttlSeconds')::bigint <= extract(epoch from now()) then null else account_turn_states.turn_state_override end, candidate = case when length(account_turn_states.candidate) <> (excluded.config->>'targetLength')::integer or account_turn_states.candidate_issued_at + (excluded.config->>'ttlSeconds')::bigint <= extract(epoch from now()) then null else account_turn_states.candidate end")
                .bind(account_id.as_str()).bind(model).bind(config).execute(&mut *transaction).await
                .map_err(|_| postgres_unavailable("configure turn state"))?;
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            append_admin_audit_event_in_transaction(&mut transaction, mutation_audit(context, "configure_turn_state", "provider_account", account_id.as_str(), vec!["turn_state_rotation".to_owned()]), revision).await?;
            Ok(revision)
        }.await;
        let revision = super::repository::finish_admin_transaction(
            transaction,
            result,
            "configure turn state",
        )
        .await
        .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(AccountUpdateResult {
            account_id: account_id.clone(),
            config_revision: admin_revision(revision)?,
        })
    }

    async fn list_accounts(
        &self,
        query: AdminAccountListQuery,
        runtime: gateway_admin::model::accounts::AccountRuntimeSnapshot,
    ) -> AdminStoreResult<AccountPage> {
        if query.page == 0 {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "page number must be positive",
            ));
        }
        let now = Utc::now();
        let cooldown = runtime.cooldown;
        let now_system_time = std::time::SystemTime::from(now);
        let active_rate_limited_ids = cooldown
            .iter()
            .filter(|(_, cooldown)| cooldown.is_active(now_system_time))
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>();
        let page =
            load_admin_account_page(&self.pool, &query, now, active_rate_limited_ids).await?;
        let item_ids = page
            .accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        let mut groups_by_account = self.account_groups_by_account(&item_ids).await?;
        let items = page
            .accounts
            .into_iter()
            .map(|summary| {
                let account_id = summary.id.clone();
                let projection = account_status_projection(
                    &summary,
                    now.into(),
                    cooldown.get(&account_id).copied(),
                );
                let mut account = admin_account_record(summary)?;
                account.groups = groups_by_account.remove(&account_id).unwrap_or_default();
                Ok(AccountPageItem {
                    account,
                    projection,
                })
            })
            .collect::<AdminStoreResult<Vec<_>>>()?;
        Ok(AccountPage {
            config_revision: page.config_revision,
            items,
            total: page.total,
            summary: page.summary,
        })
    }

    async fn load_account(
        &self,
        account_id: &str,
        runtime: gateway_admin::model::accounts::AccountRuntimeSnapshot,
    ) -> AdminStoreResult<Option<AccountPageItem>> {
        let record = self
            .accounts
            .load_provider_account(account_id)
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let Some(record) = record else {
            return Ok(None);
        };
        let now = Utc::now();
        let cooldown = runtime.cooldown.get(account_id).copied();
        let projection = account_status_projection(&record.summary, now.into(), cooldown);
        let account_id = record.summary.id.clone();
        let mut groups = self
            .account_groups_by_account(std::slice::from_ref(&account_id))
            .await?;
        let mut account = admin_account_record(record.summary)?;
        account.groups = groups.remove(&account_id).unwrap_or_default();
        Ok(Some(AccountPageItem {
            account,
            projection,
        }))
    }

    async fn load_account_usage(
        &self,
        range: TimeRange,
        account_ids: &[String],
    ) -> AdminStoreResult<Vec<AccountUsage>> {
        let range = ObservabilityRange::new(range.start, range.end)
            .map_err(|error| admin_store_error(ENTITY, error))?;
        self.usage_observations(range, account_ids)
            .await?
            .into_iter()
            .map(admin_account_usage)
            .collect()
    }

    async fn load_account_usage_by_windows(
        &self,
        windows: &[AccountUsageWindowQuery],
    ) -> AdminStoreResult<Vec<AccountUsageWindowResult>> {
        self.usage_by_windows(windows).await
    }

    async fn load_quota_forecast_history(
        &self,
        window: &AccountUsageWindowQuery,
    ) -> AdminStoreResult<gateway_admin::model::quota_forecast_sampling::QuotaForecastHistory> {
        super::quota_forecast::load_history(&self.pool, &self.query_budget, window).await
    }

    async fn credential_details(
        &self,
        provider_kind: &ProviderKind,
        account_id: &CoreProviderAccountId,
    ) -> AdminStoreResult<Option<CredentialDetails>> {
        let (control_plane, account) = futures::try_join!(
            self.control_plane.load_control_plane(),
            self.accounts.load_provider_account(account_id.as_str()),
        )
        .map_err(|error| admin_store_error(ENTITY, error))?;
        account
            .filter(|record| record.summary.provider_kind == provider_kind.as_str())
            .map(|record| {
                Ok(CredentialDetails {
                    config_revision: admin_revision(control_plane.settings.config_revision)?,
                    credential: admin_account_record(record.summary)?,
                })
            })
            .transpose()
    }

    async fn load_credentials_for_export(
        &self,
        provider_kind: &ProviderKind,
        account_ids: &[CoreProviderAccountId],
    ) -> AdminStoreResult<Vec<ProviderExportCredentialInput>> {
        let ids = account_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>();
        validate_admin_account_ids(&ids).map_err(|error| admin_store_error(ENTITY, error))?;
        let mut credentials = Vec::with_capacity(account_ids.len());
        for account_id in account_ids {
            let record = self
                .accounts
                .load_provider_account(account_id.as_str())
                .await
                .map_err(|error| admin_store_error(ENTITY, error))?
                .ok_or_else(|| {
                    AdminStoreError::new(
                        AdminStoreErrorKind::NotFound,
                        ENTITY,
                        "one or more exported credentials do not exist",
                    )
                })?;
            if record.summary.provider_kind != provider_kind.as_str() {
                return Err(AdminStoreError::new(
                    AdminStoreErrorKind::NotFound,
                    ENTITY,
                    "one or more exported credentials belong to another Provider",
                ));
            }
            credentials.push(ProviderExportCredentialInput {
                account: admin_account_record(record.summary)?,
                provider_material: ProviderDocument::new(OpaqueProviderData::new(
                    record.provider_credentials_json.fields().clone(),
                )),
            });
        }
        Ok(credentials)
    }

    async fn commit_credential_import(
        &self,
        command: CredentialImportCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialImportResult> {
        self.commit_prepared_import(
            command.prepared,
            command.settings,
            context,
            "import_document",
            command.outbound_proxy,
        )
        .await
    }

    async fn commit_authorization(
        &self,
        command: AuthorizationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        match command.credential {
            AuthorizationCredentialCommit::Create(credential) => {
                let CredentialImportResult {
                    config_revision,
                    credential_ids,
                } = self
                    .commit_prepared_import(
                        PreparedCredentialImport {
                            provider_kind: credential.provider_kind.clone(),
                            credentials: vec![credential],
                        },
                        command.settings,
                        context,
                        "authorize",
                        command
                            .pending
                            .outbound_proxy_id()
                            .zip(command.pending.outbound_proxy())
                            .map(
                                |(id, proxy)| gateway_admin::model::proxies::ImportProxyBinding {
                                    id: id.to_owned(),
                                    proxy: proxy.clone(),
                                },
                            ),
                    )
                    .await?;
                let [account_id]: [CoreProviderAccountId; 1] =
                    credential_ids.try_into().map_err(|_| {
                        AdminStoreError::new(
                            AdminStoreErrorKind::Unavailable,
                            ENTITY,
                            "authorization import returned an unexpected account count",
                        )
                    })?;
                let details = self
                    .accounts
                    .load_provider_account(account_id.as_str())
                    .await
                    .map_err(|error| admin_store_error(ENTITY, error))?
                    .ok_or_else(|| {
                        AdminStoreError::new(
                            AdminStoreErrorKind::Unavailable,
                            ENTITY,
                            "authorized credential was not visible after commit",
                        )
                    })?;
                Ok(CredentialMutationResult {
                    config_revision,
                    account_id,
                    credential_revision: Some(admin_revision(details.summary.credential_revision)?),
                })
            }
            AuthorizationCredentialCommit::Reauthorize(prepared) => {
                if command.settings.is_some() {
                    return Err(AdminStoreError::new(
                        AdminStoreErrorKind::Invalid,
                        ENTITY,
                        "reauthorization cannot change account settings",
                    ));
                }
                self.commit_prepared_rotation(prepared, None, context, "reauthorize")
                    .await
            }
        }
    }

    async fn commit_credential_rotation(
        &self,
        command: CredentialRotationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        self.commit_prepared_rotation(
            command.prepared,
            command.settings,
            context,
            "rotate_credential",
        )
        .await
    }

    async fn commit_credential_refresh(
        &self,
        command: CredentialRotationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        if command.settings.is_some() {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "credential refresh cannot change account settings",
            ));
        }
        self.commit_prepared_rotation(command.prepared, None, context, "refresh_credential")
            .await
    }

    async fn update_account(
        &self,
        command: UpdateAccount,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        let account_id = CoreProviderAccountId::new(command.account_id.clone()).map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "invalid provider account ID",
            )
        })?;
        let mut changed_fields = vec![
            "enabled".to_owned(),
            "concurrency_limit".to_owned(),
            "weight".to_owned(),
            "groups".to_owned(),
        ];
        if command.model_access.is_some() {
            changed_fields.push("model_access".to_owned());
        }
        if command.outbound_proxy.is_some() {
            changed_fields.push("outbound_proxy".to_owned());
        }
        if command.notes.is_some() {
            changed_fields.push("notes".to_owned());
        }
        if command.turn_state_override.is_some() {
            changed_fields.push("turn_state_override".to_owned());
        }
        let config_revision = self
            .accounts
            .batch_update_provider_accounts_admin(BatchUpdateProviderAccountsAdmin {
                account_ids: vec![command.account_id.clone()],
                notes: command.notes,
                turn_state_override: command.turn_state_override,
                enabled: Some(command.enabled),
                concurrency_limit: Some(command.concurrency_limit),
                weight: Some(command.weight),
                model_access: command.model_access,
                group_ids: Some(command.group_ids),
                outbound_proxy: command.outbound_proxy,
                audit: mutation_audit(
                    context,
                    "update",
                    "provider_account",
                    &command.account_id,
                    changed_fields,
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(admin_revision)?;
        Ok(AccountUpdateResult {
            config_revision,
            account_id,
        })
    }

    async fn lower_concurrency_limit(
        &self,
        account_id: &CoreProviderAccountId,
        limit: AccountConcurrencyLimit,
        context: &MutationContext,
    ) -> AdminStoreResult<Option<AccountUpdateResult>> {
        let mut transaction = self.pool.begin().await.map_err(|_| {
            admin_store_error(ENTITY, postgres_unavailable("begin concurrency reduction"))
        })?;
        let result =
            async {
                // 与管理写入采用相同锁顺序；锁住默认值后再检查账号最新设置，避免把旧快照写回。
                let default_limit: i64 = sqlx::query_scalar(
                "select max_concurrent_per_account from runtime_settings where id = 1 for update"
            ).fetch_one(&mut *transaction).await
                .map_err(|_| postgres_unavailable("lock default concurrency"))?;
                let changed = sqlx::query_scalar::<_, String>(
                    "update provider_accounts set concurrency_limit = $2, updated_at = now()
                 where id = $1 and enabled = true and coalesce(concurrency_limit, $3) > $2
                 returning id",
                )
                .bind(account_id.as_str())
                .bind(i64::from(limit.get()))
                .bind(default_limit)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|_| postgres_unavailable("lower account concurrency"))?;
                if changed.is_none() {
                    return Ok(None);
                }
                let revision = bump_config_revision_in_transaction(&mut transaction).await?;
                append_admin_audit_event_in_transaction(
                    &mut transaction,
                    mutation_audit(
                        context,
                        "adapt_concurrency",
                        "provider_account",
                        account_id.as_str(),
                        vec!["concurrency_limit".to_owned()],
                    ),
                    revision,
                )
                .await?;
                Ok(Some(revision))
            }
            .await;
        let revision = super::repository::finish_admin_transaction(
            transaction,
            result,
            "lower account concurrency",
        )
        .await
        .map_err(|error| admin_store_error(ENTITY, error))?;
        revision
            .map(|revision| {
                Ok(AccountUpdateResult {
                    config_revision: admin_revision(revision)?,
                    account_id: account_id.clone(),
                })
            })
            .transpose()
    }

    async fn recover_account(
        &self,
        account_id: &CoreProviderAccountId,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        if let Some(cooldowns) = self.cooldowns.as_deref() {
            cooldowns.clear_all(account_id).await.map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Unavailable,
                    ENTITY,
                    "provider account cooldown cleanup failed",
                )
            })?;
        }
        let config_revision = self
            .accounts
            .recover_provider_account_admin(RecoverProviderAccount {
                account_id: account_id.as_str().to_owned(),
                audit: mutation_audit(
                    context,
                    "recover",
                    "provider_account",
                    account_id.as_str(),
                    vec!["status".to_owned(), "quota".to_owned()],
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(admin_revision)?;
        Ok(AccountUpdateResult {
            config_revision,
            account_id: account_id.clone(),
        })
    }

    async fn batch_update_accounts(
        &self,
        command: BatchUpdateAccounts,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountsUpdateResult> {
        let account_ids = command
            .account_ids
            .iter()
            .map(|id| {
                CoreProviderAccountId::new(id.clone()).map_err(|_| {
                    AdminStoreError::new(
                        AdminStoreErrorKind::Invalid,
                        ENTITY,
                        "invalid provider account ID",
                    )
                })
            })
            .collect::<AdminStoreResult<Vec<_>>>()?;
        let audit_target = if command.account_ids.len() == 1 {
            command.account_ids[0].clone()
        } else {
            "provider_accounts".to_owned()
        };
        let mut changed_fields = Vec::new();
        for (changed, field) in [
            (command.enabled.is_some(), "enabled"),
            (command.concurrency_limit.is_some(), "concurrency_limit"),
            (command.weight.is_some(), "weight"),
            (command.group_ids.is_some(), "groups"),
        ] {
            if changed {
                changed_fields.push(field.to_owned());
            }
        }
        if command.model_access.is_some() {
            changed_fields.push("model_access".to_owned());
        }
        if command.outbound_proxy.is_some() {
            changed_fields.push("outbound_proxy".to_owned());
        }
        if command.turn_state_override.is_some() {
            changed_fields.push("turn_state_override".to_owned());
        }
        let config_revision = self
            .accounts
            .batch_update_provider_accounts_admin(BatchUpdateProviderAccountsAdmin {
                account_ids: command.account_ids,
                notes: None,
                turn_state_override: command.turn_state_override,
                enabled: command.enabled,
                concurrency_limit: command.concurrency_limit,
                weight: command.weight,
                model_access: command.model_access,
                group_ids: command.group_ids,
                outbound_proxy: command.outbound_proxy,
                audit: mutation_audit(
                    context,
                    "batch_update",
                    "provider_account",
                    &audit_target,
                    changed_fields,
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(admin_revision)?;
        Ok(AccountsUpdateResult {
            config_revision,
            account_ids,
        })
    }

    async fn delete_accounts(
        &self,
        command: DeleteAccounts,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminRevision> {
        let first_account_id = command.account_ids.first().ok_or_else(|| {
            AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "account deletion requires at least one account ID",
            )
        })?;
        let scope = self.required_scope(first_account_id).await?;
        let audit_target = if command.account_ids.len() == 1 {
            first_account_id.clone()
        } else {
            "provider_accounts".to_owned()
        };
        self.accounts
            .delete_provider_accounts_admin(DeleteProviderAccounts {
                scope,
                account_ids: command.account_ids,
                audit: mutation_audit(
                    context,
                    "delete",
                    "provider_account",
                    &audit_target,
                    Vec::new(),
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(admin_revision)
    }

    async fn record_credential_export(
        &self,
        account_ids: &[CoreProviderAccountId],
        context: &MutationContext,
    ) -> AdminStoreResult<()> {
        let ids = account_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>();
        validate_admin_account_ids(&ids).map_err(|error| admin_store_error(ENTITY, error))?;
        for account_id in &ids {
            if self
                .accounts
                .load_provider_account(account_id)
                .await
                .map_err(|error| admin_store_error(ENTITY, error))?
                .is_none()
            {
                return Err(AdminStoreError::new(
                    AdminStoreErrorKind::NotFound,
                    ENTITY,
                    "one or more exported credentials do not exist",
                ));
            }
        }
        let control_plane = self
            .control_plane
            .load_control_plane()
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let revision = control_plane.settings.config_revision;
        let mut transaction = self.accounts.pool.begin().await.map_err(|_| {
            admin_store_error(
                ENTITY,
                postgres_unavailable("begin credential export audit"),
            )
        })?;
        let result = async {
            for account_id in &ids {
                append_admin_audit_event_in_transaction(
                    &mut transaction,
                    mutation_audit(
                        context,
                        "export_credentials",
                        "provider_account",
                        account_id,
                        Vec::new(),
                    ),
                    revision,
                )
                .await?;
            }
            Ok(())
        }
        .await;
        finish_admin_transaction(transaction, result, "credential export audit")
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
    }
}
