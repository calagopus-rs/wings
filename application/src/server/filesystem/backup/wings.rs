use crate::models::DirectoryEntry;
use std::{
    io::{Read, Seek},
    path::{Path, PathBuf},
};
use tokio::io::AsyncWriteExt;

fn zip_entry_to_directory_entry(
    path: &Path,
    sizes: &[(u64, PathBuf)],
    entry: &mut zip::read::ZipFile<impl Read + Seek>,
) -> DirectoryEntry {
    let size = if entry.is_dir() {
        sizes
            .iter()
            .filter(|(_, name)| name.starts_with(path))
            .map(|(size, _)| *size)
            .sum()
    } else {
        entry.size()
    };

    let mut buffer = [0; 128];
    let buffer = if entry.take(128).read(&mut buffer).is_err() {
        None
    } else {
        Some(&buffer)
    };

    let mime = if entry.is_dir() {
        "inode/directory"
    } else if entry.is_symlink() {
        "inode/symlink"
    } else if let Some(buffer) = buffer {
        if let Some(mime) = infer::get(buffer) {
            mime.mime_type()
        } else if std::str::from_utf8(buffer).is_ok() {
            "text/plain"
        } else {
            "application/octet-stream"
        }
    } else {
        "application/octet-stream"
    };

    let mut mode_str = String::new();
    let mode = entry.unix_mode().unwrap_or(0o644);
    const TYPE_CHARS: &str = "dalTLDpSugct?";

    let file_type = (mode >> 28) & 0xF;
    if file_type < TYPE_CHARS.len() as u32 {
        mode_str.push(TYPE_CHARS.chars().nth(file_type as usize).unwrap());
    } else {
        mode_str.push('?');
    }

    const RWX: &str = "rwxrwxrwx";
    for i in 0..9 {
        if mode & (1 << (8 - i)) != 0 {
            mode_str.push(RWX.chars().nth(i).unwrap());
        } else {
            mode_str.push('-');
        }
    }

    DirectoryEntry {
        name: path.file_name().unwrap().to_string_lossy().to_string(),
        created: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        modified: crate::server::filesystem::archive::zip_entry_get_modified_time(entry)
            .map(|dt| dt.into())
            .unwrap_or_default(),
        mode: mode_str,
        mode_bits: format!("{:o}", entry.unix_mode().unwrap_or(0x644) & 0o777),
        size,
        directory: entry.is_dir(),
        file: entry.is_file(),
        symlink: entry.is_symlink(),
        mime,
    }
}

pub async fn list(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
) -> std::io::Result<Vec<DirectoryEntry>> {
    let (file_format, file_name) = crate::server::backup::wings::get_first_file_name(server, uuid)
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No backup files found for this backup",
            )
        })?;
    if !matches!(
        file_format,
        crate::config::SystemBackupsWingsArchiveFormat::Zip
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "This backup does not use the ZIP format",
        ));
    }

    let directory_entry_limit = server.config.api.directory_entry_limit;
    let entries =
        tokio::task::spawn_blocking(move || -> Result<Vec<DirectoryEntry>, std::io::Error> {
            let mut archive = zip::ZipArchive::new(std::fs::File::open(file_name)?)?;
            let mut entries = Vec::new();

            let names = archive
                .file_names()
                .map(|name| name.to_string())
                .collect::<Vec<_>>();
            let sizes = names
                .into_iter()
                .map(|name| {
                    (
                        archive
                            .by_name(&name)
                            .map(|file| file.size())
                            .unwrap_or_default(),
                        PathBuf::from(name),
                    )
                })
                .collect::<Vec<_>>();

            let path_len = path.components().count();
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)?;
                let name = match entry.enclosed_name() {
                    Some(name) => name,
                    None => continue,
                };

                let name_len = name.components().count();
                if name_len < path_len
                    || !name.starts_with(&path)
                    || name == path
                    || name_len > path_len + 1
                {
                    continue;
                }

                let entry = zip_entry_to_directory_entry(&name, &sizes, &mut entry);
                entries.push(entry);

                if entries.len() >= directory_entry_limit {
                    break;
                }
            }

            Ok(entries)
        })
        .await??;

    Ok(entries)
}

