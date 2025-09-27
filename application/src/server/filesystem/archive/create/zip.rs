use crate::io::{
    abort::{AbortGuard, AbortWriter},
    compression::CompressionLevel,
    counting_reader::CountingReader,
};
use cap_std::fs::PermissionsExt;
use chrono::{Datelike, Timelike};
use std::{
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub struct CreateZipOptions {
    pub compression_level: CompressionLevel,
}

pub async fn create_zip(
    filesystem: crate::server::filesystem::cap::CapFilesystem,
    destination: impl Write + Seek + Send + 'static,
    base: &Path,
    sources: Vec<PathBuf>,
    bytes_archived: Option<Arc<AtomicU64>>,
    ignored: Vec<ignore::gitignore::Gitignore>,
    options: CreateZipOptions,
) -> Result<(), anyhow::Error> {
    let base = filesystem.relative_path(base);
    let (_guard, listener) = AbortGuard::new();

    tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        let writer = AbortWriter::new(destination, listener);
        let mut archive = zip::ZipWriter::new(writer);

        let mut read_buffer = vec![0; crate::BUFFER_SIZE];
        for source in sources {
            let relative = source;
            let source = base.join(&relative);

            let source_metadata = match filesystem.symlink_metadata(&source) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if ignored
                .iter()
                .any(|i| i.matched(&source, source_metadata.is_dir()).is_ignore())
            {
                continue;
            }

            let mut zip_options: zip::write::FileOptions<'_, ()> =
                zip::write::FileOptions::default()
                    .compression_level(Some(options.compression_level.to_deflate_level() as i64))
                    .unix_permissions(source_metadata.permissions().mode())
                    .large_file(true);

            if let Ok(mtime) = source_metadata.modified() {
                let mtime: chrono::DateTime<chrono::Utc> = chrono::DateTime::from(mtime.into_std());

                if let Ok(mtime) = zip::DateTime::from_date_and_time(
                    mtime.year() as u16,
                    mtime.month() as u8,
                    mtime.day() as u8,
                    mtime.hour() as u8,
                    mtime.minute() as u8,
                    mtime.second() as u8,
                ) {
                    zip_options = zip_options.last_modified_time(mtime);
                }
            }

            if source_metadata.is_dir() {
                archive.add_directory(relative.to_string_lossy(), zip_options)?;
                if let Some(bytes_archived) = &bytes_archived {
                    bytes_archived.fetch_add(source_metadata.len(), Ordering::SeqCst);
                }

                let mut walker = filesystem.walk_dir(source)?.with_ignored(&ignored);
                while let Some(Ok((_, path))) = walker.next_entry() {
                    let relative = match path.strip_prefix(&base) {
                        Ok(path) => path,
                        Err(_) => continue,
                    };

                    let metadata = match filesystem.symlink_metadata(&path) {
                        Ok(metadata) => metadata,
                        Err(_) => continue,
                    };

                    let mut zip_options: zip::write::FileOptions<'_, ()> =
                        zip::write::FileOptions::default()
                            .compression_level(Some(
                                options.compression_level.to_deflate_level() as i64
                            ))
                            .unix_permissions(metadata.permissions().mode())
                            .large_file(true);

                    if let Ok(mtime) = metadata.modified() {
                        let mtime: chrono::DateTime<chrono::Utc> =
                            chrono::DateTime::from(mtime.into_std());

                        if let Ok(mtime) = zip::DateTime::from_date_and_time(
                            mtime.year() as u16,
                            mtime.month() as u8,
                            mtime.day() as u8,
                            mtime.hour() as u8,
                            mtime.minute() as u8,
                            mtime.second() as u8,
                        ) {
                            zip_options = zip_options.last_modified_time(mtime);
                        }
                    }

                    if metadata.is_dir() {
                        archive.add_directory(relative.to_string_lossy(), zip_options)?;
                        if let Some(bytes_archived) = &bytes_archived {
                            bytes_archived.fetch_add(metadata.len(), Ordering::SeqCst);
                        }
                    } else if metadata.is_file() {
                        let file = filesystem.open(&path)?;
                        let mut reader: Box<dyn Read + Send> = match &bytes_archived {
                            Some(bytes_archived) => Box::new(CountingReader::new_with_bytes_read(
                                file,
                                Arc::clone(bytes_archived),
                            )),
                            None => Box::new(file),
                        };

                        archive.start_file(relative.to_string_lossy(), zip_options)?;
                        crate::io::copy_shared(&mut read_buffer, &mut reader, &mut archive)?;
                    } else if let Ok(link_target) = filesystem.read_link_contents(&path) {
                        archive.add_symlink(
                            relative.to_string_lossy(),
                            link_target.to_string_lossy(),
                            zip_options,
                        )?;
                        if let Some(bytes_archived) = &bytes_archived {
                            bytes_archived.fetch_add(source_metadata.len(), Ordering::SeqCst);
                        }
                    }
                }
            } else if source_metadata.is_file() {
                let file = filesystem.open(&source)?;
                let mut reader: Box<dyn Read + Send> = match &bytes_archived {
                    Some(bytes_archived) => Box::new(CountingReader::new_with_bytes_read(
                        file,
                        Arc::clone(bytes_archived),
                    )),
                    None => Box::new(file),
                };

                archive.start_file(relative.to_string_lossy(), zip_options)?;
                crate::io::copy_shared(&mut read_buffer, &mut reader, &mut archive)?;
            } else if let Ok(link_target) = filesystem.read_link_contents(&source) {
                archive.add_symlink(
                    relative.to_string_lossy(),
                    link_target.to_string_lossy(),
                    zip_options,
                )?;
                if let Some(bytes_archived) = &bytes_archived {
                    bytes_archived.fetch_add(source_metadata.len(), Ordering::SeqCst);
                }
            }
        }

        let mut inner = archive.finish()?;
        inner.flush()?;

        Ok(())
    })
    .await??;

    Ok(())
}

