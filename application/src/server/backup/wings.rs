use crate::remote::backups::RawServerBackup;
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};
use chrono::{Datelike, Timelike};
use ignore::WalkBuilder;
use sha1::Digest;
use std::{
    fs::Permissions,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tokio::io::AsyncReadExt;

#[inline]
fn get_tar_gz_file_name(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.config.system.backup_directory).join(format!("{}.tar.gz", uuid))
}

#[inline]
fn get_zip_file_name(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    Path::new(&server.config.system.backup_directory).join(format!("{}.zip", uuid))
}

#[inline]
fn get_file_name(server: &crate::server::Server, uuid: uuid::Uuid) -> PathBuf {
    match server.config.system.backups.wings.archive_format {
        crate::config::SystemBackupsWingsArchiveFormat::TarGz => get_tar_gz_file_name(server, uuid),
        crate::config::SystemBackupsWingsArchiveFormat::Zip => get_zip_file_name(server, uuid),
    }
}

#[inline]
async fn get_first_file_name(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(crate::config::SystemBackupsWingsArchiveFormat, PathBuf), anyhow::Error> {
    let file_name = get_tar_gz_file_name(server, uuid);
    if tokio::fs::metadata(&file_name).await.is_ok() {
        return Ok((
            crate::config::SystemBackupsWingsArchiveFormat::TarGz,
            file_name,
        ));
    }

    let file_name = get_zip_file_name(server, uuid);
    if tokio::fs::metadata(&file_name).await.is_ok() {
        return Ok((
            crate::config::SystemBackupsWingsArchiveFormat::Zip,
            file_name,
        ));
    }

    Err(anyhow::anyhow!("No backup file found for UUID: {}", uuid))
}

pub async fn create_backup(
    server: crate::server::Server,
    uuid: uuid::Uuid,
    overrides: ignore::overrides::Override,
) -> Result<RawServerBackup, anyhow::Error> {
    let file_name = get_file_name(&server, uuid);
    let writer = std::fs::File::create(&file_name)?;

    let archive_format = server.config.system.backups.wings.archive_format;
    let compression_level = server.config.system.backups.compression_level;
    tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        match archive_format {
            crate::config::SystemBackupsWingsArchiveFormat::TarGz => {
                let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
                    writer,
                    flate2::Compression::new(compression_level.into()),
                ));

                tar.mode(tar::HeaderMode::Complete);
                tar.follow_symlinks(false);

                for entry in WalkBuilder::new(&server.filesystem.base_path)
                    .overrides(overrides)
                    .add_custom_ignore_filename(".pteroignore")
                    .follow_links(false)
                    .git_global(false)
                    .hidden(false)
                    .build()
                    .flatten()
                {
                    let path = entry.path().canonicalize()?;
                    let metadata = entry.metadata()?;

                    if let Ok(relative) = path.strip_prefix(&server.filesystem.base_path) {
                        if metadata.is_dir() {
                            tar.append_dir(relative, &path).ok();
                        } else {
                            tar.append_path_with_name(&path, relative).ok();
                        }
                    }
                }

                tar.finish()?;
            }
            crate::config::SystemBackupsWingsArchiveFormat::Zip => {
                let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(writer));

                for entry in WalkBuilder::new(&server.filesystem.base_path)
                    .overrides(overrides)
                    .add_custom_ignore_filename(".pteroignore")
                    .follow_links(false)
                    .git_global(false)
                    .hidden(false)
                    .build()
                    .flatten()
                {
                    let path = entry.path().canonicalize()?;
                    let metadata = entry.metadata()?;

                    if let Ok(relative) = path.strip_prefix(&server.filesystem.base_path) {
                        if metadata.is_dir() {
                            let mut options: zip::write::FileOptions<'_, ()> =
                                zip::write::FileOptions::default()
                                    .compression_level(Some(u32::from(compression_level) as i64))
                                    .unix_permissions(metadata.permissions().mode());

                            if let Ok(mtime) = metadata.modified() {
                                let mtime: chrono::DateTime<chrono::Local> =
                                    chrono::DateTime::from(mtime);

                                options =
                                    options.last_modified_time(zip::DateTime::from_date_and_time(
                                        mtime.year() as u16,
                                        mtime.month() as u8,
                                        mtime.day() as u8,
                                        mtime.hour() as u8,
                                        mtime.minute() as u8,
                                        mtime.second() as u8,
                                    )?);
                            }

                            zip.add_directory(relative.to_string_lossy(), options).ok();
                        } else if metadata.is_file() {
                            let mut options: zip::write::FileOptions<'_, ()> =
                                zip::write::FileOptions::default()
                                    .compression_level(Some(u32::from(compression_level) as i64))
                                    .unix_permissions(metadata.permissions().mode());

                            if let Ok(mtime) = metadata.modified() {
                                let mtime: chrono::DateTime<chrono::Local> =
                                    chrono::DateTime::from(mtime);

                                options =
                                    options.last_modified_time(zip::DateTime::from_date_and_time(
                                        mtime.year() as u16,
                                        mtime.month() as u8,
                                        mtime.day() as u8,
                                        mtime.hour() as u8,
                                        mtime.minute() as u8,
                                        mtime.second() as u8,
                                    )?);
                            }

                            zip.start_file(relative.to_string_lossy(), options)?;
                            let mut file = std::fs::File::open(&path)?;
                            std::io::copy(&mut file, &mut zip)?;
                        }
                    }
                }

                zip.finish()?;
            }
        }

        Ok(())
    })
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
    server: crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let (file_format, file_name) = get_first_file_name(&server, uuid).await?;
    let file = std::fs::File::open(&file_name)?;

    let server = server.clone();
    tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        let runtime = tokio::runtime::Handle::current();

        match file_format {
            crate::config::SystemBackupsWingsArchiveFormat::TarGz => {
                let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));

                for entry in archive.entries().unwrap() {
                    let mut entry = entry.unwrap();
                    let path = entry.path().unwrap();

                    if path.is_absolute() {
                        continue;
                    }

                    let destination_path = server.filesystem.base_path.join(&path);
                    if !server.filesystem.is_safe_path_sync(&destination_path) {
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
                            std::os::unix::fs::chown(
                                &destination_path,
                                header.uid().map(|u| u as u32).ok(),
                                header.gid().map(|g| g as u32).ok(),
                            )
                            .unwrap();
                        }
                        tar::EntryType::Regular => {
                            runtime.block_on(
                                server.log_daemon(format!("(restoring): {}", path.display())),
                            );

                            std::fs::create_dir_all(destination_path.parent().unwrap()).unwrap();

                            let mut writer =
                                crate::server::filesystem::writer::FileSystemWriter::new(
                                    server.clone(),
                                    destination_path,
                                    Some(Permissions::from_mode(header.mode().unwrap_or(0o644))),
                                    header
                                        .mtime()
                                        .map(|t| {
                                            std::time::UNIX_EPOCH
                                                + std::time::Duration::from_secs(t)
                                        })
                                        .ok(),
                                )
                                .unwrap();

                            std::io::copy(&mut entry, &mut writer).unwrap();
                            writer.flush().unwrap();
                        }
                        _ => {}
                    }
                }
            }
            crate::config::SystemBackupsWingsArchiveFormat::Zip => {
                let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).unwrap();

                for i in 0..archive.len() {
                    let mut entry = archive.by_index(i)?;
                    let path = match entry.enclosed_name() {
                        Some(path) => path,
                        None => continue,
                    };

                    if path.is_absolute() {
                        continue;
                    }

                    let destination_path = server.filesystem.base_path.join(&path);
                    if !server.filesystem.is_safe_path_sync(&destination_path) {
                        continue;
                    }

                    if server
                        .filesystem
                        .is_ignored_sync(&destination_path, entry.is_dir())
                    {
                        continue;
                    }

                    if entry.is_dir() {
                        std::fs::create_dir_all(&destination_path)?;
                    } else {
                        runtime.block_on(
                            server.log_daemon(format!("(restoring): {}", path.display())),
                        );

                        std::fs::create_dir_all(destination_path.parent().unwrap())?;

                        let mut writer = crate::server::filesystem::writer::FileSystemWriter::new(
                            server.clone(),
                            destination_path,
                            entry.unix_mode().map(Permissions::from_mode),
                            None,
                        )?;

                        std::io::copy(&mut entry, &mut writer)?;
                        writer.flush()?;
                    }
                }
            }
        };

        Ok(())
    })
    .await??;

    Ok(())
}

