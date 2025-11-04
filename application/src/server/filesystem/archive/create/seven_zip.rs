use crate::io::{
    abort::{AbortGuard, AbortWriter},
    compression::CompressionLevel,
    counting_reader::CountingReader,
};
use sevenz_rust2::{
    EncoderConfiguration, EncoderMethod, NtTime,
    encoder_options::{EncoderOptions, Lzma2Options},
};
use std::{
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub struct Create7zOptions {
    pub compression_level: CompressionLevel,
    pub threads: usize,
}

pub async fn create_7z<W: Write + Seek + Send + 'static>(
    filesystem: crate::server::filesystem::cap::CapFilesystem,
    destination: W,
    base: &Path,
    sources: Vec<PathBuf>,
    bytes_archived: Option<Arc<AtomicU64>>,
    ignored: Vec<ignore::gitignore::Gitignore>,
    options: Create7zOptions,
) -> Result<W, anyhow::Error> {
    let base = filesystem.relative_path(base);
    let (_guard, listener) = AbortGuard::new();

    tokio::task::spawn_blocking(move || {
        let writer = AbortWriter::new(destination, listener);
        let mut archive = sevenz_rust2::ArchiveWriter::new(writer)?;

        archive.set_content_methods(vec![
            EncoderConfiguration::new(EncoderMethod::LZMA2).with_options(EncoderOptions::Lzma2(
                Lzma2Options::from_level_mt(
                    options.compression_level.to_lzma2_level(),
                    options.threads as u32,
                    16 * 1024,
                ),
            )),
        ]);

        let mut directory_entries = chunked_vec::ChunkedVec::new();

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

            let mtime = source_metadata
                .modified()
                .map_or(None, |mtime| NtTime::try_from(mtime.into_std()).ok());

            if source_metadata.is_dir() {
                directory_entries.push((relative, mtime));
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

                    let mtime = source_metadata
                        .modified()
                        .map_or(None, |mtime| NtTime::try_from(mtime.into_std()).ok());

                    if metadata.is_dir() {
                        directory_entries.push((relative.to_path_buf(), mtime));
                        if let Some(bytes_archived) = &bytes_archived {
                            bytes_archived.fetch_add(metadata.len(), Ordering::SeqCst);
                        }
                    } else if metadata.is_file() {
                        let file = filesystem.open(&path)?;
                        let reader: Box<dyn Read + Send> = match &bytes_archived {
                            Some(bytes_archived) => Box::new(CountingReader::new_with_bytes_read(
                                file,
                                Arc::clone(bytes_archived),
                            )),
                            None => Box::new(file),
                        };

                        let mut entry =
                            sevenz_rust2::ArchiveEntry::new_file(&relative.to_string_lossy());
                        if let Some(mtime) = mtime {
                            entry.has_last_modified_date = true;
                            entry.last_modified_date = mtime;
                        }
                        entry.size = metadata.len();

                        archive.push_archive_entry(entry, Some(reader))?;
                    }
                }
            } else if source_metadata.is_file() {
                let file = filesystem.open(&source)?;
                let reader: Box<dyn Read + Send> = match &bytes_archived {
                    Some(bytes_archived) => Box::new(CountingReader::new_with_bytes_read(
                        file,
                        Arc::clone(bytes_archived),
                    )),
                    None => Box::new(file),
                };

                let mut entry = sevenz_rust2::ArchiveEntry::new_file(&relative.to_string_lossy());
                if let Some(mtime) = mtime {
                    entry.has_last_modified_date = true;
                    entry.last_modified_date = mtime;
                }
                entry.size = source_metadata.len();

                archive.push_archive_entry(entry, Some(reader))?;
            }
        }

        for (source_path, mtime) in directory_entries {
            let mut entry =
                sevenz_rust2::ArchiveEntry::new_directory(&source_path.to_string_lossy());
            if let Some(mtime) = mtime {
                entry.has_last_modified_date = true;
                entry.last_modified_date = mtime;
            }

            archive.push_archive_entry(entry, None::<&[u8]>)?;
        }

        let mut inner = archive.finish()?.into_inner();
        inner.flush()?;

        Ok(inner)
    })
    .await?
}
