use cap_std::fs::Permissions;
use cap_std::time::SystemTime;
use std::future::Future;
use std::path::Path;
use std::{
    io::{BufWriter, Seek, SeekFrom, Write},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncSeek, AsyncWrite};

const ALLOCATION_THRESHOLD: i64 = 1024 * 1024;

pub struct FileSystemWriter {
    server: crate::server::Server,
    parent: Vec<String>,
    writer: Option<BufWriter<std::fs::File>>,
    ignorant: bool,
    accumulated_bytes: i64,
    modified: Option<SystemTime>,
    current_position: u64,
    highest_position: u64,
}

impl FileSystemWriter {
    pub fn new(
        server: crate::server::Server,
        destination: &Path,
        permissions: Option<Permissions>,
        modified: Option<SystemTime>,
    ) -> Result<Self, anyhow::Error> {
        let parent_path = destination.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Destination has no parent",
            )
        })?;

        let parent = server
            .filesystem
            .path_to_components(&server.filesystem.relative_path(parent_path));
        let file = server.filesystem.create(destination)?;

        if let Some(permissions) = permissions {
            server
                .filesystem
                .set_permissions(destination, permissions)?;
        }

        Ok(Self {
            server,
            parent,
            writer: Some(BufWriter::with_capacity(crate::BUFFER_SIZE, file)),
            ignorant: false,
            accumulated_bytes: 0,
            modified,
            current_position: 0,
            highest_position: 0,
        })
    }

    /// Skip Disk Limit Checks
    pub fn ignorant(mut self) -> Self {
        self.ignorant = true;
        self
    }

    fn allocate_accumulated(&mut self) -> std::io::Result<()> {
        if self.accumulated_bytes > 0 {
            if !self.server.filesystem.allocate_in_path_slice(
                &self.parent,
                self.accumulated_bytes,
                self.ignorant,
            ) {
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
        let written = if let Some(writer) = self.writer.as_mut() {
            writer.write(buf)?
        } else {
            return Err(std::io::Error::other("Writer is not available"));
        };

        self.current_position += written as u64;

        if crate::unlikely(self.current_position > self.highest_position) {
            let additional_space = (self.current_position - self.highest_position) as i64;
            self.accumulated_bytes += additional_space;
            self.highest_position = self.current_position;
        }

        if crate::unlikely(self.accumulated_bytes >= ALLOCATION_THRESHOLD) {
            self.allocate_accumulated()?;
        }

        Ok(written)
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        self.allocate_accumulated()?;

        if let Some(writer) = self.writer.as_mut() {
            writer.flush()
        } else {
            Err(std::io::Error::other("Writer is not available"))
        }
    }
}

impl Seek for FileSystemWriter {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.allocate_accumulated()?;

        let new_pos = if let Some(writer) = self.writer.as_mut() {
            writer.seek(pos)?
        } else {
            return Err(std::io::Error::other("Writer is not available"));
        };

        self.current_position = new_pos;

        Ok(new_pos)
    }
}

impl Drop for FileSystemWriter {
    fn drop(&mut self) {
        if let Some(modified) = self.modified
            && let Some(writer) = self.writer.take()
            && let Ok(file) = writer.into_inner()
        {
            file.set_modified(modified.into_std()).ok();
        }
    }
}

pub struct AsyncFileSystemWriter {
    server: crate::server::Server,
    parent: Vec<String>,
    writer: Option<tokio::io::BufWriter<tokio::fs::File>>,
    ignorant: bool,
    accumulated_bytes: i64,
    modified: Option<SystemTime>,
    allocation_in_progress: Option<Pin<Box<dyn Future<Output = bool> + Send>>>,
    current_position: u64,
    highest_position: u64,
}

