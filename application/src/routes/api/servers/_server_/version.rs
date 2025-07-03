use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::routes::{ApiError, api::servers::_server_::GetServer};
    use axum::http::StatusCode;
    use axum_extra::extract::Query;
    use serde::{Deserialize, Serialize};
    use sha2::Digest;
    use std::path::{Path, PathBuf};
    use tokio::io::AsyncReadExt;
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize, Default, Clone, Copy)]
    #[serde(rename_all = "snake_case")]
    #[schema(rename_all = "snake_case")]
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
        (status = NOT_FOUND, body = ApiError),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
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
                let mut jar = PathBuf::from("server.jar");
                for (key, value) in &server.configuration.read().await.environment {
                    if let Some(value_str) = value.as_str() {
                        if key.contains("JAR") && value_str.contains(".jar") {
                            jar = value_str.into();

                            break;
                        }
                    }
                }

                'forge: {
                    let path = Path::new("libraries/net/minecraftforge/forge");

                    if server
                        .filesystem
                        .metadata(path)
                        .await
                        .is_ok_and(|m| m.is_dir())
                    {
                        let mut entries = server.filesystem.read_dir(path).await.unwrap();

                        while let Some(Ok(entry)) = entries.next_entry().await {
                            if let Ok(mut entries) =
                                server.filesystem.read_dir(path.join(&entry)).await
                            {
                                while let Some(Ok(sub_entry)) = entries.next_entry().await {
                                    if sub_entry.ends_with("-server.jar")
                                        || sub_entry.ends_with("-universal.jar")
                                    {
                                        jar = path.join(entry).join(sub_entry);
                                        break 'forge;
                                    }
                                }
                            }
                        }
                    }
                }

                'neoforge: {
                    let path = Path::new("libraries/net/neoforged/neoforge");

                    if server
                        .filesystem
                        .metadata(path)
                        .await
                        .is_ok_and(|m| m.is_dir())
                    {
                        let mut entries = server.filesystem.read_dir(path).await.unwrap();

                        while let Some(Ok(entry)) = entries.next_entry().await {
                            if let Ok(mut entries) =
                                server.filesystem.read_dir(path.join(&entry)).await
                            {
                                while let Some(Ok(sub_entry)) = entries.next_entry().await {
                                    if sub_entry.ends_with("-server.jar")
                                        || sub_entry.ends_with("-universal.jar")
                                    {
                                        jar = path.join(entry).join(sub_entry);
                                        break 'neoforge;
                                    }
                                }
                            }
                        }
                    }
                }

                let mut file = match server.filesystem.open(&jar).await {
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
                        serde_json::to_value(Response {
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
