use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::RwLock,
};

type DiskUsageMap = HashMap<String, (PathBuf, u32, i64)>;
static DISK_USAGE: LazyLock<Arc<RwLock<DiskUsageMap>>> = LazyLock::new(|| {
    let disk_usage: Arc<RwLock<DiskUsageMap>> = Arc::new(RwLock::new(HashMap::new()));

    tokio::spawn({
        let disk_usage = Arc::clone(&disk_usage);

        async move {
            loop {
                let mut quota_output_cache: HashMap<PathBuf, String> = HashMap::new();

                for (server, (path, project_id, server_usage)) in
                    disk_usage.write().await.iter_mut()
                {
                    *server_usage = -1;

                    let output_str = if let Some(cached) = quota_output_cache.get(path) {
                        cached.clone()
                    } else {
                        let output = Command::new("xfs_quota")
                            .arg("-x")
                            .arg("-c")
                            .arg("report -p -b")
                            .arg(get_mount_point(path).await.unwrap_or_else(|_| path.clone()))
                            .output()
                            .await;

                        match output {
                            Ok(output) if output.status.success() => {
                                let output_str =
                                    String::from_utf8_lossy(&output.stdout).to_string();
                                quota_output_cache.insert(path.clone(), output_str.clone());
                                output_str
                            }
                            Ok(output) => {
                                tracing::error!(
                                    server = server,
                                    "failed to get XFS quota report: {}",
                                    String::from_utf8_lossy(&output.stderr)
                                );
                                continue;
                            }
                            Err(err) => {
                                tracing::error!("error executing xfs_quota command: {:#?}", err);
                                continue;
                            }
                        }
                    };

                    let mut found_header = false;
                    for line in output_str.lines() {
                        if !found_header {
                            if line.starts_with("Project ID") {
                                found_header = true;
                            }
                            continue;
                        }

                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if parts.len() >= 2
                            && let Some(project_id_str) = parts[0].strip_prefix('#')
                            && let Ok(pid) = project_id_str.parse::<u32>()
                            && pid == *project_id
                            && let Ok(used_bytes) = parts[1].parse::<i64>()
                        {
                            *server_usage = used_bytes * 1024;
                            break;
                        }
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
    });

    disk_usage
});

fn uuid_to_project_id(uuid: &uuid::Uuid) -> u32 {
    let uuid_bytes = uuid.as_bytes();
    u32::from_be_bytes([uuid_bytes[0], uuid_bytes[1], uuid_bytes[2], uuid_bytes[3]])
}

async fn get_mount_point(path: &Path) -> Result<PathBuf, std::io::Error> {
    let output = Command::new("df")
        .arg("--output=target")
        .arg(path)
        .output()
        .await?;

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to get mount point for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = output_str.lines().collect();
    if lines.len() < 2 {
        return Err(std::io::Error::other(format!(
            "Unexpected output from df command for {}: {}",
            path.display(),
            output_str
        )));
    }

    let mount_point = lines[1].trim();

    Ok(PathBuf::from(mount_point))
}

pub async fn setup(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        "setting up xfs disk limiter for volume"
    );

    if tokio::fs::metadata(&filesystem.base_path).await.is_err() {
        tokio::fs::create_dir_all(&filesystem.base_path).await?;
    }

    let mut projects = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(Path::new("/etc/projects"))
        .await?;

    let mut contains_project = false;
    let mut contents = String::new();
    projects.read_to_string(&mut contents).await?;

    for line in contents.lines() {
        if line.starts_with(&format!("{}:", uuid_to_project_id(&filesystem.uuid))) {
            contains_project = true;
            break;
        }
    }

    if !contains_project {
        projects
            .write_all(
                format!(
                    "{}:{}\n",
                    uuid_to_project_id(&filesystem.uuid),
                    filesystem.base_path.display()
                )
                .as_bytes(),
            )
            .await?;
        projects.sync_all().await?;
        drop(projects);
    }

    let project_id = uuid_to_project_id(&filesystem.uuid);

    let output = Command::new("xfs_quota")
        .arg("-x")
        .arg("-c")
        .arg(format!("project -s {project_id}"))
        .arg(get_mount_point(&filesystem.base_path).await?)
        .output()
        .await?;

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to set up XFS project quota for {}: {}",
            filesystem.base_path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    DISK_USAGE.write().await.insert(
        filesystem.uuid.to_string(),
        (filesystem.base_path.clone(), project_id, 0),
    );

    Ok(())
}

pub async fn attach(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        "attaching xfs disk limiter for volume"
    );

    let project_id = uuid_to_project_id(&filesystem.uuid);

    DISK_USAGE.write().await.insert(
        filesystem.uuid.to_string(),
        (filesystem.base_path.clone(), project_id, 0),
    );

    Ok(())
}

pub async fn disk_usage(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<u64, std::io::Error> {
    if let Some(usage) = DISK_USAGE.read().await.get(&filesystem.uuid.to_string())
        && usage.2 >= 0
    {
        return Ok(usage.2 as u64);
    }

    Err(std::io::Error::other(format!(
        "Failed to load XFS disk usage for {}",
        filesystem.base_path.display()
    )))
}

pub async fn update_disk_limit(
    filesystem: &crate::server::filesystem::Filesystem,
    limit: u64,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        limit = limit,
        "setting xfs disk limit"
    );

    let project_id = uuid_to_project_id(&filesystem.uuid);

    let limit_mb = if limit == 0 {
        "0".to_string()
    } else {
        format!("{}m", limit / 1024 / 1024)
    };

    let output = Command::new("xfs_quota")
        .arg("-x")
        .arg("-c")
        .arg(format!(
            "limit -p bsoft={limit_mb} bhard={limit_mb} {project_id}"
        ))
        .arg(get_mount_point(&filesystem.base_path).await?)
        .output()
        .await?;

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to set XFS disk limit for {}: {}",
            filesystem.base_path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(())
}

pub async fn destroy(
    filesystem: &crate::server::filesystem::Filesystem,
) -> Result<(), std::io::Error> {
    tracing::debug!(
        path = %filesystem.base_path.display(),
        "destroying xfs project quota for server"
    );

    update_disk_limit(filesystem, 0).await?;

    if let Ok(mut projects) = File::open(Path::new("/etc/projects")).await {
        let mut contents = String::new();
        projects.read_to_string(&mut contents).await?;

        let project_id = uuid_to_project_id(&filesystem.uuid);
        let new_contents: String = contents
            .lines()
            .filter(|line| !line.starts_with(&format!("{project_id}:")))
            .collect::<Vec<&str>>()
            .join("\n");

        let mut projects_file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(Path::new("/etc/projects"))
            .await?;
        projects_file.write_all(new_contents.as_bytes()).await?;
        projects_file.sync_all().await?;
    }

    tokio::fs::remove_dir_all(&filesystem.base_path).await?;

    DISK_USAGE
        .write()
        .await
        .remove(&filesystem.uuid.to_string());

    Ok(())
}
