use crate::remote::backups::RawServerBackup;
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};
use ignore::{WalkBuilder, overrides::OverrideBuilder};
use sha1::Digest;
use std::{
    fs::Permissions,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::AsyncReadExt;

#[inline]
fn get_file_name(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.config.system.backup_directory).join(format!("{}.tar.gz", uuid))
}

pub async fn create_backup(
    server: &Arc<crate::server::Server>,
    uuid: uuid::Uuid,
    ignore: String,
) -> Result<RawServerBackup, Box<dyn std::error::Error + Send + Sync>> {
    let file_name = get_file_name(server, uuid);
    let writer = std::io::BufWriter::new(std::fs::File::create(&file_name)?);

    let mut overrides = OverrideBuilder::new(&server.filesystem.base_path);
    for line in ignore.lines() {
        if line.trim().is_empty() {
            continue;
        }

        if let Some(line) = line.trim().strip_prefix('!') {
            overrides.add(line).ok();
        } else {
            overrides.add(&format!("!{}", line.trim())).ok();
        }
    }

    let filesystem = Arc::clone(&server.filesystem);
    let compression_level = server.config.system.backups.compression_level;
    tokio::task::spawn_blocking(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
                writer,
                flate2::Compression::new(compression_level.into()),
            ));

            for entry in WalkBuilder::new(&filesystem.base_path)
                .overrides(overrides.build()?)
                .add_custom_ignore_filename(".pteroignore")
                .follow_links(false)
                .git_global(false)
                .build()
                .flatten()
            {
                let path = entry.path().canonicalize()?;
                let metadata = entry.metadata()?;

                if let Ok(relative) = path.strip_prefix(&filesystem.base_path) {
                    if metadata.is_dir() {
                        archive.append_dir(relative, &path).ok();
                    } else {
                        archive.append_path_with_name(&path, relative).ok();
                    }
                }
            }

            archive.finish()?;

            Ok(())
        },
    )
    .await??;

    let mut sha1 = sha1::Sha1::new();
    let mut file = tokio::fs::File::open(&file_name).await?;

    let mut buffer = [0; 8192];
    loop {
        let bytes_read = file.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }

        sha1.update(&buffer[..bytes_read]);
    }

    Ok(RawServerBackup {
        checksum: format!("{:x}", sha1.finalize()),
        checksum_type: "sha1".to_string(),
        size: file.metadata().await?.len(),
        successful: true,
        parts: vec![],
    })
}

pub async fn restore_backup(
    server: &Arc<crate::server::Server>,
    uuid: uuid::Uuid,
    truncate_directory: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let file_name = get_file_name(server, uuid);
    let file = std::fs::File::open(&file_name)?;

    if truncate_directory {
        server.filesystem.truncate_root().await;
    }

    let filesystem = Arc::clone(&server.filesystem);
    let server = Arc::clone(server);
    tokio::task::spawn_blocking(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));

            for entry in archive.entries().unwrap() {
                let mut entry = entry.unwrap();
                let path = entry.path().unwrap();

                if path.is_absolute() {
                    continue;
                }

                let destination_path = filesystem.base_path.join(&path);
                if !filesystem.is_safe_path(&destination_path) {
                    continue;
                }

                let header = entry.header();
                match header.entry_type() {
                    tar::EntryType::Directory => {
                        std::fs::create_dir_all(&destination_path).unwrap();
                        std::fs::set_permissions(
                            &destination_path,
                            Permissions::from_mode(header.mode().unwrap_or(0o755)),
                        )
                        .unwrap();
                    }
                    tar::EntryType::Regular => {
                        futures::executor::block_on(
                            server.log_daemon(format!("(restoring): {}", path.display())),
                        );

                        std::fs::create_dir_all(destination_path.parent().unwrap()).unwrap();

                        let mut writer = crate::server::filesystem::writer::FileSystemWriter::new(
                            Arc::clone(&filesystem),
                            destination_path,
                            Some(Permissions::from_mode(header.mode().unwrap_or(0o644))),
                        )
                        .unwrap();

                        std::io::copy(&mut entry, &mut writer).unwrap();
                        writer.flush().unwrap();
                    }
                    _ => {}
                }
            }

            Ok(())
        },
    )
    .await??;

    Ok(())
}

pub async fn download_backup(
    server: &Arc<crate::server::Server>,
    uuid: uuid::Uuid,
) -> Result<(StatusCode, HeaderMap, Body), Box<dyn std::error::Error + Send + Sync>> {
    let file_name = get_file_name(server, uuid);
    let file = tokio::fs::File::open(&file_name).await?;

    let mut headers = HeaderMap::new();
    headers.insert(
        "Content-Disposition",
        format!("attachment; filename={}.tar.gz", uuid)
            .parse()
            .unwrap(),
    );
    headers.insert("Content-Type", "application/gzip".parse().unwrap());
    headers.insert("Content-Length", file.metadata().await?.len().into());

    Ok((
        StatusCode::OK,
        headers,
        Body::from_stream(tokio_util::io::ReaderStream::new(
            tokio::io::BufReader::new(file),
        )),
    ))
}

pub async fn delete_backup(
    server: &Arc<crate::server::Server>,
    uuid: uuid::Uuid,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let file_name = get_file_name(server, uuid);
    if file_name.exists() {
        tokio::fs::remove_file(&file_name).await?;
    }

    Ok(())
}

pub async fn list_backups(
    server: &Arc<crate::server::Server>,
) -> Result<Vec<uuid::Uuid>, Box<dyn std::error::Error + Send + Sync>> {
    let mut backups = Vec::new();
    let path = Path::new(&server.config.system.backup_directory);

    let mut entries = tokio::fs::read_dir(path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let file_name = entry.file_name();

        if let Ok(uuid) = uuid::Uuid::parse_str(
            file_name
                .to_str()
                .unwrap_or_default()
                .trim_end_matches(".tar.gz"),
        ) {
            backups.push(uuid);
        }
    }

    Ok(backups)
}
