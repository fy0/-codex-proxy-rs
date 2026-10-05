mod artifacts;
mod distribution;
mod instances;
mod management;

pub(super) use management::model_router;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IdRequest {
    id: String,
}

pub(super) fn router<S: crate::auth::SessionState + Clone + Send + Sync + 'static>()
-> axum::Router<S> {
    artifacts::router()
        .merge(distribution::router())
        .merge(instances::router())
        .merge(management::router())
}
