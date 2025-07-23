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

    let mut buffer = [0; 64];
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
        } else if crate::is_valid_utf8_slice(buffer) || buffer.is_empty() {
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
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
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
    is_ignored: impl Fn(&Path, bool) -> bool + Send + Sync + 'static,
) -> Result<(usize, Vec<DirectoryEntry>), anyhow::Error> {
    let repository = get_repository(server).await;

    let entries = tokio::task::spawn_blocking(
        move || -> Result<(usize, Vec<DirectoryEntry>), anyhow::Error> {
            let archive = repository.get_archive(&uuid.to_string())?;
            let entry = match archive.find_archive_entry(&path) {
                Some(entry) => entry,
                None => {
                    let mut directory_entries = Vec::new();
                    directory_entries.reserve_exact(
                        archive
                            .entries()
                            .iter()
                            .filter(|e| e.is_directory())
                            .count(),
                    );
                    let mut other_entries = Vec::new();
                    other_entries.reserve_exact(
                        archive
                            .entries()
                            .iter()
                            .filter(|e| !e.is_directory())
                            .count(),
                    );

                    for entry in archive.entries() {
                        if is_ignored(Path::new(entry.name()), entry.is_directory()) {
                            continue;
                        }

                        if entry.is_directory() {
                            directory_entries.push(entry);
                        } else {
                            other_entries.push(entry);
                        }
                    }

                    directory_entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));
                    other_entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));

                    let total_entries = directory_entries.len() + other_entries.len();
                    let mut entries = Vec::new();

                    if let Some(per_page) = per_page {
                        let start = (page - 1) * per_page;

                        for entry in directory_entries
                            .into_iter()
                            .chain(other_entries.into_iter())
                            .skip(start)
                            .take(per_page)
                        {
                            let path = path.join(entry.name());

                            entries.push(ddup_bak_entry_to_directory_entry(
                                &path,
                                &repository,
                                entry,
                            ));
                        }
                    } else {
                        for entry in directory_entries
                            .into_iter()
                            .chain(other_entries.into_iter())
                        {
                            let path = path.join(entry.name());

                            entries.push(ddup_bak_entry_to_directory_entry(
                                &path,
                                &repository,
                                entry,
                            ));
                        }
                    }

                    return Ok((total_entries, entries));
                }
            };

            match entry {
                ddup_bak::archive::entries::Entry::Directory(dir) => {
                    let mut directory_entries = Vec::new();
                    directory_entries
                        .reserve_exact(dir.entries.iter().filter(|e| e.is_directory()).count());
                    let mut other_entries = Vec::new();
                    other_entries
                        .reserve_exact(dir.entries.iter().filter(|e| !e.is_directory()).count());

                    for entry in &dir.entries {
                        if is_ignored(Path::new(entry.name()), entry.is_directory()) {
                            continue;
                        }

                        if entry.is_directory() {
                            directory_entries.push(entry);
                        } else {
                            other_entries.push(entry);
                        }
                    }

                    directory_entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));
                    other_entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));

                    let total_entries = directory_entries.len() + other_entries.len();
                    let mut entries = Vec::new();

                    if let Some(per_page) = per_page {
                        let start = (page - 1) * per_page;

                        for entry in directory_entries
                            .into_iter()
                            .chain(other_entries.into_iter())
                            .skip(start)
                            .take(per_page)
                        {
                            let path = path.join(&dir.name).join(entry.name());

                            entries.push(ddup_bak_entry_to_directory_entry(
                                &path,
                                &repository,
                                entry,
                            ));
                        }
                    } else {
                        for entry in directory_entries
                            .into_iter()
                            .chain(other_entries.into_iter())
                        {
                            let path = path.join(&dir.name).join(entry.name());

                            entries.push(ddup_bak_entry_to_directory_entry(
                                &path,
                                &repository,
                                entry,
                            ));
                        }
                    }

                    Ok((total_entries, entries))
                }
                _ => Err(anyhow::anyhow!("Expected a directory entry")),
            }
        },
    )
    .await??;

    Ok(entries)
}

pub async fn reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
) -> Result<(Box<dyn tokio::io::AsyncRead + Unpin + Send>, u64), anyhow::Error> {
    let repository = get_repository(server).await;

    tokio::task::spawn_blocking(move || {
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
        let (async_reader, mut async_writer) = tokio::io::duplex(crate::BUFFER_SIZE);

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

        Ok((
            Box::new(async_reader) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
            size,
        ))
    })
    .await?
}

pub async fn files_reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
    file_paths: Vec<PathBuf>,
) -> Result<tokio::io::DuplexStream, anyhow::Error> {
    let repository = get_repository(server).await;

    let (writer, reader) = tokio::io::duplex(crate::BUFFER_SIZE);
    let compression_level = server.config.system.backups.compression_level;

    tokio::task::spawn_blocking(move || {
        let writer = tokio_util::io::SyncIoBridge::new(writer);
        let writer =
            flate2::write::GzEncoder::new(writer, compression_level.flate2_compression_level());
        let mut tar = tar::Builder::new(writer);
        tar.mode(tar::HeaderMode::Complete);

        let exit_early = &mut false;
        let archive = repository.get_archive(&uuid.to_string())?;

        for file_path in file_paths {
            let path = path.join(&file_path);

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
                    if path.components().count() == 0 {
                        for entry in archive.entries() {
                            if *exit_early {
                                break;
                            }

                            tar_recursive_convert_entries(
                                entry,
                                exit_early,
                                &repository,
                                &mut tar,
                                "",
                            );
                        }

                        if !*exit_early {
                            tar.finish().unwrap();
                        }
                    }
                }
            };
        }

        Ok(())
    });

    Ok(reader)
}

pub async fn directory_reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
) -> Result<tokio::io::DuplexStream, anyhow::Error> {
    let repository = get_repository(server).await;

    let (writer, reader) = tokio::io::duplex(crate::BUFFER_SIZE);
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
                if path.components().count() == 0 {
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
            }
        };

        Ok(())
    });

    Ok(reader)
}
