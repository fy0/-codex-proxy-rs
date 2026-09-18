//! 轮换配置与不含令牌正文的状态查询。

use crate::auth::SessionState;
use gateway_core::account::TurnStateConfig;

use super::*;

pub(super) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/accounts/turn-state", get(status::<S>))
        .route(
            "/api/admin/accounts/turn-state/configure",
            post(configure::<S>),
        )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StatusQuery {
    account_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfigureRequest {
    account_id: String,
    model: String,
    config: TurnStateConfig,
}

async fn status<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<StatusQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let data = state
        .admin_services()
        .accounts()
        .turn_state_status(query.account_id.as_deref())
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn configure<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<ConfigureRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    require_account_id(&request.account_id, "accountId").map_err(map_wire_error)?;
    let account_id = ProviderAccountId::new(request.account_id)
        .map_err(|_| map_wire_error(WireValidationError::new("accountId")))?;
    let result = state
        .admin_services()
        .accounts()
        .configure_turn_state(
            &auth.context().mutation_context(),
            account_id,
            request.model,
            request.config,
        )
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(UpdatedAccountData::from(result)),
    ))
}
