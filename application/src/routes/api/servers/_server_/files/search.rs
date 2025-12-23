use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod post {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::{ApiError, GetState, api::servers::_server_::GetServer},
    };
    use axum::http::StatusCode;
    use ignore::{gitignore::GitignoreBuilder, overrides::OverrideBuilder};
    use serde::{Deserialize, Serialize};
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    use tokio::{
        io::{AsyncReadExt, AsyncSeekExt},
        sync::Mutex,
    };
    use utoipa::ToSchema;

    async fn search_in_stream(
        mut reader: impl tokio::io::AsyncRead + Unpin,
        substr: &str,
        case_insensitive: bool,
    ) -> Result<bool, std::io::Error> {
        if substr.is_empty() {
            return Ok(true);
        }

        let needle_owned;
        let needle_bytes = if case_insensitive {
            needle_owned = substr.to_lowercase();
            needle_owned.as_bytes()
        } else {
            substr.as_bytes()
        };

        let needle_len = needle_bytes.len();

        let mut buffer = vec![0; std::cmp::max(crate::BUFFER_SIZE, needle_len) + needle_len];
        let mut valid_bytes = 0;

        let finder = if !case_insensitive {
            Some(memchr::memmem::Finder::new(needle_bytes))
        } else {
            None
        };

        loop {
            let n = reader
                .read(&mut buffer[valid_bytes..valid_bytes + crate::BUFFER_SIZE])
                .await?;

            if crate::unlikely(n == 0) {
                return Ok(false);
            }

            let data_end = valid_bytes + n;
            let active_slice = &buffer[..data_end];

            let found = if let Some(f) = &finder {
                f.find(active_slice).is_some()
            } else {
                active_slice.windows(needle_len).any(|window| {
                    window
                        .iter()
                        .zip(needle_bytes.iter())
                        .all(|(a, b)| a.eq_ignore_ascii_case(b))
                })
            };

            if crate::unlikely(found) {
                return Ok(true);
            }

            if data_end >= needle_len {
                let keep_len = needle_len - 1;
                buffer.copy_within(data_end - keep_len..data_end, 0);
                valid_bytes = keep_len;
            } else {
                valid_bytes = data_end;
            }
        }
    }

    #[derive(ToSchema, Deserialize)]
    pub struct PayloadV1 {
        #[serde(default)]
        root: compact_str::CompactString,
        query: compact_str::CompactString,
        #[serde(default)]
        include_content: bool,

        limit: Option<usize>,
        max_size: Option<u64>,
    }

    #[derive(ToSchema, Deserialize)]
    pub struct PathFilter {
        include: Vec<compact_str::CompactString>,
        #[serde(default)]
        exclude: Vec<compact_str::CompactString>,
    }

    #[derive(ToSchema, Deserialize)]
    pub struct SizeFilter {
        #[serde(default)]
        min: u64,
        max: u64,
    }

    #[derive(ToSchema, Deserialize)]
    pub struct ContentFilter {
        query: compact_str::CompactString,
        max_search_size: u64,
        #[serde(default)]
        include_unmatched: bool,
        #[serde(default)]
        case_insensitive: bool,
    }

    #[derive(ToSchema, Deserialize)]
    pub struct PayloadV2 {
        #[serde(default)]
        root: compact_str::CompactString,
        #[schema(inline)]
        path_filter: Option<PathFilter>,
        #[schema(inline)]
        size_filter: Option<SizeFilter>,
        #[schema(inline)]
        content_filter: Option<ContentFilter>,

        per_page: usize,
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    pub enum Payload {
        V1(PayloadV1),
        V2(PayloadV2),
    }

    impl utoipa::PartialSchema for Payload {
        fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
            PayloadV2::schema()
        }
    }

    impl utoipa::ToSchema for Payload {
        fn name() -> std::borrow::Cow<'static, str> {
            PayloadV2::name()
        }

        fn schemas(
            schemas: &mut Vec<(
                String,
                utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
            )>,
        ) {
            PayloadV2::schemas(schemas)
        }
    }

    #[derive(ToSchema, Serialize)]
    struct Response<'a> {
        results: &'a [crate::models::DirectoryEntry],
    }

    #[utoipa::path(post, path = "/", responses(
        (status = OK, body = inline(Response)),
        (status = NOT_FOUND, body = ApiError),
    ), params(
        (
            "server" = uuid::Uuid,
            description = "The server uuid",
            example = "123e4567-e89b-12d3-a456-426614174000",
        ),
    ), request_body = inline(Payload))]
    pub async fn route(
        state: GetState,
        server: GetServer,
        axum::Json(data): axum::Json<Payload>,
    ) -> ApiResponseResult {
        let results_count = Arc::new(AtomicUsize::new(0));
        let results = Arc::new(Mutex::new(Vec::new()));

        match data {
            Payload::V1(data) => {
                let limit = data.limit.unwrap_or(100).min(500);
                let max_size = data.max_size.unwrap_or(512 * 1024);

                let root = match server.filesystem.async_canonicalize(&data.root).await {
                    Ok(path) => path,
                    Err(_) => {
                        return ApiResponse::error("root not found")
                            .with_status(StatusCode::NOT_FOUND)
                            .ok();
                    }
                };

                let metadata = server.filesystem.async_metadata(&root).await;
                if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
                    return ApiResponse::error("root is not a directory")
                        .with_status(StatusCode::NOT_FOUND)
                        .ok();
                }

                let ignored = &[server.filesystem.get_ignored().await];
                let mut walker = server
                    .filesystem
                    .async_walk_dir(&root)
                    .await?
                    .with_ignored(ignored);

                walker
                    .run_multithreaded(
                        state.config.api.file_search_threads,
                        Arc::new({
                            let server = server.clone();
                            let results_count = Arc::clone(&results_count);
                            let results = Arc::clone(&results);
                            let data = Arc::new(data);
                            let root = Arc::new(root);

                            move |is_dir, path: PathBuf| {
                                let server = server.clone();
                                let results_count = Arc::clone(&results_count);
                                let results = Arc::clone(&results);
                                let data = Arc::clone(&data);
                                let root = Arc::clone(&root);

                                async move {
                                    if is_dir || results_count.load(Ordering::Relaxed) >= limit {
                                        return Ok(());
                                    }

                                    let metadata =
                                        match server.filesystem.async_symlink_metadata(&path).await
                                        {
                                            Ok(metadata) => metadata,
                                            Err(_) => return Ok(()),
                                        };

                                    if !metadata.is_file() {
                                        return Ok(());
                                    }

                                    if path.to_string_lossy().contains(data.query.as_str()) {
                                        let mut entry = server
                                            .filesystem
                                            .to_api_entry(path.to_path_buf(), metadata)
                                            .await;
                                        entry.name = match path.strip_prefix(&data.root) {
                                            Ok(path) => path.to_string_lossy().into(),
                                            Err(_) => return Ok(()),
                                        };

                                        results.lock().await.push(entry);
                                        results_count.fetch_add(1, Ordering::Relaxed);
                                        return Ok(());
                                    }

                                    if data.include_content && metadata.len() <= max_size {
                                        let mut file =
                                            match server.filesystem.async_open(&path).await {
                                                Ok(file) => file,
                                                Err(_) => return Ok(()),
                                            };

                                        let mut buffer = [0; 128];
                                        let bytes_read = match file.read(&mut buffer).await {
                                            Ok(bytes_read) => bytes_read,
                                            Err(_) => return Ok(()),
                                        };

                                        if !crate::utils::is_valid_utf8_slice(&buffer[..bytes_read])
                                        {
                                            return Ok(());
                                        }

                                        file.seek(std::io::SeekFrom::Start(0)).await?;

                                        if search_in_stream(file, &data.query, true).await? {
                                            let mut entry = server
                                                .filesystem
                                                .to_api_entry_buffer(
                                                    path.to_path_buf(),
                                                    &metadata,
                                                    false,
                                                    Some(&buffer[..bytes_read]),
                                                    None,
                                                    None,
                                                )
                                                .await;
                                            entry.name = match path.strip_prefix(&*root) {
                                                Ok(path) => path.to_string_lossy().into(),
                                                Err(_) => return Ok(()),
                                            };

                                            results.lock().await.push(entry);
                                            results_count.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }

                                    Ok(())
                                }
                            }
                        }),
                    )
                    .await?;
            }
            Payload::V2(data) => {
                let root = match server.filesystem.async_canonicalize(&data.root).await {
                    Ok(path) => path,
                    Err(_) => {
                        return ApiResponse::error("root not found")
                            .with_status(StatusCode::NOT_FOUND)
                            .ok();
                    }
                };

                let metadata = server.filesystem.async_metadata(&root).await;
                if !metadata.map(|m| m.is_dir()).unwrap_or(true) {
                    return ApiResponse::error("root is not a directory")
                        .with_status(StatusCode::NOT_FOUND)
                        .ok();
                }

                let mut override_builder = OverrideBuilder::new("/");
                let mut ignore_builder = GitignoreBuilder::new("/");

                if let Some(path_filter) = &data.path_filter {
                    for glob in &path_filter.include {
                        override_builder.add(glob).ok();
                    }
                    for glob in &path_filter.exclude {
                        ignore_builder.add_line(None, glob).ok();
                    }
                }

                let path_includes = Arc::new(override_builder.build()?);

                let ignored = &[
                    server.filesystem.get_ignored().await,
                    ignore_builder.build()?,
                ];
                let mut walker = server
                    .filesystem
                    .async_walk_dir(&root)
                    .await?
                    .with_ignored(ignored);

                walker
                    .run_multithreaded(
                        state.config.api.file_search_threads,
                        Arc::new({
                            let server = server.clone();
                            let results_count = Arc::clone(&results_count);
                            let results = Arc::clone(&results);
                            let data = Arc::new(data);
                            let root = Arc::new(root);

                            move |is_dir, path: PathBuf| {
                                let server = server.clone();
                                let results_count = Arc::clone(&results_count);
                                let results = Arc::clone(&results);
                                let path_includes = Arc::clone(&path_includes);
                                let data = Arc::clone(&data);
                                let root = Arc::clone(&root);

                                async move {
                                    if is_dir
                                        || results_count.load(Ordering::Relaxed) >= data.per_page
                                    {
                                        return Ok(());
                                    }

                                    if data.path_filter.is_some()
                                        && !path_includes
                                            .matched(path.clone(), is_dir)
                                            .is_whitelist()
                                    {
                                        return Ok(());
                                    }

                                    let metadata =
                                        match server.filesystem.async_symlink_metadata(&path).await
                                        {
                                            Ok(metadata) => metadata,
                                            Err(_) => return Ok(()),
                                        };

                                    if let Some(size_filter) = &data.size_filter
                                        && !(size_filter.min..size_filter.max)
                                            .contains(&metadata.len())
                                    {
                                        return Ok(());
                                    }

                                    let mut local_buffer = [0; 128];
                                    let buffer = if let Some(content_filter) = &data.content_filter
                                        && (metadata.len() <= content_filter.max_search_size
                                            || content_filter.include_unmatched)
                                    {
                                        let mut file =
                                            match server.filesystem.async_open(&path).await {
                                                Ok(file) => file,
                                                Err(_) => return Ok(()),
                                            };

                                        let bytes_read = match file.read(&mut local_buffer).await {
                                            Ok(bytes_read) => bytes_read,
                                            Err(_) => return Ok(()),
                                        };

                                        if metadata.len() <= content_filter.max_search_size {
                                            if !crate::utils::is_valid_utf8_slice(
                                                &local_buffer[..bytes_read],
                                            ) {
                                                return Ok(());
                                            }

                                            file.seek(std::io::SeekFrom::Start(0)).await?;

                                            if !search_in_stream(
                                                file,
                                                &content_filter.query,
                                                content_filter.case_insensitive,
                                            )
                                            .await?
                                            {
                                                return Ok(());
                                            }
                                        }

                                        &local_buffer[..bytes_read]
                                    } else if data
                                        .content_filter
                                        .as_ref()
                                        .is_some_and(|cf| !cf.include_unmatched)
                                    {
                                        return Ok(());
                                    } else {
                                        let mut file =
                                            match server.filesystem.async_open(&path).await {
                                                Ok(file) => file,
                                                Err(_) => return Ok(()),
                                            };

                                        let bytes_read = match file.read(&mut local_buffer).await {
                                            Ok(bytes_read) => bytes_read,
                                            Err(_) => return Ok(()),
                                        };

                                        &local_buffer[..bytes_read]
                                    };

                                    let mut entry = server
                                        .filesystem
                                        .to_api_entry_buffer(
                                            path.to_path_buf(),
                                            &metadata,
                                            false,
                                            Some(buffer),
                                            None,
                                            None,
                                        )
                                        .await;
                                    entry.name = match path.strip_prefix(&*root) {
                                        Ok(path) => path.to_string_lossy().into(),
                                        Err(_) => return Ok(()),
                                    };

                                    results.lock().await.push(entry);
                                    results_count.fetch_add(1, Ordering::Relaxed);

                                    Ok(())
                                }
                            }
                        }),
                    )
                    .await?;
            }
        }

        ApiResponse::json(Response {
            results: &results.lock().await,
        })
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(post::route))
        .with_state(state.clone())
}
