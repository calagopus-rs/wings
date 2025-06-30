use crate::{
    models::DirectoryEntry,
    server::backup::ddup_bak::{get_repository, tar_recursive_convert_entries},
};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
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
        Ok(mut reader) => {
            if reader.read(&mut buffer).is_err() {
                None
            } else {
                Some(&buffer)
            }
        }
        Err(_) => None,
    };

    let mime = if entry.is_directory() {
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
    let mode = entry.mode().bits();

    mode_str.reserve_exact(10);
    mode_str.push(match rustix::fs::FileType::from_raw_mode(mode) {
        rustix::fs::FileType::RegularFile => '-',
        rustix::fs::FileType::Directory => 'd',
        rustix::fs::FileType::Symlink => 'l',
        rustix::fs::FileType::BlockDevice => 'b',
        rustix::fs::FileType::CharacterDevice => 'c',
        rustix::fs::FileType::Socket => 's',
        rustix::fs::FileType::Fifo => 'p',
        rustix::fs::FileType::Unknown => '?',
    });

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
        directory: entry.is_directory(),
        file: entry.is_file(),
        symlink: entry.is_symlink(),
        mime,
    }
}

pub async fn list(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
    per_page: Option<usize>,
    page: usize,
) -> Result<Vec<DirectoryEntry>, anyhow::Error> {
    let repository = get_repository(server).await;

    let entries =
        tokio::task::spawn_blocking(move || -> Result<Vec<DirectoryEntry>, anyhow::Error> {
            let archive = repository.get_archive(&uuid.to_string())?;
            let entry = match archive.find_archive_entry(&path) {
                Some(entry) => entry,
                None => {
                    let mut entries = Vec::new();
                    entries.reserve_exact(
                        archive
                            .entries()
                            .len()
                            .min(per_page.unwrap_or(archive.entries().len())),
                    );

                    let mut matched_entries = 0;
                    for entry in archive.into_entries() {
                        let path = path.join(entry.name());

                        matched_entries += 1;
                        if let Some(per_page) = per_page
                            && matched_entries < (page - 1) * per_page
                        {
                            continue;
                        }

                        entries.push(ddup_bak_entry_to_directory_entry(
                            &path,
                            &repository,
                            &entry,
                        ));

                        if let Some(per_page) = per_page
                            && entries.len() >= per_page
                        {
                            break;
                        }
                    }

                    return Ok(entries);
                }
            };

            match entry {
                ddup_bak::archive::entries::Entry::Directory(dir) => {
                    let mut entries = Vec::new();
                    entries.reserve_exact(
                        dir.entries.len().min(per_page.unwrap_or(dir.entries.len())),
                    );

                    let mut matched_entries = 0;
                    for entry in &dir.entries {
                        let path = path.join(&dir.name).join(entry.name());

                        matched_entries += 1;
                        if let Some(per_page) = per_page
                            && matched_entries <= (page - 1) * per_page
                        {
                            continue;
                        }

                        entries.push(ddup_bak_entry_to_directory_entry(&path, &repository, entry));

                        if let Some(per_page) = per_page
                            && entries.len() >= per_page
                        {
                            break;
                        }
                    }

                    Ok(entries)
                }
                _ => Err(anyhow::anyhow!("Expected a directory entry")),
            }
        })
        .await??;

    Ok(entries)
}

pub async fn reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
) -> Result<(Box<dyn tokio::io::AsyncRead + Send>, u64), anyhow::Error> {
    let repository = get_repository(server).await;

    tokio::task::spawn_blocking(
        move || -> Result<(Box<dyn tokio::io::AsyncRead + Send>, u64), anyhow::Error> {
            let archive = repository.get_archive(&uuid.to_string())?;
            let entry = match archive.find_archive_entry(&path) {
                Some(entry) => entry,
                None => {
                    return Err(anyhow::anyhow!(
                        "Path not found in archive: {}",
                        path.display()
                    ));
                }
            };

            let size = match entry {
                ddup_bak::archive::entries::Entry::File(file) => file.size_real,
                _ => {
                    return Err(anyhow::anyhow!("Expected a file entry"));
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
    path: PathBuf,
) -> Result<tokio::io::DuplexStream, anyhow::Error> {
    let repository = get_repository(server).await;

    let (writer, reader) = tokio::io::duplex(65536);
    let compression_level = server.config.system.backups.compression_level;

    tokio::task::spawn_blocking(move || {
        let writer = tokio_util::io::SyncIoBridge::new(writer);
        let writer =
            flate2::write::GzEncoder::new(writer, compression_level.flate2_compression_level());
        let mut tar = tar::Builder::new(writer);
        tar.mode(tar::HeaderMode::Complete);

        let exit_early = &mut false;

        let archive = repository.get_archive(&uuid.to_string())?;
        match archive.find_archive_entry(&path) {
            Some(entry) => {
                let entry = match entry {
                    ddup_bak::archive::entries::Entry::Directory(dir) => dir,
                    _ => {
                        *exit_early = true;
                        return Err(anyhow::anyhow!("Expected a directory entry"));
                    }
                };

                for entry in entry.entries.iter() {
                    if *exit_early {
                        break;
                    }

                    tar_recursive_convert_entries(entry, exit_early, &repository, &mut tar, "");
                }

                if !*exit_early {
                    tar.finish().unwrap();
                }
            }
            None => {
                for entry in archive.entries() {
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
