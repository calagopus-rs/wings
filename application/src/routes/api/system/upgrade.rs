use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::ApiError,
    };
    use axum::http::{HeaderMap, HeaderName, StatusCode};
    use serde::{Deserialize, Serialize};
    use sha1::Digest;
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize)]
    pub struct Payload {
        url: String,
        headers: HashMap<String, String>,
        sha256: String,

        restart_command: String,
        restart_command_args: Vec<String>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {}

    #[utoipa::path(post, path = "/", responses(
        (status = ACCEPTED, body = inline(Response)),
        (status = CONFLICT, body = inline(ApiError)),
    ), request_body = inline(Payload))]
    pub async fn route(axum::Json(data): axum::Json<Payload>) -> ApiResponseResult {
        let current_exe = std::env::current_exe()?;
        let current_exe_parent = match current_exe.parent() {
            Some(parent) => parent,
            None => {
                return ApiResponse::error("unable to find parent of current exe")
                    .with_status(StatusCode::BAD_REQUEST)
                    .ok();
            }
        };
        let current_exe_filename = match current_exe.file_name() {
            Some(filename) => filename,
            None => {
                return ApiResponse::error("unable to find file name of current exe")
                    .with_status(StatusCode::BAD_REQUEST)
                    .ok();
            }
        };

        let tmp_file =
            current_exe_parent.join(format!("{}.upgrade", current_exe_filename.display()));

        let mut headers = HeaderMap::new();
        headers.reserve(data.headers.len());

        for (key, value) in data.headers {
            headers.insert(
                match HeaderName::try_from(key) {
                    Ok(v) => v,
                    Err(_) => continue,
                },
                match value.parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                },
            );
        }

        let client = reqwest::Client::builder()
            .user_agent("Pterodactyl Panel (https://pterodactyl.io)")
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()?;

        let mut response = client.get(data.url).headers(headers).send().await?;
        let mut file = tokio::fs::File::options()
            .create(true)
            .write(true)
            .truncate(true)
            .read(true)
            .mode(0o766)
            .open(&tmp_file)
            .await?;

        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk).await?;
        }

        file.sync_all().await?;
        drop(response);

        file.seek(std::io::SeekFrom::Start(0)).await?;

        let mut hasher = sha2::Sha256::new();
        let mut buffer = vec![0; crate::BUFFER_SIZE];

        loop {
            match file.read(&mut buffer).await? {
                0 => break,
                bytes_read => hasher.update(&buffer[..bytes_read]),
            }
        }

        drop(file);

        if format!("{:x}", hasher.finalize()) != data.sha256 {
            tokio::fs::remove_file(tmp_file).await.ok();

            return ApiResponse::error("downloaded file does not match provided sha256")
                .with_status(StatusCode::CONFLICT)
                .ok();
        }

        tokio::spawn(async move {
            let run = async || -> Result<(), anyhow::Error> {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                tokio::fs::rename(tmp_file, current_exe).await?;

                #[allow(clippy::zombie_processes)]
                std::process::Command::new(data.restart_command)
                    .args(data.restart_command_args)
                    .spawn()?;

                Ok(())
            };

            if let Err(err) = run().await {
                tracing::error!("error while upgrading binary: {:#?}", err)
            }
        });

        ApiResponse::json(Response {})
            .with_status(StatusCode::ACCEPTED)
            .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