pub async fn download_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(StatusCode, HeaderMap, Body), anyhow::Error> {
    let (file_format, file_name) = get_first_file_name(server, uuid).await?;
    let file = tokio::fs::File::open(&file_name).await?;

    let mut headers = HeaderMap::new();

    match file_format {
        crate::config::SystemBackupsWingsArchiveFormat::TarGz => {
            headers.insert(
                "Content-Disposition",
                format!("attachment; filename={}.tar.gz", uuid)
                    .parse()
                    .unwrap(),
            );
            headers.insert("Content-Type", "application/gzip".parse().unwrap());
        }
        crate::config::SystemBackupsWingsArchiveFormat::Zip => {
            headers.insert(
                "Content-Disposition",
                format!("attachment; filename={}.zip", uuid)
                    .parse()
                    .unwrap(),
            );
            headers.insert("Content-Type", "application/zip".parse().unwrap());
        }
    };

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
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<(), anyhow::Error> {
    let (_, file_name) = get_first_file_name(server, uuid).await?;

    tokio::fs::remove_file(file_name).await?;

    Ok(())
}

pub async fn list_backups(
    server: &crate::server::Server,
) -> Result<Vec<uuid::Uuid>, anyhow::Error> {
    let mut backups = Vec::new();
    let path = Path::new(&server.config.system.backup_directory);

    let mut entries = tokio::fs::read_dir(path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let file_name = entry.file_name();

        if let Ok(uuid) = uuid::Uuid::parse_str(
            file_name
                .to_str()
                .unwrap_or_default()
                .trim_end_matches(".tar.gz")
                .trim_end_matches(".zip"),
        ) {
            backups.push(uuid);
        }
    }

    Ok(backups)
}
