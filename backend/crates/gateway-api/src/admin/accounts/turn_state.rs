//! 状态查询不含令牌正文，管理员主动复制使用独立的按需读取入口。

use crate::auth::SessionState;
use gateway_core::account::TurnStateConfig;

use super::*;

pub(super) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/accounts/turn-state", get(status::<S>))
        .route("/api/admin/accounts/turn-state/probe", post(probe::<S>))
        .route("/api/admin/accounts/turn-state/apply", post(apply::<S>))
        .route("/api/admin/accounts/turn-state/remove", post(remove::<S>))
        .route("/api/admin/accounts/turn-state/copy", post(copy::<S>))
        .route("/api/admin/accounts/turn-state/preview", post(preview::<S>))
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProbeRequest {
    account_id: String,
    model: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApplyRequest {
    account_id: String,
    model: String,
    issued_at: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewRequest {
    config: TurnStateConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CopyResponse {
    value: String,
    issued_at: i64,
}

async fn copy<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<ApplyRequest>,
) -> Result<Response, AdminError>
where
    S: SessionState + Send + Sync,
{
    require_account_id(&request.account_id, "accountId").map_err(map_wire_error)?;
    let account_id = ProviderAccountId::new(request.account_id)
        .map_err(|_| map_wire_error(WireValidationError::new("accountId")))?;
    let token = state
        .admin_services()
        .accounts()
        .turn_state_token(account_id, request.model, request.issued_at)
        .await
        .map_err(map_service_error)?;
    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(CopyResponse {
            value: token.value,
            issued_at: token.issued_at,
        }),
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn preview<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<PreviewRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let preview = state
        .admin_services()
        .accounts()
        .turn_state_probe_preview(request.config)
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(preview),
    ))
}

async fn apply<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<ApplyRequest>,
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
        .apply_turn_state(
            &auth.context().mutation_context(),
            account_id,
            request.model,
            request.issued_at,
        )
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(UpdatedAccountData::from(result)),
    ))
}

async fn remove<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<ApplyRequest>,
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
        .remove_turn_state(
            &auth.context().mutation_context(),
            account_id,
            request.model,
            request.issued_at,
        )
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(UpdatedAccountData::from(result)),
    ))
}

async fn probe<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<ProbeRequest>,
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
        .request_turn_state_probe(
            &auth.context().mutation_context(),
            account_id,
            request.model,
        )
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::ACCEPTED,
        AdminEnvelope::ok(UpdatedAccountData::from(result)),
    ))
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
