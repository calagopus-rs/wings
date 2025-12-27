use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState},
        server::permissions::Permissions,
    };
    use serde::{Deserialize, Serialize};
    use std::collections::HashSet;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        user: uuid::Uuid,
        servers: HashSet<uuid::Uuid>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = CONFLICT, body = inline(ApiError)),
    ), request_body = inline(Payload))]
    pub async fn route(
        state: GetState,
        axum::Json(data): axum::Json<Payload>,
    ) -> ApiResponseResult {
        if data.servers.is_empty() {
            for server in state.server_manager.get_servers().await.iter() {
                server
                    .user_permissions
                    .set_permissions(data.user, Permissions::default(), &[] as &[&str])
                    .await;
            }
        } else {
            for server in state.server_manager.get_servers().await.iter() {
                if data.servers.contains(&server.uuid) {
                    server
                        .user_permissions
                        .set_permissions(data.user, Permissions::default(), &[] as &[&str])
                        .await;
                }
            }
        }

        ApiResponse::json(Response {}).ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
