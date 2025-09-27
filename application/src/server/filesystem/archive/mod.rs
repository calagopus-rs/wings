use crate::io::{
    abort::{AbortGuard, AbortListener, AbortReader, AbortWriter},
    compression::{
        CompressionType,
        reader::{AsyncCompressionReader, CompressionReader},
    },
    counting_reader::CountingReader,
    counting_writer::CountingWriter,
};
use cap_std::fs::{Permissions, PermissionsExt as _};
use serde::Deserialize;
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
};
use utoipa::ToSchema;

pub mod create;
pub mod multi_reader;

#[derive(Clone, Copy)]
pub enum ArchiveType {
    None,
    Tar,
    Zip,
    Rar,
    SevenZip,
    Ddup,
}

#[derive(ToSchema, Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
#[schema(rename_all = "snake_case")]
pub enum StreamableArchiveFormat {
    Tar,
    #[default]
    TarGz,
    TarXz,
    TarBz2,
    TarLz4,
    TarZstd,
    Zip,
}

impl StreamableArchiveFormat {
    #[inline]
    pub fn compression_format(self) -> CompressionType {
        match self {
            StreamableArchiveFormat::Tar => CompressionType::None,
            StreamableArchiveFormat::TarGz => CompressionType::Gz,
            StreamableArchiveFormat::TarXz => CompressionType::Xz,
            StreamableArchiveFormat::TarBz2 => CompressionType::Bz2,
            StreamableArchiveFormat::TarLz4 => CompressionType::Lz4,
            StreamableArchiveFormat::TarZstd => CompressionType::Zstd,
            StreamableArchiveFormat::Zip => CompressionType::None,
        }
    }

    #[inline]
    pub fn extension(self) -> &'static str {
        match self {
            StreamableArchiveFormat::Tar => "tar",
            StreamableArchiveFormat::TarGz => "tar.gz",
            StreamableArchiveFormat::TarXz => "tar.xz",
            StreamableArchiveFormat::TarBz2 => "tar.bz2",
            StreamableArchiveFormat::TarLz4 => "tar.lz4",
            StreamableArchiveFormat::TarZstd => "tar.zst",
            StreamableArchiveFormat::Zip => "zip",
        }
    }

    #[inline]
    pub fn mime_type(self) -> &'static str {
        match self {
            StreamableArchiveFormat::Tar => "application/x-tar",
            StreamableArchiveFormat::TarGz => "application/gzip",
            StreamableArchiveFormat::TarXz => "application/x-xz",
            StreamableArchiveFormat::TarBz2 => "application/x-bzip2",
            StreamableArchiveFormat::TarLz4 => "application/x-lz4",
            StreamableArchiveFormat::TarZstd => "application/zstd",
            StreamableArchiveFormat::Zip => "application/zip",
        }
    }
}

