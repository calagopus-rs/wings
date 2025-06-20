use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::Permissions,
    io::{SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
};
use tokio::{
    fs::File,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader},
};
use utoipa::ToSchema;

#[derive(Clone, Copy)]
pub enum CompressionType {
    None,
    Gz,
    Xz,
    Bz2,
    Lz4,
    Zstd,
}

#[derive(Clone, Copy, ToSchema, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
#[schema(rename_all = "snake_case")]
pub enum CompressionLevel {
    #[default]
    BestSpeed,
    GoodSpeed,
    GoodCompression,
    BestCompression,
}

impl CompressionLevel {
    #[inline]
    pub fn flate2_compression_level(&self) -> flate2::Compression {
        match self {
            CompressionLevel::BestSpeed => flate2::Compression::new(1),
            CompressionLevel::GoodSpeed => flate2::Compression::new(3),
            CompressionLevel::GoodCompression => flate2::Compression::new(6),
            CompressionLevel::BestCompression => flate2::Compression::new(9),
        }
    }

    #[inline]
    pub fn zstd_compression_level(&self) -> i32 {
        match self {
            CompressionLevel::BestSpeed => 1,
            CompressionLevel::GoodSpeed => 7,
            CompressionLevel::GoodCompression => 13,
            CompressionLevel::BestCompression => 22,
        }
    }
}

#[derive(Clone, Copy)]
pub enum ArchiveType {
    None,
    Tar,
    Zip,
    SevenZip,
}

pub struct Archive {
    pub compression: CompressionType,
    pub archive: ArchiveType,

    pub server: crate::server::Server,
    pub header: [u8; 16],

    pub file: File,
    pub path: PathBuf,
}

impl Archive {
    pub async fn open(server: crate::server::Server, path: PathBuf) -> Option<Self> {
        let mut file = server.filesystem.open(&path).await.ok()?;

        let mut header = [0; 16];
        #[allow(clippy::unused_io_amount)]
        file.read(&mut header).await.ok()?;

        let inferred = infer::get(&header);
        let compression_format = match inferred.map(|f| f.mime_type()) {
            Some("application/gzip") => CompressionType::Gz,
            Some("application/x-bzip2") => CompressionType::Bz2,
            Some("application/x-xz") => CompressionType::Xz,
            Some("application/x-lz4") => CompressionType::Lz4,
            Some("application/zstd") => CompressionType::Zstd,
            _ => CompressionType::None,
        };

        let archive_format = match path.extension() {
            Some(ext) if ext == "tar" => ArchiveType::Tar,
            Some(ext) if ext == "zip" => ArchiveType::Zip,
            Some(ext) if ext == "7z" => ArchiveType::SevenZip,
            _ => path.file_stem().map_or(ArchiveType::None, |stem| {
                if stem.to_str().is_some_and(|s| s.ends_with(".tar")) {
                    ArchiveType::Tar
                } else {
                    ArchiveType::None
                }
            }),
        };

        Some(Self {
            compression: compression_format,
            archive: archive_format,
            server,
            header,
            file,
            path,
        })
    }

