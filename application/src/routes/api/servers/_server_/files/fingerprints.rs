use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::api::servers::_server_::GetServer,
    };
    use axum_extra::extract::Query;
    use serde::{Deserialize, Serialize};
    use sha1::Digest;
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    use utoipa::ToSchema;

    #[derive(ToSchema, Deserialize, Clone, Copy)]
    #[serde(rename_all = "lowercase")]
    #[schema(rename_all = "lowercase")]
    pub enum Algorithm {
        Md5,
        Crc32,
        Sha1,
        Sha224,
        Sha256,
        Sha384,
        Sha512,
        Curseforge,
    }

    #[derive(ToSchema, Deserialize)]
    pub struct Params {
        algorithm: Algorithm,
        files: Vec<String>,
    }

    #[derive(ToSchema, Serialize)]
    struct Response {
        fingerprints: HashMap<String, String>,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = inline(Response)),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
        (
            "algorithm" = Algorithm, Query,
            description = "The algorithm to use for the fingerprint",
        ),
        (
            "files" = Vec<String>, Query,
            description = "The list of files to fingerprint",
        ),
    ))]
    pub async fn route(server: GetServer, Query(data): Query<Params>) -> ApiResponseResult {
        let mut fingerprint_handles = Vec::new();
        for path_raw in data.files {
            let path = match server.filesystem.async_canonicalize(&path_raw).await {
                Ok(path) => path,
                Err(_) => continue,
            };
            let metadata = match server.filesystem.async_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if !metadata.is_file() || server.filesystem.is_ignored(&path, metadata.is_dir()).await {
                continue;
            }

            let mut file = match server.filesystem.async_open(&path).await {
                Ok(file) => file,
                Err(_) => continue,
            };

            let mut buffer = vec![0; crate::BUFFER_SIZE];

            fingerprint_handles.push(async move {
                Ok::<_, std::io::Error>((
                    path_raw,
                    match data.algorithm {
                        Algorithm::Md5 => {
                            let mut hasher = md5::Context::new();

                            loop {
                                let bytes_read = file.read(&mut buffer).await?;
                                if bytes_read == 0 {
                                    break;
                                }

                                hasher.consume(&buffer[..bytes_read]);
                            }

                            format!("{:x}", hasher.finalize())
                        }
                        Algorithm::Crc32 => {
                            let mut hasher = crc32fast::Hasher::new();

                            loop {
                                let bytes_read = file.read(&mut buffer).await?;
                                if bytes_read == 0 {
                                    break;
                                }

                                hasher.update(&buffer[..bytes_read]);
                            }

                            format!("{:x}", hasher.finalize())
                        }
                        Algorithm::Sha1 => {
                            let mut hasher = sha1::Sha1::new();

                            loop {
                                let bytes_read = file.read(&mut buffer).await?;
                                if bytes_read == 0 {
                                    break;
                                }

                                hasher.update(&buffer[..bytes_read]);
                            }

                            format!("{:x}", hasher.finalize())
                        }
                        Algorithm::Sha224 => {
                            let mut hasher = sha2::Sha224::new();

                            loop {
                                let bytes_read = file.read(&mut buffer).await?;
                                if bytes_read == 0 {
                                    break;
                                }

                                hasher.update(&buffer[..bytes_read]);
                            }

                            format!("{:x}", hasher.finalize())
                        }
                        Algorithm::Sha256 => {
                            let mut hasher = sha2::Sha256::new();

                            loop {
                                let bytes_read = file.read(&mut buffer).await?;
                                if bytes_read == 0 {
                                    break;
                                }

                                hasher.update(&buffer[..bytes_read]);
                            }

                            format!("{:x}", hasher.finalize())
                        }
                        Algorithm::Sha384 => {
                            let mut hasher = sha2::Sha384::new();

                            loop {
                                let bytes_read = file.read(&mut buffer).await?;
                                if bytes_read == 0 {
                                    break;
                                }

                                hasher.update(&buffer[..bytes_read]);
                            }

                            format!("{:x}", hasher.finalize())
                        }
                        Algorithm::Sha512 => {
                            let mut hasher = sha2::Sha512::new();

                            loop {
                                let bytes_read = file.read(&mut buffer).await?;
                                if bytes_read == 0 {
                                    break;
                                }

                                hasher.update(&buffer[..bytes_read]);
                            }

                            format!("{:x}", hasher.finalize())
                        }
                        Algorithm::Curseforge => {
                            #[inline]
                            fn is_ignored_in_curseforge_fingerprint(b: u8) -> bool {
                                b == b'\t' || b == b'\n' || b == b'\r' || b == b' '
                            }

                            const MULTIPLEX: u32 = 1540483477;

                            let mut normalized_length: u32 = 0;

                            loop {
                                let bytes_read = file.read(&mut buffer).await?;
                                if bytes_read == 0 {
                                    break;
                                }

                                for &b in &buffer[..bytes_read] {
                                    if !is_ignored_in_curseforge_fingerprint(b) {
                                        normalized_length = normalized_length.wrapping_add(1);
                                    }
                                }
                            }

                            file.seek(std::io::SeekFrom::Start(0)).await?;

                            let mut num2: u32 = 1 ^ normalized_length;
                            let mut num3: u32 = 0;
                            let mut num4: u32 = 0;

                            loop {
                                let bytes_read = file.read(&mut buffer).await?;
                                if bytes_read == 0 {
                                    break;
                                }

                                for &b in &buffer[..bytes_read] {
                                    if !is_ignored_in_curseforge_fingerprint(b) {
                                        num3 |= (b as u32) << num4;
                                        num4 = num4.wrapping_add(8);

                                        if num4 == 32 {
                                            let num6 = num3.wrapping_mul(MULTIPLEX);
                                            let num7 =
                                                (num6 ^ (num6 >> 24)).wrapping_mul(MULTIPLEX);

                                            num2 = num2.wrapping_mul(MULTIPLEX) ^ num7;
                                            num3 = 0;
                                            num4 = 0;
                                        }
                                    }
                                }
                            }

                            if num4 > 0 {
                                num2 = (num2 ^ num3).wrapping_mul(MULTIPLEX);
                            }

                            let num6 = (num2 ^ (num2 >> 13)).wrapping_mul(MULTIPLEX);
                            let result = num6 ^ (num6 >> 15);

                            result.to_string()
                        }
                    },
                ))
            });
        }

        let joined_fingerprints = futures::future::join_all(fingerprint_handles).await;

        ApiResponse::json(Response {
            fingerprints: joined_fingerprints
                .into_iter()
                .filter_map(Result::ok)
                .collect(),
        })
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
