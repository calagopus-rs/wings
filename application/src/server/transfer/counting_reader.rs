use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};
use tokio::io::AsyncRead;

pub struct CountingReader<R: std::io::Read> {
    inner: R,
    pub bytes_read: Arc<AtomicU64>,
}

impl<R: std::io::Read> CountingReader<R> {
    pub fn new_with_bytes_read(inner: R, bytes_read: Arc<AtomicU64>) -> Self {
        Self { inner, bytes_read }
    }
}

impl<R: std::io::Read> std::io::Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let bytes_read = self.inner.read(buf)?;
        self.bytes_read
            .fetch_add(bytes_read as u64, Ordering::Relaxed);

        Ok(bytes_read)
    }
}

pub struct AsyncCountingReader<R: AsyncRead + Unpin> {
    inner: R,
    pub bytes_read: Arc<AtomicU64>,
}

impl<R: AsyncRead + Unpin> AsyncCountingReader<R> {
    pub fn new_with_bytes_read(inner: R, bytes_read: Arc<AtomicU64>) -> Self {
        Self { inner, bytes_read }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for AsyncCountingReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let filled_before = buf.filled().len();

        let poll_result = Pin::new(&mut self.inner).poll_read(cx, buf);

        if let Poll::Ready(Ok(())) = &poll_result {
            let filled_after = buf.filled().len();
            let bytes_read = filled_after - filled_before;

            self.bytes_read
                .fetch_add(bytes_read as u64, Ordering::Relaxed);
        }

        poll_result
    }
}