    pub async fn estimated_size(&mut self) -> Option<u64> {
        match self.compression {
            CompressionType::None => Some(self.file.metadata().await.ok()?.len()),
            CompressionType::Gz => {
                let file_size = self.file.metadata().await.ok()?.len();

                if file_size < 4 {
                    return None;
                }

                if self.file.seek(SeekFrom::End(-4)).await.is_err() {
                    return None;
                }

                let mut buffer = [0; 4];
                if self.file.read_exact(&mut buffer).await.is_err() {
                    return None;
                }

                Some(u32::from_le_bytes(buffer) as u64)
            }
            CompressionType::Xz => None,
            CompressionType::Bz2 => None,
            CompressionType::Lz4 => {
                if self.header[0..4] != [0x04, 0x22, 0x4D, 0x18] {
                    return None;
                }

                let flags = self.header[4];
                let has_content_size = (flags & 0x08) != 0;

                if !has_content_size || self.header.len() < 13 {
                    return None;
                }

                Some(u64::from_le_bytes(self.header[5..13].try_into().ok()?))
            }
            CompressionType::Zstd => {
                if self.header[0..4] != [0x28, 0xB5, 0x2F, 0xFD] {
                    return None;
                }

                let frame_header_descriptor = self.header[4];

                let fcs_flag = frame_header_descriptor & 0x03;
                let single_segment = (frame_header_descriptor & 0x20) != 0;

                if fcs_flag == 0 && !single_segment {
                    return None;
                }

                let size_bytes = match fcs_flag {
                    0 => {
                        if single_segment {
                            1
                        } else {
                            return None;
                        }
                    }
                    1 => 2,
                    2 => 4,
                    3 => 8,
                    _ => return None,
                };

                let size_buffer = &self.header[5..13];

                match size_bytes {
                    1 => Some(size_buffer[0] as u64),
                    2 => Some(u16::from_le_bytes([size_buffer[0], size_buffer[1]]) as u64),
                    4 => Some(u32::from_le_bytes([
                        size_buffer[0],
                        size_buffer[1],
                        size_buffer[2],
                        size_buffer[3],
                    ]) as u64),
                    8 => Some(u64::from_le_bytes(size_buffer.try_into().ok()?)),
                    _ => None,
                }
            }
        }
    }

    pub async fn reader(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        self.file.seek(SeekFrom::Start(0)).await.ok()?;

        let file = BufReader::new(self.file.try_clone().await.ok()?);

        let reader: Box<dyn AsyncRead + Send + Unpin> = match self.compression {
            CompressionType::None => Box::new(file),
            CompressionType::Gz => {
                Box::new(async_compression::tokio::bufread::GzipDecoder::new(file))
            }
            CompressionType::Xz => {
                Box::new(async_compression::tokio::bufread::XzDecoder::new(file))
            }
            CompressionType::Bz2 => {
                Box::new(async_compression::tokio::bufread::BzDecoder::new(file))
            }
            CompressionType::Lz4 => {
                Box::new(async_compression::tokio::bufread::Lz4Decoder::new(file))
            }
            CompressionType::Zstd => {
                Box::new(async_compression::tokio::bufread::ZstdDecoder::new(file))
            }
        };

        Some(reader)
    }

