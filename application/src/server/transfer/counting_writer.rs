use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub struct CountingWriter<R: std::io::Write> {
    inner: R,
    pub bytes_written: Arc<AtomicU64>,
}

impl<R: std::io::Write> CountingWriter<R> {
    pub fn new_with_bytes_written(inner: R, bytes_written: Arc<AtomicU64>) -> Self {
        Self {
            inner,
            bytes_written,
        }
    }
}

impl<R: std::io::Write> std::io::Write for CountingWriter<R> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let bytes_written = self.inner.write(buf)?;
        self.bytes_written
            .fetch_add(bytes_written as u64, Ordering::Relaxed);
        Ok(bytes_written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
