use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use axum_extra::extract::Query;
    use serde::{Deserialize, Serialize};
    use sha2::Digest;
    use tokio::{fs::File, io::AsyncReadExt};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize, Default, Clone, Copy)]
    #[serde(rename_all = "lowercase")]
    #[schema(rename_all = "lowercase")]
    pub enum Game {
        #[default]
        MinecraftJava,
    }

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        #[serde(default)]
        game: Game,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {
        hash: String,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = NOT_FOUND, body = inline(ApiError)),
    ), params(
        (
            "game" = Game, Query,
            description = "The game logic to use for the sha256 hash",
        ),
    ))]
    pub async fn route(
        server: GetServer,
        Query(data): Query<Params>,
    ) -> (StatusCode, axum::Json<serde_json::Value>) {
        match data.game {
            Game::MinecraftJava => {
                let mut jar = server.filesystem.base_path.join("server.jar");
                for (key, value) in &server.configuration.read().await.environment {
                    if let Some(value_str) = value.as_str() {
                        if key.contains("JAR") && value_str.contains(".jar") {
                            if let Some(path) = server.filesystem.safe_path(value_str) {
                                jar = path;
                            }

                            break;
                        }
                    }
                }

                'forge: {
                    if server
                        .filesystem
                        .base_path
                        .join("libraries/net/minecraftforge/forge")
                        .is_dir()
                    {
                        let mut entries = tokio::fs::read_dir(
                            server
                                .filesystem
                                .base_path
                                .join("libraries/net/minecraftforge/forge"),
                        )
                        .await
                        .unwrap();

                        while let Some(entry) = entries.next_entry().await.unwrap() {
                            if let Ok(mut entries) = tokio::fs::read_dir(entry.path()).await {
                                while let Some(entry) = entries.next_entry().await.unwrap() {
                                    let name_str = entry.file_name();
                                    let name_str = name_str.to_string_lossy();

                                    if name_str.ends_with("-server.jar")
                                        || name_str.ends_with("-universal.jar")
                                    {
                                        jar = entry.path();
                                        break 'forge;
                                    }
                                }
                            }
                        }
                    }
                }

                'neoforge: {
                    if server
                        .filesystem
                        .base_path
                        .join("libraries/net/neoforged/neoforge")
                        .is_dir()
                    {
                        let mut entries = tokio::fs::read_dir(
                            server
                                .filesystem
                                .base_path
                                .join("libraries/net/neoforged/neoforge"),
                        )
                        .await
                        .unwrap();

                        while let Some(entry) = entries.next_entry().await.unwrap() {
                            if let Ok(mut entries) = tokio::fs::read_dir(entry.path()).await {
                                while let Some(entry) = entries.next_entry().await.unwrap() {
                                    let name_str = entry.file_name();
                                    let name_str = name_str.to_string_lossy();

                                    if name_str.ends_with("-server.jar")
                                        || name_str.ends_with("-universal.jar")
                                    {
                                        jar = entry.path();
                                        break 'neoforge;
                                    }
                                }
                            }
                        }
                    }
                }

                let mut file = match File::open(&jar).await {
                    Ok(file) => file,
                    Err(_) => {
                        return (
                            StatusCode::NOT_FOUND,
                            axum::Json(ApiError::new("version not found").to_json()),
                        );
                    }
                };

                let mut hasher = sha2::Sha256::new();
                let mut buffer = [0; 8192];
                loop {
                    let bytes_read = file.read(&mut buffer).await.unwrap();
                    if bytes_read == 0 {
                        break;
                    }

                    hasher.update(&buffer[..bytes_read]);
                }

                (
                    StatusCode::OK,
                    axum::Json(
                        serde_json::to_value(&Response {
                            hash: format!("{:x}", hasher.finalize()),
                        })
                        .unwrap(),
                    ),
                )
            }
        }
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
