use crate::{models::DirectoryEntry, server::backup::restic::get_backup_base_path};
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    process::Command,
    sync::Mutex,
};

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ResticEntryType {
    File,
    Dir,
    Symlink,
}

#[derive(Deserialize)]
struct ResticDirectoryEntry {
    r#type: ResticEntryType,
    path: PathBuf,
    mode: u32,
    size: Option<u64>,
    mtime: chrono::DateTime<chrono::Utc>,
}

type ResticDirectoryEntries = Arc<Vec<ResticDirectoryEntry>>;
static FILES: Mutex<Option<HashMap<uuid::Uuid, (ResticDirectoryEntries, std::time::Instant)>>> =
    Mutex::const_new(None);

async fn get_files_for_backup(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
) -> Result<ResticDirectoryEntries, anyhow::Error> {
    let mut files = FILES.lock().await;

    if let Some(files) = files.as_mut() {
        if let Some((cached_files, last_access)) = files.get_mut(&uuid) {
            *last_access = std::time::Instant::now();

            return Ok(Arc::clone(cached_files));
        }
    }

    if files.is_none() {
        *files = Some(HashMap::new());

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;

                let mut files = FILES.lock().await;
                let now = std::time::Instant::now();

                files.as_mut().unwrap().retain(|_, (_, last_access)| {
                    now.duration_since(*last_access) < std::time::Duration::from_secs(300)
                });
            }
        });
    }
    drop(files);

    let base_path = get_backup_base_path(server, uuid).await?;

    let child = Command::new("restic")
        .envs(&server.config.system.backups.restic.environment)
        .arg("--json")
        .arg("--repo")
        .arg(&server.config.system.backups.restic.repository)
        .arg("--password-file")
        .arg(&server.config.system.backups.restic.password_file)
        .arg("--retry-lock")
        .arg(format!(
            "{}s",
            server.config.system.backups.restic.retry_lock_seconds
        ))
        .arg("ls")
        .arg(format!("latest:{}", base_path.display()))
        .arg("/")
        .arg("--recursive")
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    let mut line_reader = tokio::io::BufReader::new(child.stdout.unwrap()).lines();
    let mut backup_files = Vec::new();

    while let Ok(Some(line)) = line_reader.next_line().await {
        if line.is_empty() {
            continue;
        }

        if let Ok(mut entry) = serde_json::from_str::<ResticDirectoryEntry>(&line) {
            entry.path = entry
                .path
                .strip_prefix(Path::new("/"))
                .unwrap_or(&entry.path)
                .to_owned();

            backup_files.push(entry);
        }
    }

    let backup_files = Arc::new(backup_files);
    FILES
        .lock()
        .await
        .as_mut()
        .unwrap()
        .insert(uuid, (Arc::clone(&backup_files), std::time::Instant::now()));

    Ok(Arc::clone(&backup_files))
}

