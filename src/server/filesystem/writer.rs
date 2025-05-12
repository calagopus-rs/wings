use std::{
    fs::{File, Permissions},
    io::{BufWriter, Seek, SeekFrom, Write},
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::io::{AsyncSeek, AsyncWrite};

const ALLOCATION_THRESHOLD: i64 = 1_024_000; // 1 MB

pub struct FileSystemWriter {
    filesystem: Arc<super::Filesystem>,
    parent: Vec<String>,
    writer: BufWriter<File>,
    accumulated_bytes: i64,
}

impl FileSystemWriter {
    pub fn new(
        filesystem: Arc<super::Filesystem>,
        destination: PathBuf,
        permissions: Option<Permissions>,
    ) -> std::io::Result<Self> {
        let parent = filesystem.path_to_components(&destination.parent().unwrap().canonicalize()?);
        let file = File::create(&destination)?;

        if let Some(permissions) = permissions {
            std::fs::set_permissions(&destination, permissions)?;
        }

        Ok(Self {
            filesystem,
            parent,
            writer: BufWriter::with_capacity(ALLOCATION_THRESHOLD as usize, file),
            accumulated_bytes: 0,
        })
    }

    fn allocate_accumulated(&mut self) -> std::io::Result<()> {
        if self.accumulated_bytes > 0 {
            if !self
                .filesystem
                .allocate_in_path_raw(&self.parent, self.accumulated_bytes)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::StorageFull,
                    "Failed to allocate space",
                ));
            }
            self.accumulated_bytes = 0;
        }
        Ok(())
    }
}

impl Write for FileSystemWriter {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let size = buf.len() as i64;

        self.accumulated_bytes += size;

        if self.accumulated_bytes >= ALLOCATION_THRESHOLD {
            self.allocate_accumulated()?;
        }

        self.writer.write(buf)
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        self.allocate_accumulated()?;
        self.writer.flush()
    }
}

impl Seek for FileSystemWriter {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.allocate_accumulated()?;
        self.writer.seek(pos)
    }
}

pub struct AsyncFileSystemWriter {
    filesystem: Arc<super::Filesystem>,
    parent: Vec<String>,
    writer: tokio::io::BufWriter<tokio::fs::File>,
    accumulated_bytes: i64,
}

impl AsyncFileSystemWriter {
    pub async fn new(
        filesystem: Arc<super::Filesystem>,
        destination: PathBuf,
        permissions: Option<Permissions>,
    ) -> std::io::Result<Self> {
        let parent_path = destination.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Destination has no parent",
            )
        })?;

        let canonicalized = tokio::fs::canonicalize(parent_path).await?;
        let parent = filesystem.path_to_components(&canonicalized);
        let file = tokio::fs::File::create(&destination).await?;

        if let Some(permissions) = permissions {
            tokio::fs::set_permissions(&destination, permissions).await?;
        }

        Ok(Self {
            filesystem,
            parent,
            writer: tokio::io::BufWriter::with_capacity(ALLOCATION_THRESHOLD as usize, file),
            accumulated_bytes: 0,
        })
    }

    fn allocate_accumulated(&mut self) -> std::io::Result<()> {
        if self.accumulated_bytes > 0 {
            if !self
                .filesystem
                .allocate_in_path_raw(&self.parent, self.accumulated_bytes)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::StorageFull,
                    "Failed to allocate space",
                ));
            }

            self.accumulated_bytes = 0;
        }
        Ok(())
    }
}

impl AsyncWrite for AsyncFileSystemWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let size = buf.len() as i64;

        self.accumulated_bytes += size;
        if self.accumulated_bytes >= ALLOCATION_THRESHOLD {
            if let Err(e) = self.allocate_accumulated() {
                return Poll::Ready(Err(e));
            }
        }

        Pin::new(&mut self.writer).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if let Err(e) = self.allocate_accumulated() {
            return Poll::Ready(Err(e));
        }

        Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if let Err(e) = self.allocate_accumulated() {
            return Poll::Ready(Err(e));
        }

        Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}

impl AsyncSeek for AsyncFileSystemWriter {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        self.allocate_accumulated()?;
        Pin::new(&mut self.writer).start_seek(position)
    }

    fn poll_complete(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        Pin::new(&mut self.writer).poll_complete(cx)
    }
}

impl Drop for AsyncFileSystemWriter {
    fn drop(&mut self) {
        if self.accumulated_bytes > 0 {
            let _ = self
                .filesystem
                .allocate_in_path_raw(&self.parent, self.accumulated_bytes);
        }
    }
}
