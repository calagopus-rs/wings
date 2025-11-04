use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub struct CountingWriter<W: std::io::Write> {
    inner: W,
    pub bytes_written: Arc<AtomicU64>,
}

impl<W: std::io::Write> CountingWriter<W> {
    pub fn new_with_bytes_written(inner: W, bytes_written: Arc<AtomicU64>) -> Self {
        Self {
            inner,
            bytes_written,
        }
    }

    #[inline]
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: std::io::Write> std::io::Write for CountingWriter<W> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let bytes_written = self.inner.write(buf)?;
        self.bytes_written
            .fetch_add(bytes_written as u64, Ordering::Relaxed);
        Ok(bytes_written)
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