fn restic_entry_to_directory_entry(
    path: &Path,
    files: &Arc<Vec<ResticDirectoryEntry>>,
    entry: &ResticDirectoryEntry,
) -> DirectoryEntry {
    let size = match entry.r#type {
        ResticEntryType::File => entry.size.unwrap_or(0),
        ResticEntryType::Dir => files
            .iter()
            .filter(|e| e.path.starts_with(&entry.path))
            .map(|e| e.size.unwrap_or(0))
            .sum(),
        _ => 0,
    };

    let mime = if matches!(entry.r#type, ResticEntryType::File) {
        "inode/directory"
    } else if matches!(entry.r#type, ResticEntryType::Symlink) {
        "inode/symlink"
    } else {
        "application/octet-stream"
    };

    let mut mode_str = String::new();

    mode_str.reserve_exact(10);
    mode_str.push(match rustix::fs::FileType::from_raw_mode(entry.mode) {
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
        if entry.mode & (1 << (8 - i)) != 0 {
            mode_str.push(RWX.chars().nth(i).unwrap());
        } else {
            mode_str.push('-');
        }
    }

    DirectoryEntry {
        name: path.file_name().unwrap().to_string_lossy().to_string(),
        created: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        modified: entry.mtime,
        mode: mode_str,
        mode_bits: format!("{:o}", entry.mode & 0o777),
        size,
        directory: matches!(entry.r#type, ResticEntryType::Dir),
        file: matches!(entry.r#type, ResticEntryType::File),
        symlink: matches!(entry.r#type, ResticEntryType::Symlink),
        mime,
    }
}

pub async fn list(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
    per_page: Option<usize>,
    page: usize,
) -> Result<(usize, Vec<DirectoryEntry>), anyhow::Error> {
    let files = get_files_for_backup(server, uuid).await?;

    let mut directory_entries = Vec::new();
    let mut other_entries = Vec::new();

    let path_len = path.components().count();
    for entry in files.iter() {
        let name = &entry.path;

        let name_len = name.components().count();
        if name_len < path_len
            || !name.starts_with(&path)
            || name == &path
            || name_len > path_len + 1
        {
            continue;
        }

        if matches!(entry.r#type, ResticEntryType::Dir) {
            directory_entries.push(entry);
        } else {
            other_entries.push(entry);
        }
    }

    directory_entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    other_entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));

    let total_entries = directory_entries.len() + other_entries.len();
    let mut entries = Vec::new();

    if let Some(per_page) = per_page {
        let start = (page - 1) * per_page;

        for entry in directory_entries
            .iter()
            .chain(other_entries.iter())
            .skip(start)
            .take(per_page)
        {
            entries.push(restic_entry_to_directory_entry(&entry.path, &files, entry));
        }
    } else {
        for entry in directory_entries.iter().chain(other_entries.iter()) {
            entries.push(restic_entry_to_directory_entry(&entry.path, &files, entry));
        }
    }

    Ok((total_entries, entries))
}

pub async fn reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
) -> Result<(Box<dyn tokio::io::AsyncRead + Send>, u64), anyhow::Error> {
    let files = get_files_for_backup(server, uuid).await?;
    let base_path = get_backup_base_path(server, uuid).await?;

    let entry = files
        .iter()
        .find(|e| e.path == path)
        .ok_or_else(|| anyhow::anyhow!("Path not found in archive: {}", path.display()))?;
    if !matches!(entry.r#type, ResticEntryType::File) {
        return Err(anyhow::anyhow!("Expected a file entry"));
    }

    let full_path = PathBuf::from(&base_path).join(&entry.path);

    let child = Command::new("restic")
        .envs(&server.config.system.backups.restic.environment)
        .arg("--json")
        .arg("--no-lock")
        .arg("--repo")
        .arg(&server.config.system.backups.restic.repository)
        .arg("--password-file")
        .arg(&server.config.system.backups.restic.password_file)
        .arg("dump")
        .arg("latest")
        .arg(full_path)
        .arg("--tag")
        .arg(uuid.to_string())
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    Ok((Box::new(child.stdout.unwrap()), entry.size.unwrap_or(0)))
}

pub async fn directory_reader(
    server: &crate::server::Server,
    uuid: uuid::Uuid,
    path: PathBuf,
) -> Result<tokio::io::DuplexStream, anyhow::Error> {
    let files = get_files_for_backup(server, uuid).await?;
    let base_path = get_backup_base_path(server, uuid).await?;

    let entry = files
        .iter()
        .find(|e| e.path == path)
        .ok_or_else(|| anyhow::anyhow!("Path not found in archive: {}", path.display()))?;
    if !matches!(entry.r#type, ResticEntryType::Dir) {
        return Err(anyhow::anyhow!("Expected a directory entry"));
    }

    let full_path = PathBuf::from(&base_path).join(&entry.path);
    let (writer, reader) = tokio::io::duplex(65536);

    let child = Command::new("restic")
        .envs(&server.config.system.backups.restic.environment)
        .arg("--json")
        .arg("--no-lock")
        .arg("--repo")
        .arg(&server.config.system.backups.restic.repository)
        .arg("--password-file")
        .arg(&server.config.system.backups.restic.password_file)
        .arg("dump")
        .arg(format!("latest:{}", full_path.display()))
        .arg("/")
        .arg("--tag")
        .arg(uuid.to_string())
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    let compression_level = server.config.system.backups.compression_level;
    tokio::spawn(async move {
        let mut stdout = child.stdout.unwrap();
        let mut writer = async_compression::tokio::write::GzipEncoder::with_quality(
            writer,
            async_compression::Level::Precise(
                compression_level.flate2_compression_level().level() as i32
            ),
        );

        tokio::io::copy(&mut stdout, &mut writer).await.ok();
        writer.shutdown().await.ok();
    });

    Ok(reader)
}
