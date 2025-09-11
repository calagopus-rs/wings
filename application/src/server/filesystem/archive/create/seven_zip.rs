use crate::io::{
    abort::{AbortGuard, AbortWriter},
    counting_reader::CountingReader,
};
use std::{
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub struct Create7zOptions {}

pub async fn create_7z(
    filesystem: crate::server::filesystem::cap::CapFilesystem,
    destination: impl Write + Seek + Send + 'static,
    base: &Path,
    sources: Vec<PathBuf>,
    bytes_archived: Option<Arc<AtomicU64>>,
    ignored: Vec<ignore::gitignore::Gitignore>,
    _options: Create7zOptions,
) -> Result<(), anyhow::Error> {
    let base = filesystem.relative_path(base);
    let (_guard, listener) = AbortGuard::new();

    tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        let writer = AbortWriter::new(destination, listener);
        let mut archive = sevenz_rust2::ArchiveWriter::new(writer)?;

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

            let mut entry = sevenz_rust2::ArchiveEntry::new();
            entry.name = relative.to_string_lossy().to_string();
            entry.is_directory = source_metadata.is_dir();
            if let Ok(mtime) = source_metadata.modified()
                && let Ok(mtime) = mtime.into_std().try_into()
            {
                entry.last_modified_date = mtime;
            }

            if source_metadata.is_dir() {
                archive.push_archive_entry(entry, None::<Box<dyn Read + Send>>)?;
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

                    let mut entry = sevenz_rust2::ArchiveEntry::new();
                    entry.name = relative.to_string_lossy().to_string();
                    entry.is_directory = metadata.is_dir();
                    if let Ok(mtime) = metadata.modified()
                        && let Ok(mtime) = mtime.into_std().try_into()
                    {
                        entry.last_modified_date = mtime;
                    }

                    if metadata.is_dir() {
                        archive.push_archive_entry(entry, None::<Box<dyn Read + Send>>)?;
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

                entry.size = source_metadata.len();

                archive.push_archive_entry(entry, Some(reader))?;
            }
        }

        let mut inner = archive.finish()?;
        inner.flush()?;

        Ok(())
    })
    .await??;

    Ok(())
}
