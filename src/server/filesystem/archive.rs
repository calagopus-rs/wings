use std::{
    fs::Permissions,
    io::{SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::Arc,
};
use tokio::{
    fs::File,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, BufReader},
};
use tokio_util::io::SyncIoBridge;

#[derive(Clone, Copy)]
pub enum CompressionType {
    None,
    Gz,
    Xz,
    Bz2,
    Lz4,
    Zstd,
}

#[derive(Clone, Copy)]
pub enum ArchiveType {
    None,
    Tar,
    Zip,
}

pub struct Archive {
    pub compression: CompressionType,
    pub archive: ArchiveType,

    pub filesystem: Arc<super::Filesystem>,
    pub header: [u8; 16],

    pub file: File,
}

impl Archive {
    pub async fn open(filesystem: Arc<super::Filesystem>, path: PathBuf) -> Option<Self> {
        let mut file = File::open(&path).await.ok()?;

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
            _ => path.file_stem().map_or(ArchiveType::None, |stem| {
                if stem.to_str().is_some_and(|s| s.ends_with(".tar")) {
                    ArchiveType::Tar
                } else {
                    ArchiveType::None
                }
            }),
        };

        if !path.symlink_metadata().ok()?.is_file() {
            return None;
        }

        Some(Self {
            compression: compression_format,
            archive: archive_format,
            filesystem,
            header,
            file,
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
    ) -> std::io::Result<()> {
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            match self.archive {
                ArchiveType::Tar => {
                    let sync_reader = SyncIoBridge::new(reader.unwrap());
                    let mut archive = tar::Archive::new(sync_reader);

                    for entry in archive.entries().unwrap() {
                        let mut entry = entry.unwrap();
                        let path = entry.path().unwrap();

                        if path.is_absolute() {
                            continue;
                        }

                        let destination_path = destination.join(path);
                        if !self.filesystem.is_safe_path(&destination_path) {
                            continue;
                        }

                        let header = entry.header();
                        match header.entry_type() {
                            tar::EntryType::Directory => {
                                std::fs::create_dir_all(&destination_path).unwrap();
                            }
                            tar::EntryType::Regular => {
                                std::fs::create_dir_all(destination_path.parent().unwrap())
                                    .unwrap();

                                let mut writer = super::writer::FileSystemWriter::new(
                                    Arc::clone(&self.filesystem),
                                    destination_path,
                                    header.mode().map(Permissions::from_mode).ok(),
                                )
                                .unwrap();

                                std::io::copy(&mut entry, &mut writer).unwrap();
                                writer.flush().unwrap();
                            }
                            _ => {}
                        }
                    }
                }
                ArchiveType::Zip => {
                    let file = self.file.try_into_std().unwrap();
                    let file = std::io::BufReader::new(file);

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
                        if !self.filesystem.is_safe_path(&destination_path) {
                            continue;
                        }

                        if entry.is_dir() {
                            std::fs::create_dir_all(&destination_path)?;
                        } else {
                            let mut writer = super::writer::FileSystemWriter::new(
                                Arc::clone(&self.filesystem),
                                destination_path,
                                entry.unix_mode().map(Permissions::from_mode),
                            )?;

                            std::io::copy(&mut entry, &mut writer)?;
                            writer.flush()?;
                        }
                    }
                }
                ArchiveType::None => {}
            }

            Ok(())
        })
        .await?
    }
}