pub async fn reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
) -> std::io::Result<(Box<dyn tokio::io::AsyncRead + Send>, u64)> {
    let (file_format, file_name) = crate::server::backup::wings::get_first_file_name(server, uuid)
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No backup files found for this backup",
            )
        })?;
    if !matches!(
        file_format,
        crate::config::SystemBackupsWingsArchiveFormat::Zip
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "This backup does not use the ZIP format",
        ));
    }

    tokio::task::spawn_blocking(
        move || -> std::io::Result<(Box<dyn tokio::io::AsyncRead + Send>, u64)> {
            let mut archive = zip::ZipArchive::new(std::fs::File::open(file_name)?)?;
            let entry = match archive.by_name(&path.to_string_lossy()) {
                Ok(entry) => entry,
                Err(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("Path not found in archive: {}", path.display()),
                    ));
                }
            };

            if !entry.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Expected a file entry",
                ));
            }

            let size = entry.size();
            let (async_reader, mut async_writer) = tokio::io::duplex(65536);
            drop(entry);

            tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Handle::current();
                let mut entry = archive.by_name(&path.to_string_lossy()).unwrap();

                let mut buffer = [0; 8192];
                loop {
                    match entry.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            if runtime
                                .block_on(async_writer.write_all(&buffer[..n]))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(err) => {
                            tracing::error!("error reading from ddup_bak entry: {:#?}", err);
                            break;
                        }
                    }
                }
            });

            Ok((Box::new(async_reader), size))
        },
    )
    .await?
}

pub async fn directory_reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
) -> std::io::Result<tokio::io::DuplexStream> {
    let (file_format, file_name) = crate::server::backup::wings::get_first_file_name(server, uuid)
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No backup files found for this backup",
            )
        })?;
    if !matches!(
        file_format,
        crate::config::SystemBackupsWingsArchiveFormat::Zip
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "This backup does not use the ZIP format",
        ));
    }

    let (writer, reader) = tokio::io::duplex(65536);

    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let writer = tokio_util::io::SyncIoBridge::new(writer);
        let writer = flate2::write::GzEncoder::new(writer, flate2::Compression::default());

        let mut tar = tar::Builder::new(writer);
        tar.mode(tar::HeaderMode::Complete);

        let mut archive = zip::ZipArchive::new(std::fs::File::open(file_name)?)?;

        for i in 0..archive.len() {
            let entry = archive.by_index(i)?;
            let name = match entry.enclosed_name() {
                Some(name) => name,
                None => continue,
            };

            let name = match name.strip_prefix(&path) {
                Ok(name) => name,
                Err(_) => continue,
            };

            if name.components().count() == 0 {
                continue;
            }

            if entry.is_dir() {
                let mut entry_header = tar::Header::new_gnu();
                if let Some(mode) = entry.unix_mode() {
                    entry_header.set_mode(mode);
                }

                entry_header.set_mtime(
                    crate::server::filesystem::archive::zip_entry_get_modified_time(&entry)
                        .map(|dt| dt.elapsed().unwrap_or_default().as_secs())
                        .unwrap_or_default(),
                );
                entry_header.set_entry_type(tar::EntryType::Directory);

                tar.append_data(&mut entry_header, name, std::io::empty())?;
            } else if entry.is_file() {
                let mut entry_header = tar::Header::new_gnu();
                if let Some(mode) = entry.unix_mode() {
                    entry_header.set_mode(mode);
                }

                entry_header.set_mtime(
                    crate::server::filesystem::archive::zip_entry_get_modified_time(&entry)
                        .map(|dt| dt.elapsed().unwrap_or_default().as_secs())
                        .unwrap_or_default(),
                );
                entry_header.set_entry_type(tar::EntryType::Regular);
                entry_header.set_size(entry.size());

                tar.append_data(&mut entry_header, name, entry)?;
            } else if entry.is_symlink() && (1..=2048).contains(&entry.size()) {
                let mut entry_header = tar::Header::new_gnu();
                if let Some(mode) = entry.unix_mode() {
                    entry_header.set_mode(mode);
                }

                entry_header.set_mtime(
                    crate::server::filesystem::archive::zip_entry_get_modified_time(&entry)
                        .map(|dt| dt.elapsed().unwrap_or_default().as_secs())
                        .unwrap_or_default(),
                );
                entry_header.set_entry_type(tar::EntryType::Symlink);

                let link_name = std::io::read_to_string(entry)?;
                tar.append_link(&mut entry_header, name, link_name)?;
            }
        }

        Ok(())
    });

    Ok(reader)
}
