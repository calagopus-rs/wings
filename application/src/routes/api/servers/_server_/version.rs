use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, api::servers::_server_::GetServer},
    };
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
    pub async fn route(server: GetServer, Query(data): Query<Params>) -> ApiResponseResult {
        match data.game {
            Game::MinecraftJava => {
                let mut jar: Option<PathBuf> = None;
                for (key, value) in &server.configuration.read().await.environment {
                    if let Some(value_str) = value.as_str()
                        && key.contains("JAR")
                        && value_str.contains(".jar")
                    {
                        jar = Some(value_str.into());
                        break;
                    }
                }

                if jar.is_none() {
                    'forge: {
                        let path = Path::new("libraries/net/minecraftforge/forge");

                        if server
                            .filesystem
                            .async_metadata(path)
                            .await
                            .is_ok_and(|m| m.is_dir())
                        {
                            let mut entries = server.filesystem.async_read_dir(path).await?;

                            while let Some(Ok((_, entry))) = entries.next_entry().await {
                                if let Ok(mut entries) =
                                    server.filesystem.async_read_dir(path.join(&entry)).await
                                {
                                    while let Some(Ok((_, sub_entry))) = entries.next_entry().await
                                    {
                                        if sub_entry.ends_with("-server.jar")
                                            || sub_entry.ends_with("-universal.jar")
                                        {
                                            jar = Some(path.join(entry).join(sub_entry));
                                            break 'forge;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if jar.is_none() {
                    'neoforge: {
                        let path = Path::new("libraries/net/neoforged/neoforge");

                        if server
                            .filesystem
                            .async_metadata(path)
                            .await
                            .is_ok_and(|m| m.is_dir())
                        {
                            let mut entries = server.filesystem.async_read_dir(path).await?;

                            while let Some(Ok((_, entry))) = entries.next_entry().await {
                                if let Ok(mut entries) =
                                    server.filesystem.async_read_dir(path.join(&entry)).await
                                {
                                    while let Some(Ok((_, sub_entry))) = entries.next_entry().await
                                    {
                                        if sub_entry.ends_with("-server.jar")
                                            || sub_entry.ends_with("-universal.jar")
                                        {
                                            jar = Some(path.join(entry).join(sub_entry));
                                            break 'neoforge;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                let mut file = match server
                    .filesystem
                    .async_open(jar.unwrap_or_else(|| PathBuf::from("server.jar")))
                    .await
                {
                    Ok(file) => file,
                    Err(_) => {
                        return ApiResponse::error("version not found")
                            .with_status(StatusCode::NOT_FOUND)
                            .ok();
                    }
                };

                let mut hasher = sha2::Sha256::new();
                let mut buffer = vec![0; crate::BUFFER_SIZE];

                loop {
                    match file.read(&mut buffer).await? {
                        0 => break,
                        bytes_read => hasher.update(&buffer[..bytes_read]),
                    }
                }

                ApiResponse::json(Response {
                    hash: format!("{:x}", hasher.finalize()),
                })
                .ok()
            }
        }
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