    pub async fn extract(
        self,
        destination: PathBuf,
        reader: Option<Box<dyn AsyncRead + Send + Unpin>>,
    ) -> Result<(), anyhow::Error> {
        match self.archive {
            ArchiveType::None => {
                let file_name = match self.path.file_stem() {
                    Some(stem) => destination.join(stem),
                    None => destination,
                };

                let metadata = self.file.metadata().await?;

                let mut writer = super::writer::AsyncFileSystemWriter::new(
                    self.server.clone(),
                    file_name,
                    Some(metadata.permissions()),
                    metadata.modified().ok(),
                )
                .await?;

                tokio::io::copy(&mut reader.unwrap(), &mut writer).await?;
                writer.flush().await?;
            }
            ArchiveType::Tar => {
                let mut archive = tokio_tar::Archive::new(reader.unwrap());

                let mut entries = archive.entries()?;
                while let Some(Ok(mut entry)) = entries.next().await {
                    let path = entry.path().unwrap();

                    if path.is_absolute() {
                        continue;
                    }

                    let destination_path = destination.join(path.as_ref());
                    let header = entry.header();

                    if self
                        .server
                        .filesystem
                        .is_ignored(
                            &destination_path,
                            header.entry_type() == tokio_tar::EntryType::Directory,
                        )
                        .await
                    {
                        continue;
                    }

                    match header.entry_type() {
                        tokio_tar::EntryType::Directory => {
                            self.server
                                .filesystem
                                .create_dir_all(&destination_path)
                                .await?;
                        }
                        tokio_tar::EntryType::Regular => {
                            self.server
                                .filesystem
                                .create_dir_all(destination_path.parent().unwrap())
                                .await?;

                            let mut writer = super::writer::AsyncFileSystemWriter::new(
                                self.server.clone(),
                                destination_path,
                                header.mode().map(Permissions::from_mode).ok(),
                                header
                                    .mtime()
                                    .map(|t| {
                                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(t)
                                    })
                                    .ok(),
                            )
                            .await
                            .unwrap();

                            tokio::io::copy(&mut entry, &mut writer).await.unwrap();
                            writer.flush().await.unwrap();
                        }
                        tokio_tar::EntryType::Symlink => {
                            let link = entry.link_name().unwrap_or_default().unwrap_or_default();

                            self.server
                                .filesystem
                                .symlink(link.as_ref(), destination_path)
                                .await
                                .unwrap_or_else(|err| {
                                    tracing::debug!(
                                        "failed to create symlink from archive: {:#?}",
                                        err
                                    );
                                });
                        }
                        _ => {}
                    }
                }
            }
            ArchiveType::Zip => {
                let filesystem = self.server.filesystem.base_dir().await?;

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let file = self.file.try_into_std().unwrap();

                    let mut archive = zip::ZipArchive::new(file)?;

                    for i in 0..archive.len() {
                        let mut entry = archive.by_index(i)?;
                        let path = match entry.enclosed_name() {
                            Some(path) => path,
                            None => continue,
                        };

                        if path.is_absolute() {
                            continue;
                        }

                        let destination_path = destination.join(path);

                        if self
                            .server
                            .filesystem
                            .is_ignored_sync(&destination_path, entry.is_dir())
                        {
                            continue;
                        }

                        if entry.is_dir() {
                            filesystem.create_dir_all(&destination_path)?;
                        } else {
                            filesystem.create_dir_all(destination_path.parent().unwrap())?;

                            let mut writer = super::writer::FileSystemWriter::new(
                                self.server.clone(),
                                destination_path,
                                entry.unix_mode().map(Permissions::from_mode),
                                None,
                            )?;

                            std::io::copy(&mut entry, &mut writer)?;
                            writer.flush()?;
                        }
                    }

                    Ok(())
                })
                .await??;
            }
            ArchiveType::SevenZip => {
                let filesystem = self.server.filesystem.base_dir().await?;

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let mut file = self.file.try_into_std().unwrap();
                    let archive = sevenz_rust2::Archive::read(&mut file, &[])?;

                    for folder_index in 0..archive.folders.len() {
                        let folder =
                            sevenz_rust2::BlockDecoder::new(folder_index, &archive, &[], &mut file);

                        folder.for_each_entries(&mut |entry, reader| {
                            let path = entry.name();
                            if path.starts_with('/') || path.starts_with('\\') {
                                return Ok(true);
                            }

                            let destination_path = destination.join(path);

                            if self
                                .server
                                .filesystem
                                .is_ignored_sync(&destination_path, entry.is_directory())
                            {
                                return Ok(true);
                            }

                            if entry.is_directory() {
                                filesystem.create_dir_all(&destination_path)?;
                            } else {
                                filesystem.create_dir_all(destination_path.parent().unwrap())?;

                                let mut writer = super::writer::FileSystemWriter::new(
                                    self.server.clone(),
                                    destination_path,
                                    None,
                                    if entry.has_last_modified_date {
                                        Some(entry.last_modified_date.into())
                                    } else {
                                        None
                                    },
                                )
                                .map_err(|e| std::io::Error::other(e.to_string()))?;

                                std::io::copy(reader, &mut writer)?;
                                writer.flush()?;
                            }

                            Ok(true)
                        })?;
                    }

                    Ok(())
                })
                .await??;
            }
        }

        Ok(())
    }
}
