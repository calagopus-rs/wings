use crate::io::{
    abort::{AbortGuard, AbortWriter},
    compression::{CompressionLevel, CompressionType, writer::CompressionWriter},
    counting_reader::CountingReader,
    fixed_reader::FixedReader,
};
use cap_std::fs::PermissionsExt;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub struct CreateTarOptions {
    pub compression_type: CompressionType,
    pub compression_level: CompressionLevel,
    pub threads: usize,
}

pub async fn create_tar(
    filesystem: crate::server::filesystem::cap::CapFilesystem,
    destination: impl Write + Send + 'static,
    base: &Path,
    sources: Vec<PathBuf>,
    bytes_archived: Option<Arc<AtomicU64>>,
    ignored: Vec<ignore::gitignore::Gitignore>,
    options: CreateTarOptions,
) -> Result<(), anyhow::Error> {
    let base = filesystem.relative_path(base);
    let (_guard, listener) = AbortGuard::new();

    tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
        let writer = CompressionWriter::new(
            destination,
            options.compression_type,
            options.compression_level,
            options.threads,
        );
        let writer = AbortWriter::new(writer, listener);
        let mut archive = tar::Builder::new(writer);

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

            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(source_metadata.permissions().mode());
            header.set_mtime(
                source_metadata
                    .modified()
                    .map(|t| {
                        t.into_std()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                    })
                    .unwrap_or_default()
                    .as_secs(),
            );

            if source_metadata.is_dir() {
                header.set_entry_type(tar::EntryType::Directory);

                archive.append_data(&mut header, relative, std::io::empty())?;
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

                    let mut header = tar::Header::new_gnu();
                    header.set_size(0);
                    header.set_mode(metadata.permissions().mode());
                    header.set_mtime(
                        metadata
                            .modified()
                            .map(|t| {
                                t.into_std()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default()
                            .as_secs(),
                    );

                    if metadata.is_dir() {
                        header.set_entry_type(tar::EntryType::Directory);

                        archive.append_data(&mut header, relative, std::io::empty())?;
                        if let Some(bytes_archived) = &bytes_archived {
                            bytes_archived.fetch_add(metadata.len(), Ordering::SeqCst);
                        }
                    } else if metadata.is_file() {
                        let file = filesystem.open(&path)?;
                        let reader: Box<dyn Read> = match &bytes_archived {
                            Some(bytes_archived) => Box::new(CountingReader::new_with_bytes_read(
                                file,
                                Arc::clone(bytes_archived),
                            )),
                            None => Box::new(file),
                        };
                        let reader =
                            FixedReader::new_with_fixed_bytes(reader, metadata.len() as usize);

                        header.set_size(metadata.len());
                        header.set_entry_type(tar::EntryType::Regular);

                        archive.append_data(&mut header, relative, reader)?;
                    } else if let Ok(link_target) = filesystem.read_link_contents(&path) {
                        header.set_entry_type(tar::EntryType::Symlink);

                        if header.set_link_name(link_target).is_ok() {
                            archive.append_data(&mut header, relative, std::io::empty())?;
                            if let Some(bytes_archived) = &bytes_archived {
                                bytes_archived.fetch_add(source_metadata.len(), Ordering::SeqCst);
                            }
                        }
                    }
                }
            } else if source_metadata.is_file() {
                let file = filesystem.open(&source)?;
                let reader: Box<dyn Read> = match &bytes_archived {
                    Some(bytes_archived) => Box::new(CountingReader::new_with_bytes_read(
                        file,
                        Arc::clone(bytes_archived),
                    )),
                    None => Box::new(file),
                };
                let reader =
                    FixedReader::new_with_fixed_bytes(reader, source_metadata.len() as usize);

                header.set_size(source_metadata.len());
                header.set_entry_type(tar::EntryType::Regular);

                archive.append_data(&mut header, relative, reader)?;
            } else if let Ok(link_target) = filesystem.read_link_contents(&source) {
                header.set_entry_type(tar::EntryType::Symlink);

                if header.set_link_name(link_target).is_ok() {
                    archive.append_data(&mut header, relative, std::io::empty())?;
                    if let Some(bytes_archived) = &bytes_archived {
                        bytes_archived.fetch_add(source_metadata.len(), Ordering::SeqCst);
                    }
                }
            }
        }

        let inner = archive.into_inner()?;
        inner.into_inner().finish()?;

        Ok(())
    })
    .await??;

    Ok(())
}