impl AsyncFileSystemWriter {
    pub async fn new(
        server: crate::server::Server,
        destination: &Path,
        permissions: Option<Permissions>,
        modified: Option<SystemTime>,
    ) -> Result<Self, anyhow::Error> {
        let parent_path = destination.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Destination has no parent",
            )
        })?;

        let parent = server
            .filesystem
            .path_to_components(&server.filesystem.relative_path(parent_path));
        let file = server.filesystem.async_create(destination).await?;

        if let Some(permissions) = permissions {
            server
                .filesystem
                .async_set_permissions(destination, permissions)
                .await?;
        }

        server.filesystem.chown_path(destination).await?;

        Ok(Self {
            server,
            parent,
            writer: Some(tokio::io::BufWriter::with_capacity(
                crate::BUFFER_SIZE,
                file,
            )),
            ignorant: false,
            accumulated_bytes: 0,
            modified,
            allocation_in_progress: None,
            current_position: 0,
            highest_position: 0,
        })
    }

    /// Skip Disk Limit Checks
    pub fn ignorant(mut self) -> Self {
        self.ignorant = true;
        self
    }

    fn start_allocation(&mut self) {
        if crate::likely(self.accumulated_bytes > 0 && self.allocation_in_progress.is_none()) {
            let server = self.server.clone();
            let parent = self.parent.clone();
            let bytes = self.accumulated_bytes;
            let ignorant = self.ignorant;

            self.allocation_in_progress = Some(Box::pin(async move {
                server
                    .filesystem
                    .async_allocate_in_path_slice(&parent, bytes, ignorant)
                    .await
            }));

            self.accumulated_bytes = 0;
        }
    }

    fn poll_allocation(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if let Some(fut) = &mut self.allocation_in_progress {
            match fut.as_mut().poll(cx) {
                Poll::Ready(true) => {
                    self.allocation_in_progress = None;
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(false) => {
                    self.allocation_in_progress = None;
                    Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::StorageFull,
                        "Failed to allocate space",
                    )))
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Ready(Ok(()))
        }
    }
}

impl AsyncWrite for AsyncFileSystemWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.poll_allocation(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }

        let result = Pin::new(self.writer.as_mut().unwrap()).poll_write(cx, buf);

        if let Poll::Ready(Ok(written)) = &result {
            let written = *written as u64;
            self.current_position += written;

            if self.current_position > self.highest_position {
                let additional_space = (self.current_position - self.highest_position) as i64;
                self.accumulated_bytes += additional_space;
                self.highest_position = self.current_position;

                if self.accumulated_bytes >= ALLOCATION_THRESHOLD {
                    self.start_allocation();

                    match self.poll_allocation(cx) {
                        Poll::Ready(Ok(())) => {}
                        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        Poll::Pending => return Poll::Pending,
                    }
                }
            }
        }

        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.poll_allocation(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }

        if crate::likely(self.accumulated_bytes > 0) {
            self.start_allocation();

            match self.poll_allocation(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }

        Pin::new(self.writer.as_mut().unwrap()).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.poll_allocation(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }

        if crate::likely(self.accumulated_bytes > 0) {
            self.start_allocation();

            match self.poll_allocation(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }

        Pin::new(self.writer.as_mut().unwrap()).poll_shutdown(cx)
    }
}

impl AsyncSeek for AsyncFileSystemWriter {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        if crate::unlikely(self.accumulated_bytes > 0) {
            self.start_allocation();
        }

        Pin::new(self.writer.as_mut().unwrap()).start_seek(position)
    }

    fn poll_complete(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        match self.poll_allocation(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }

        let result = Pin::new(self.writer.as_mut().unwrap()).poll_complete(cx);

        if let Poll::Ready(Ok(new_pos)) = &result {
            self.current_position = *new_pos;
        }

        result
    }
}

impl Drop for AsyncFileSystemWriter {
    fn drop(&mut self) {
        if self.accumulated_bytes > 0 {
            let server = self.server.clone();
            let parent = self.parent.clone();
            let bytes = self.accumulated_bytes;
            let ignorant = self.ignorant;

            if bytes > 0 {
                tokio::spawn(async move {
                    server
                        .filesystem
                        .async_allocate_in_path_slice(&parent, bytes, ignorant)
                        .await;
                });
            }
        }

        if let Some(modified) = self.modified
            && let Some(writer) = self.writer.take()
        {
            tokio::spawn(async move {
                let file = writer.into_inner().into_std().await;

                tokio::task::spawn_blocking(move || file.set_modified(modified.into_std()));
            });
        }
    }
}
