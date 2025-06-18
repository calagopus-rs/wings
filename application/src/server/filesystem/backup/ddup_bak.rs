use crate::{
    models::DirectoryEntry,
    server::backup::ddup_bak::{get_repository, tar_recursive_convert_entries},
};
use std::{io::Read, path::Path};
use tokio::io::AsyncWriteExt;

fn ddup_bak_entry_to_directory_entry(
    path: &Path,
    repository: &ddup_bak::repository::Repository,
    entry: &ddup_bak::archive::entries::Entry,
) -> DirectoryEntry {
    let size = match entry {
        ddup_bak::archive::entries::Entry::File(file) => file.size_real,
        ddup_bak::archive::entries::Entry::Directory(dir) => {
            fn recursive_size(entry: &ddup_bak::archive::entries::Entry) -> u64 {
                match entry {
                    ddup_bak::archive::entries::Entry::File(file) => file.size_real,
                    ddup_bak::archive::entries::Entry::Directory(dir) => {
                        dir.entries.iter().map(recursive_size).sum()
                    }
                    ddup_bak::archive::entries::Entry::Symlink(link) => link.target.len() as u64,
                }
            }

            dir.entries.iter().map(recursive_size).sum()
        }
        ddup_bak::archive::entries::Entry::Symlink(link) => link.target.len() as u64,
    };

    let mut buffer = [0; 128];
    let buffer = match repository.entry_reader(entry.clone()) {
        Ok(reader) => {
            if reader.take(128).read(&mut buffer).is_err() {
                None
            } else {
                Some(&buffer)
            }
        }
        Err(_) => None,
    };

    let mime = if matches!(entry, ddup_bak::archive::entries::Entry::Directory(_)) {
        "inode/directory"
    } else if matches!(entry, ddup_bak::archive::entries::Entry::Symlink(_)) {
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
    let mode = entry.mode().bits();
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
        modified: chrono::DateTime::from_timestamp(
            entry
                .mtime()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            0,
        )
        .unwrap(),
        mode: mode_str,
        mode_bits: format!("{:o}", entry.mode().bits() & 0o777),
        size,
        directory: matches!(entry, ddup_bak::archive::entries::Entry::Directory(_)),
        file: matches!(entry, ddup_bak::archive::entries::Entry::File(_)),
        symlink: matches!(entry, ddup_bak::archive::entries::Entry::Symlink(_)),
        mime,
    }
}

pub async fn list(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: &Path,
) -> std::io::Result<Vec<DirectoryEntry>> {
    let repository = get_repository(server).await;

    let path = path.to_path_buf();
    let directory_entry_limit = server.config.api.directory_entry_limit;
    let entries =
        tokio::task::spawn_blocking(move || -> Result<Vec<DirectoryEntry>, std::io::Error> {
            let archive = repository.get_archive(&uuid.to_string())?;
            let entry = match archive.find_archive_entry(&path)? {
                Some(entry) => entry,
                None => {
                    let mut entries =
                        Vec::with_capacity(archive.entries().len().min(directory_entry_limit));
                    for entry in archive.into_entries() {
                        let path = path.join(entry.name());

                        entries.push(ddup_bak_entry_to_directory_entry(
                            &path,
                            &repository,
                            &entry,
                        ));

                        if entries.len() >= directory_entry_limit {
                            break;
                        }
                    }

                    return Ok(entries);
                }
            };

            match entry {
                ddup_bak::archive::entries::Entry::Directory(dir) => {
                    let mut entries =
                        Vec::with_capacity(dir.entries.len().min(directory_entry_limit));
                    for entry in &dir.entries {
                        let path = path.join(&dir.name).join(entry.name());

                        entries.push(ddup_bak_entry_to_directory_entry(&path, &repository, entry));

                        if entries.len() >= directory_entry_limit {
                            break;
                        }
                    }

                    Ok(entries)
                }
                _ => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Expected a directory entry",
                )),
            }
        })
        .await??;

    Ok(entries)
}

pub async fn reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: &Path,
) -> std::io::Result<(Box<dyn tokio::io::AsyncRead + Send>, u64)> {
    let repository = get_repository(server).await;

    let path = path.to_path_buf();
    tokio::task::spawn_blocking(
        move || -> std::io::Result<(Box<dyn tokio::io::AsyncRead + Send>, u64)> {
            let full_path = path.to_path_buf();
            let archive = repository.get_archive(&uuid.to_string())?;
            let entry = match archive.find_archive_entry(&full_path) {
                Ok(Some(entry)) => entry,
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("Path not found in archive: {}", full_path.display()),
                    ));
                }
            };

            let size = match entry {
                ddup_bak::archive::entries::Entry::File(file) => file.size_real,
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Expected a file entry",
                    ));
                }
            };

            let mut reader = repository.entry_reader(entry.clone())?;
            let (async_reader, mut async_writer) = tokio::io::duplex(65536);

            tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Handle::current();

                let mut buffer = [0; 8192];
                loop {
                    match reader.read(&mut buffer) {
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
    path: &Path,
) -> std::io::Result<tokio::io::DuplexStream> {
    let repository = get_repository(server).await;

    let (writer, reader) = tokio::io::duplex(65536);

    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let writer = tokio_util::io::SyncIoBridge::new(writer);
        let writer = flate2::write::GzEncoder::new(writer, flate2::Compression::default());

        let mut tar = tar::Builder::new(writer);
        tar.mode(tar::HeaderMode::Complete);

        let exit_early = &mut false;

        let archive = repository.get_archive(&uuid.to_string())?;
        match archive.find_archive_entry(&path) {
            Ok(Some(entry)) => {
                let entry = match entry {
                    ddup_bak::archive::entries::Entry::Directory(dir) => dir.clone(),
                    _ => {
                        *exit_early = true;
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "Expected a directory entry",
                        ));
                    }
                };

                for entry in entry.entries {
                    if *exit_early {
                        break;
                    }

                    tar_recursive_convert_entries(entry, exit_early, &repository, &mut tar, "");
                }

                if !*exit_early {
                    tar.finish().unwrap();
                }
            }
            _ => {
                for entry in archive.into_entries() {
                    if *exit_early {
                        break;
                    }

                    tar_recursive_convert_entries(entry, exit_early, &repository, &mut tar, "");
                }

                if !*exit_early {
                    tar.finish().unwrap();
                }
            }
        };

        Ok(())
    });

    Ok(reader)
}