pub async fn create_zip_streaming(
    filesystem: crate::server::filesystem::cap::CapFilesystem,
    destination: impl Write + Send + 'static,
    base: &Path,
    sources: Vec<PathBuf>,
    bytes_archived: Option<Arc<AtomicU64>>,
    ignored: Vec<ignore::gitignore::Gitignore>,
    options: CreateZipOptions,
) -> Result<(), anyhow::Error> {
    let base = filesystem.relative_path(base);
    let (_guard, listener) = AbortGuard::new();

    tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        let writer = AbortWriter::new(destination, listener);
        let mut archive = zip::ZipWriter::new_stream(writer);

        let mut read_buffer = vec![0; crate::BUFFER_SIZE];
        for source in sources {
            let relative = source;
            let source = base.join(&relative);

            let source_metadata = match filesystem.symlink_metadata(&source) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if ignored
                .iter()
                .any(|i| i.matched(&source, source_metadata.is_dir()).is_ignore())
            {
                continue;
            }

            let mut zip_options: zip::write::FileOptions<'_, ()> =
                zip::write::FileOptions::default()
                    .compression_level(Some(options.compression_level.to_deflate_level() as i64))
                    .unix_permissions(source_metadata.permissions().mode())
                    .large_file(true);

            if let Ok(mtime) = source_metadata.modified() {
                let mtime: chrono::DateTime<chrono::Utc> = chrono::DateTime::from(mtime.into_std());

                if let Ok(mtime) = zip::DateTime::from_date_and_time(
                    mtime.year() as u16,
                    mtime.month() as u8,
                    mtime.day() as u8,
                    mtime.hour() as u8,
                    mtime.minute() as u8,
                    mtime.second() as u8,
                ) {
                    zip_options = zip_options.last_modified_time(mtime);
                }
            }

            if source_metadata.is_dir() {
                archive.add_directory(relative.to_string_lossy(), zip_options)?;
                if let Some(bytes_archived) = &bytes_archived {
                    bytes_archived.fetch_add(source_metadata.len(), Ordering::SeqCst);
                }

                let mut walker = filesystem.walk_dir(source)?.with_ignored(&ignored);
                while let Some(Ok((_, path))) = walker.next_entry() {
                    let relative = match path.strip_prefix(&base) {
                        Ok(path) => path,
                        Err(_) => continue,
                    };

                    let metadata = match filesystem.symlink_metadata(&path) {
                        Ok(metadata) => metadata,
                        Err(_) => continue,
                    };

                    let mut zip_options: zip::write::FileOptions<'_, ()> =
                        zip::write::FileOptions::default()
                            .compression_level(Some(
                                options.compression_level.to_deflate_level() as i64
                            ))
                            .unix_permissions(metadata.permissions().mode())
                            .large_file(true);

                    if let Ok(mtime) = metadata.modified() {
                        let mtime: chrono::DateTime<chrono::Utc> =
                            chrono::DateTime::from(mtime.into_std());

                        if let Ok(mtime) = zip::DateTime::from_date_and_time(
                            mtime.year() as u16,
                            mtime.month() as u8,
                            mtime.day() as u8,
                            mtime.hour() as u8,
                            mtime.minute() as u8,
                            mtime.second() as u8,
                        ) {
                            zip_options = zip_options.last_modified_time(mtime);
                        }
                    }

                    if metadata.is_dir() {
                        archive.add_directory(relative.to_string_lossy(), zip_options)?;
                        if let Some(bytes_archived) = &bytes_archived {
                            bytes_archived.fetch_add(metadata.len(), Ordering::SeqCst);
                        }
                    } else if metadata.is_file() {
                        let file = filesystem.open(&path)?;
                        let mut reader: Box<dyn Read + Send> = match &bytes_archived {
                            Some(bytes_archived) => Box::new(CountingReader::new_with_bytes_read(
                                file,
                                Arc::clone(bytes_archived),
                            )),
                            None => Box::new(file),
                        };

                        archive.start_file(relative.to_string_lossy(), zip_options)?;
                        crate::io::copy_shared(&mut read_buffer, &mut reader, &mut archive)?;
                    } else if let Ok(link_target) = filesystem.read_link_contents(&path) {
                        archive.add_symlink(
                            relative.to_string_lossy(),
                            link_target.to_string_lossy(),
                            zip_options,
                        )?;
                        if let Some(bytes_archived) = &bytes_archived {
                            bytes_archived.fetch_add(source_metadata.len(), Ordering::SeqCst);
                        }
                    }
                }
            } else if source_metadata.is_file() {
                let file = filesystem.open(&source)?;
                let mut reader: Box<dyn Read + Send> = match &bytes_archived {
                    Some(bytes_archived) => Box::new(CountingReader::new_with_bytes_read(
                        file,
                        Arc::clone(bytes_archived),
                    )),
                    None => Box::new(file),
                };

                archive.start_file(relative.to_string_lossy(), zip_options)?;
                crate::io::copy_shared(&mut read_buffer, &mut reader, &mut archive)?;
            } else if let Ok(link_target) = filesystem.read_link_contents(&source) {
                archive.add_symlink(
                    relative.to_string_lossy(),
                    link_target.to_string_lossy(),
                    zip_options,
                )?;
                if let Some(bytes_archived) = &bytes_archived {
                    bytes_archived.fetch_add(source_metadata.len(), Ordering::SeqCst);
                }
            }
        }

        let mut inner = archive.finish()?;
        inner.flush()?;

        Ok(())
    })
    .await??;

    Ok(())
}
