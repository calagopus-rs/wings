use super::State;
use utoipa_axum::router::OpenApiRouter;

mod chmod;
mod compress;
mod contents;
mod copy;
mod create_directory;
mod decompress;
mod delete;
mod fingerprints;
mod list;
mod list_directory;
mod operations;
mod pull;
mod rename;
mod search;
mod write;

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .nest("/contents", contents::router(state))
        .nest("/list-directory", list_directory::router(state))
        .nest("/list", list::router(state))
        .nest("/rename", rename::router(state))
        .nest("/copy", copy::router(state))
        .nest("/write", write::router(state))
        .nest("/create-directory", create_directory::router(state))
        .nest("/delete", delete::router(state))
        .nest("/chmod", chmod::router(state))
        .nest("/search", search::router(state))
        .nest("/fingerprints", fingerprints::router(state))
        .nest("/pull", pull::router(state))
        .nest("/compress", compress::router(state))
        .nest("/decompress", decompress::router(state))
        .nest("/operations", operations::router(state))
        .with_state(state.clone())
}