#[inline]
pub fn zip_entry_get_modified_time(
    entry: &zip::read::ZipFile<impl std::io::Read>,
) -> Option<cap_std::time::SystemTime> {
    for field in entry.extra_data_fields() {
        if let zip::extra_fields::ExtraField::ExtendedTimestamp(ext) = field
            && let Some(mod_time) = ext.mod_time()
        {
            return Some(cap_std::time::SystemTime::from_std(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(mod_time as u64),
            ));
        }
    }

    if let Some(time) = entry.last_modified()
        && time.is_valid()
    {
        let chrono_date = chrono::NaiveDate::from_ymd_opt(
            time.year() as i32,
            time.month() as u32,
            time.day() as u32,
        )?;
        let chrono_time = chrono::NaiveTime::from_hms_opt(
            time.hour() as u32,
            time.minute() as u32,
            time.second() as u32,
        )?;

        return Some(cap_std::time::SystemTime::from_std(
            std::time::UNIX_EPOCH
                + std::time::Duration::from_secs(
                    chrono_date.and_time(chrono_time).and_utc().timestamp() as u64,
                ),
        ));
    }

    None
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
        let mut file = server.filesystem.async_open(&path).await.ok()?;

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
            Some(ext)
                if ["tar", "tgz", "tbz", "tbz2", "txz", "tlz", "tlz4", "tzst"]
                    .contains(&ext.to_str().unwrap_or_default()) =>
            {
                ArchiveType::Tar
            }
            Some(ext) if ext == "zip" => ArchiveType::Zip,
            Some(ext) if ext == "rar" => ArchiveType::Rar,
            Some(ext) if ext == "7z" => ArchiveType::SevenZip,
            Some(ext) if ext == "ddup" => ArchiveType::Ddup,
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

    pub async fn reader(mut self) -> Result<AsyncCompressionReader, anyhow::Error> {
        self.file.seek(SeekFrom::Start(0)).await?;

        Ok(AsyncCompressionReader::new(
            self.file.into_std().await,
            self.compression,
        ))
    }

    pub async fn extract(
        mut self,
        destination: PathBuf,
        progress: Option<Arc<AtomicU64>>,
        total: Option<Arc<AtomicU64>>,
    ) -> Result<(), anyhow::Error> {
        self.file.seek(SeekFrom::Start(0)).await?;

        match self.archive {
            ArchiveType::None => {
                let file_name = match self.path.file_stem() {
                    Some(stem) => destination.join(stem),
                    None => return Err(anyhow::anyhow!("Invalid file name")),
                };

                let metadata = self.server.filesystem.async_metadata(&self.path).await?;

                let file = self.file.into_std().await;
                let (guard, listener) = AbortGuard::new();

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let reader = CompressionReader::new(file, self.compression);
                    let reader = AbortReader::new(reader, listener);
                    let mut reader: Box<dyn Read> = match progress {
                        Some(progress) => {
                            Box::new(CountingReader::new_with_bytes_read(reader, progress))
                        }
                        None => Box::new(reader),
                    };

                    let mut writer = super::writer::FileSystemWriter::new(
                        self.server.clone(),
                        &file_name,
                        Some(metadata.permissions()),
                        metadata.modified().ok(),
                    )?;

                    crate::io::copy(&mut reader, &mut writer)?;
                    writer.flush()?;

                    Ok(())
                })
                .await??;

                drop(guard);
            }
            ArchiveType::Tar => {
                let file = self.file.into_std().await;
                let (guard, listener) = AbortGuard::new();

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let reader: Box<dyn Read> = match progress {
                        Some(progress) => {
                            Box::new(CountingReader::new_with_bytes_read(file, progress))
                        }
                        None => Box::new(file),
                    };
                    let reader = CompressionReader::new(reader, self.compression);
                    let reader = AbortReader::new(reader, listener);

                    if let Some(total) = total
                        && let Ok(metadata) = self.server.filesystem.metadata(&self.path)
                    {
                        total.store(metadata.len(), Ordering::Relaxed);
                    }

                    let mut archive = tar::Archive::new(reader);
                    let mut directory_entries = chunked_vec::ChunkedVec::new();
                    let mut entries = archive.entries()?;

                    let mut read_buffer = vec![0; crate::BUFFER_SIZE];
                    while let Some(Ok(mut entry)) = entries.next() {
                        let path = entry.path()?;

                        if path.is_absolute() {
                            continue;
                        }

                        let destination_path = destination.join(path.as_ref());
                        let header = entry.header();

                        if self
                            .server
                            .filesystem
                            .is_ignored_sync(&destination_path, header.entry_type().is_dir())
                        {
                            continue;
                        }

                        match header.entry_type() {
                            tar::EntryType::Directory => {
                                self.server.filesystem.create_dir_all(&destination_path)?;
                                if let Ok(permissions) = header.mode().map(Permissions::from_mode) {
                                    self.server
                                        .filesystem
                                        .set_permissions(&destination_path, permissions)?;
                                }

                                if let Ok(modified_time) = header.mtime() {
                                    directory_entries.push((destination_path, modified_time));
                                }
                            }
                            tar::EntryType::Regular => {
                                if let Some(parent) = destination_path.parent() {
                                    self.server.filesystem.create_dir_all(parent)?;
                                }

                                let mut writer = super::writer::FileSystemWriter::new(
                                    self.server.clone(),
                                    &destination_path,
                                    header.mode().map(Permissions::from_mode).ok(),
                                    header
                                        .mtime()
                                        .map(|t| {
                                            cap_std::time::SystemTime::from_std({
                                                std::time::UNIX_EPOCH
                                                    + std::time::Duration::from_secs(t)
                                            })
                                        })
                                        .ok(),
                                )?;

                                crate::io::copy_shared(&mut read_buffer, &mut entry, &mut writer)?;
                                writer.flush()?;
                            }
                            tar::EntryType::Symlink => {
                                let link =
                                    entry.link_name().unwrap_or_default().unwrap_or_default();

                                if let Err(err) =
                                    self.server.filesystem.symlink(link, &destination_path)
                                {
                                    tracing::debug!(
                                        path = %path.display(),
                                        "failed to create symlink from archive: {:#?}",
                                        err
                                    );
                                } else if let Ok(modified_time) = header.mtime() {
                                    self.server.filesystem.set_times(
                                        &destination_path,
                                        std::time::UNIX_EPOCH
                                            + std::time::Duration::from_secs(modified_time),
                                        None,
                                    )?;
                                }
                            }
                            _ => {}
                        }
                    }

                    for (destination_path, modified_time) in directory_entries {
                        self.server.filesystem.set_times(
                            &destination_path,
                            std::time::UNIX_EPOCH + std::time::Duration::from_secs(modified_time),
                            None,
                        )?;
                    }

                    Ok(())
                })
                .await??;

                drop(guard)
            }
            ArchiveType::Zip => {
                let file = Arc::new(self.file.into_std().await);
                let (guard, listener) = AbortGuard::new();

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let reader = multi_reader::MultiReader::new(file)?;
                    let reader = AbortReader::new(reader, listener);
                    let mut archive = zip::ZipArchive::new(reader)?;
                    let entry_index = Arc::new(AtomicUsize::new(0));

                    if let Some(total) = total {
                        let mut entry_total = 0;
                        for i in 0..archive.len() {
                            let entry = archive.by_index(i)?;
                            entry_total += entry.size();
                        }

                        total.store(entry_total, Ordering::Relaxed);
                    }

                    let pool = rayon::ThreadPoolBuilder::new()
                        .num_threads(self.server.app_state.config.api.file_decompression_threads)
                        .build()?;

                    let error = Arc::new(RwLock::new(None));

                    pool.in_place_scope(|scope| {
                        let archive = archive.clone();
                        let destination = destination.clone();
                        let server = self.server.clone();
                        let error_clone = Arc::clone(&error);

                        scope.spawn_broadcast(move |_, _| {
                            let mut archive = archive.clone();
                            let progress = progress.clone();
                            let entry_index = Arc::clone(&entry_index);
                            let error_clone2 = Arc::clone(&error_clone);
                            let destination = destination.clone();
                            let server = server.clone();

                            let mut run = move || -> Result<(), anyhow::Error> {
                                let mut read_buffer = vec![0; crate::BUFFER_SIZE];

                                loop {
                                    if error_clone2.read().unwrap().is_some() {
                                        return Ok(());
                                    }

                                    let i = entry_index.fetch_add(1, Ordering::SeqCst);
                                    if i >= archive.len() {
                                        return Ok(());
                                    }

                                    let mut entry = archive.by_index(i)?;
                                    let path = match entry.enclosed_name() {
                                        Some(path) => path,
                                        None => continue,
                                    };

                                    if path.is_absolute() {
                                        continue;
                                    }

                                    let destination_path = destination.join(path);

                                    if server
                                        .filesystem
                                        .is_ignored_sync(&destination_path, entry.is_dir())
                                    {
                                        continue;
                                    }

                                    if entry.is_dir() {
                                        server.filesystem.create_dir_all(&destination_path)?;
                                        server.filesystem.set_permissions(
                                            &destination_path,
                                            cap_std::fs::Permissions::from_mode(
                                                entry.unix_mode().unwrap_or(0o755),
                                            ),
                                        )?;
                                    } else if entry.is_file() {
                                        if let Some(parent) = destination_path.parent() {
                                            server.filesystem.create_dir_all(parent)?;
                                        }

                                        let mut writer = super::writer::FileSystemWriter::new(
                                            server.clone(),
                                            &destination_path,
                                            entry.unix_mode().map(Permissions::from_mode),
                                            zip_entry_get_modified_time(&entry),
                                        )?;

                                        let mut reader: Box<dyn Read> = match &progress {
                                            Some(progress) => {
                                                Box::new(CountingReader::new_with_bytes_read(
                                                    entry,
                                                    Arc::clone(progress),
                                                ))
                                            }
                                            None => Box::new(entry),
                                        };

                                        crate::io::copy_shared(
                                            &mut read_buffer,
                                            &mut reader,
                                            &mut writer,
                                        )?;
                                        writer.flush()?;
                                    } else if entry.is_symlink()
                                        && (1..=2048).contains(&entry.size())
                                    {
                                        let link =
                                            std::io::read_to_string(&mut entry).unwrap_or_default();

                                        if let Err(err) =
                                            server.filesystem.symlink(link, &destination_path)
                                        {
                                            tracing::debug!(
                                                path = %destination_path.display(),
                                                "failed to create symlink from archive: {:#?}",
                                                err
                                            );
                                        } else if let Some(modified_time) =
                                            zip_entry_get_modified_time(&entry)
                                        {
                                            server.filesystem.set_times(
                                                &destination_path,
                                                modified_time.into_std(),
                                                None,
                                            )?;
                                        }

                                        if let Some(progress) = &progress {
                                            progress.fetch_add(entry.size(), Ordering::Relaxed);
                                        }
                                    }
                                }
                            };

                            if let Err(err) = run() {
                                error_clone.write().unwrap().replace(err);
                            }
                        });
                    });

                    if let Some(err) = error.write().unwrap().take() {
                        Err(err)
                    } else {
                        for i in 0..archive.len() {
                            let entry = archive.by_index(i)?;

                            if entry.is_dir() {
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

                                if let Some(modified_time) = zip_entry_get_modified_time(&entry) {
                                    self.server.filesystem.set_times(
                                        &destination_path,
                                        modified_time.into_std(),
                                        None,
                                    )?;
                                }
                            }
                        }

                        Ok(())
                    }
                })
                .await??;

                drop(guard);
            }
            ArchiveType::Rar => {
                let (guard, listener) = AbortGuard::new();

                fn dos_time_to_unix(dos_time: u32) -> Option<u64> {
                    let seconds = (dos_time & 0x1F) * 2;
                    let minutes = (dos_time >> 5) & 0x3F;
                    let hours = (dos_time >> 11) & 0x1F;
                    let day = (dos_time >> 16) & 0x1F;
                    let month = (dos_time >> 21) & 0x0F;
                    let year = ((dos_time >> 25) & 0x7F) + 1980;

                    if seconds >= 60
                        || minutes >= 60
                        || hours >= 24
                        || !(1..=31).contains(&day)
                        || !(1..=12).contains(&month)
                    {
                        return None;
                    }

                    let date = chrono::NaiveDate::from_ymd_opt(year as i32, month, day)?;
                    let time = chrono::NaiveTime::from_hms_opt(hours, minutes, seconds)?;

                    Some(chrono::NaiveDateTime::new(date, time).and_utc().timestamp() as u64)
                }

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    drop(self.file);

                    if let Some(total) = total {
                        let mut entry_total = 0;
                        let archive = unrar::Archive::new_owned(
                            self.server.filesystem.base_path.join(&self.path),
                        )
                        .open_for_listing()?;
                        for entry in archive.flatten() {
                            entry_total += entry.unpacked_size;
                        }

                        total.store(entry_total, Ordering::Relaxed);
                    }

                    let mut archive =
                        unrar::Archive::new_owned(self.server.filesystem.base_path.join(self.path))
                            .open_for_processing()?;
                    let mut directory_entries = chunked_vec::ChunkedVec::new();

                    loop {
                        let entry = match archive.read_header()? {
                            Some(entry) => entry,
                            None => break,
                        };

                        let path = &entry.entry().filename;
                        if path.is_absolute() {
                            archive = entry.skip()?;
                            continue;
                        }

                        if self
                            .server
                            .filesystem
                            .is_ignored_sync(path, entry.entry().is_directory())
                        {
                            archive = entry.skip()?;
                            continue;
                        }

                        if listener.is_aborted() {
                            return Err(anyhow::anyhow!("operation aborted"));
                        }

                        let destination_path = destination.join(path);

                        if entry.entry().is_directory() {
                            self.server.filesystem.create_dir_all(&destination_path)?;

                            if let Some(modified_time) = dos_time_to_unix(entry.entry().file_time) {
                                directory_entries.push((destination_path, modified_time));
                            }

                            archive = entry.skip()?;
                            continue;
                        } else {
                            if let Some(parent) = destination_path.parent() {
                                self.server.filesystem.create_dir_all(parent)?;
                            }

                            let writer = super::writer::FileSystemWriter::new(
                                self.server.clone(),
                                &destination_path,
                                None,
                                dos_time_to_unix(entry.entry().file_time).map(|secs| {
                                    cap_std::time::SystemTime::from_std(
                                        std::time::UNIX_EPOCH
                                            + std::time::Duration::from_secs(secs),
                                    )
                                }),
                            )?;
                            let writer = AbortWriter::new(writer, listener.clone());
                            let writer: Box<dyn Write + Send + Sync> = match &progress {
                                Some(progress) => Box::new(CountingWriter::new_with_bytes_written(
                                    writer,
                                    Arc::clone(progress),
                                )),
                                None => Box::new(writer),
                            };

                            let (unrar::Stream(writer, err), processed_archive) =
                                entry.read_to_stream(writer)?;
                            if let Some(mut writer) = writer {
                                writer.flush()?;
                            }

                            if let Some(err) = err {
                                return Err(err.into());
                            }

                            archive = processed_archive;
                        }
                    }

                    for (destination_path, modified_time) in directory_entries {
                        self.server.filesystem.set_times(
                            &destination_path,
                            std::time::UNIX_EPOCH + std::time::Duration::from_secs(modified_time),
                            None,
                        )?;
                    }

                    Ok(())
                })
                .await??;

                drop(guard);
            }
            ArchiveType::SevenZip => {
                let file = Arc::new(self.file.into_std().await);
                let (guard, listener) = AbortGuard::new();

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    let reader = multi_reader::MultiReader::new(file)?;
                    let reader = AbortReader::new(reader, listener);
                    let password = sevenz_rust2::Password::empty();
                    let archive = sevenz_rust2::Archive::read(&mut reader.clone(), &password)?;

                    if let Some(total) = total {
                        total.store(
                            archive.files.iter().map(|f| f.size).sum(),
                            Ordering::Relaxed,
                        );
                    }

                    let pool = rayon::ThreadPoolBuilder::new()
                        .num_threads(self.server.app_state.config.api.file_decompression_threads)
                        .build()
                        .unwrap();

                    let error = Arc::new(RwLock::new(None));

                    pool.in_place_scope(|scope| {
                        for block_index in 0..archive.blocks.len() {
                            let archive = archive.clone();
                            let progress = progress.clone();
                            let mut reader = reader.clone();
                            let destination = destination.clone();
                            let server = self.server.clone();
                            let error_clone = Arc::clone(&error);

                            scope.spawn(move |_| {
                                if error_clone.read().unwrap().is_some() {
                                    return;
                                }

                                let password = sevenz_rust2::Password::empty();
                                let folder = sevenz_rust2::BlockDecoder::new(
                                    1,
                                    block_index,
                                    &archive,
                                    &password,
                                    &mut reader,
                                );

                                let mut read_buffer = vec![0; crate::BUFFER_SIZE];
                                if let Err(err) = folder.for_each_entries(&mut |entry, reader| {
                                    let path = entry.name();
                                    if path.starts_with('/') || path.starts_with('\\') {
                                        return Ok(true);
                                    }

                                    let destination_path = destination.join(path);

                                    if server
                                        .filesystem
                                        .is_ignored_sync(&destination_path, entry.is_directory())
                                    {
                                        return Ok(true);
                                    }

                                    if entry.is_directory() {
                                        if let Err(err) =
                                            server.filesystem.create_dir_all(&destination_path)
                                        {
                                            return Err(sevenz_rust2::Error::Other(
                                                err.to_string().into(),
                                            ));
                                        }
                                    } else {
                                        if let Some(parent) = destination_path.parent()
                                            && let Err(err) =
                                                server.filesystem.create_dir_all(parent)
                                        {
                                            return Err(sevenz_rust2::Error::Other(
                                                err.to_string().into(),
                                            ));
                                        }

                                        let mut writer = super::writer::FileSystemWriter::new(
                                            server.clone(),
                                            &destination_path,
                                            None,
                                            if entry.has_last_modified_date {
                                                Some(cap_std::time::SystemTime::from_std(
                                                    entry.last_modified_date.into(),
                                                ))
                                            } else {
                                                None
                                            },
                                        )
                                        .map_err(|e| std::io::Error::other(e.to_string()))?;

                                        let mut reader: Box<dyn Read> = match &progress {
                                            Some(progress) => {
                                                Box::new(CountingReader::new_with_bytes_read(
                                                    reader,
                                                    Arc::clone(progress),
                                                ))
                                            }
                                            None => Box::new(reader),
                                        };

                                        crate::io::copy_shared(
                                            &mut read_buffer,
                                            &mut reader,
                                            &mut writer,
                                        )?;
                                        writer.flush()?;
                                    }

                                    Ok(true)
                                }) {
                                    error_clone.write().unwrap().replace(err);
                                }
                            });
                        }
                    });

                    if let Some(err) = error.write().unwrap().take() {
                        Err(err.into())
                    } else {
                        for entry in archive.files {
                            if entry.is_directory() && entry.has_last_modified_date {
                                let path = entry.name();
                                if path.starts_with('/') || path.starts_with('\\') {
                                    continue;
                                }

                                let destination_path = destination.join(path);

                                if self
                                    .server
                                    .filesystem
                                    .is_ignored_sync(&destination_path, entry.is_directory())
                                {
                                    continue;
                                }

                                self.server.filesystem.set_times(
                                    &destination_path,
                                    entry.last_modified_date.into(),
                                    None,
                                )?;
                            }
                        }

                        Ok(())
                    }
                })
                .await??;

                drop(guard);
            }
            ArchiveType::Ddup => {
                let mut file = self.file.into_std().await;
                let (guard, listener) = AbortGuard::new();

                tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                    file.seek(SeekFrom::Start(0))?;
                    let archive = ddup_bak::archive::Archive::open_file(file)?;

                    if let Some(total) = total {
                        fn recursive_size(entry: &ddup_bak::archive::entries::Entry) -> u64 {
                            match entry {
                                ddup_bak::archive::entries::Entry::File(file) => file.size,
                                ddup_bak::archive::entries::Entry::Directory(dir) => {
                                    dir.entries.iter().map(recursive_size).sum()
                                }
                                _ => 0,
                            }
                        }

                        total.store(
                            archive.entries().iter().map(recursive_size).sum(),
                            Ordering::Relaxed,
                        );
                    }

                    let pool = rayon::ThreadPoolBuilder::new()
                        .num_threads(self.server.app_state.config.api.file_decompression_threads)
                        .build()
                        .unwrap();

                    fn recursive_traverse(
                        scope: &rayon::Scope,
                        listener: &AbortListener,
                        progress: &Option<Arc<AtomicU64>>,
                        server: &crate::server::Server,
                        destination: &Path,
                        entry: ddup_bak::archive::entries::Entry,
                    ) -> Result<(), anyhow::Error> {
                        let destination_path = destination.join(entry.name());
                        if server
                            .filesystem
                            .is_ignored_sync(&destination_path, entry.is_directory())
                        {
                            return Ok(());
                        }

                        if listener.is_aborted() {
                            return Err(anyhow::anyhow!("operation aborted"));
                        }

                        match entry {
                            ddup_bak::archive::entries::Entry::Directory(dir) => {
                                server.filesystem.create_dir_all(&destination_path)?;
                                server.filesystem.set_permissions(
                                    &destination_path,
                                    cap_std::fs::Permissions::from_std(dir.mode.into()),
                                )?;

                                for entry in dir.entries {
                                    recursive_traverse(
                                        scope,
                                        listener,
                                        progress,
                                        server,
                                        &destination_path,
                                        entry,
                                    )?;
                                }

                                server
                                    .filesystem
                                    .set_times(&destination_path, dir.mtime, None)?;
                            }
                            ddup_bak::archive::entries::Entry::File(file) => {
                                let writer = super::writer::FileSystemWriter::new(
                                    server.clone(),
                                    &destination_path,
                                    Some(cap_std::fs::Permissions::from_std(file.mode.into())),
                                    Some(cap_std::time::SystemTime::from_std(file.mtime)),
                                )?;
                                let mut writer = AbortWriter::new(writer, listener.clone());

                                let reader = AbortReader::new(file, listener.clone());
                                let mut reader: Box<dyn Read + Send> = match progress {
                                    Some(progress) => {
                                        Box::new(CountingReader::new_with_bytes_read(
                                            reader,
                                            Arc::clone(progress),
                                        ))
                                    }
                                    None => Box::new(reader),
                                };

                                scope.spawn(move |_| {
                                    crate::io::copy(&mut reader, &mut writer).unwrap();
                                    writer.flush().unwrap();
                                });
                            }
                            ddup_bak::archive::entries::Entry::Symlink(link) => {
                                if let Err(err) =
                                    server.filesystem.symlink(link.target, &destination_path)
                                {
                                    tracing::debug!(
                                        path = %destination_path.display(),
                                        "failed to create symlink from archive: {:#?}",
                                        err
                                    );
                                } else {
                                    server.filesystem.set_times(
                                        &destination_path,
                                        link.mtime,
                                        None,
                                    )?;
                                }
                            }
                        }

                        Ok(())
                    }

                    pool.in_place_scope(|scope| {
                        for entry in archive.into_entries() {
                            recursive_traverse(
                                scope,
                                &listener,
                                &progress,
                                &self.server,
                                &destination,
                                entry,
                            )?;
                        }

                        Ok(())
                    })
                })
                .await??;

                drop(guard);
            }
        }

        Ok(())
    }
}
