use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone)]
pub struct AbortListener(Arc<AtomicBool>);

impl AbortListener {
    #[cold]
    #[inline(always)]
    pub fn is_aborted(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

pub struct AbortGuard(Arc<AtomicBool>);

impl AbortGuard {
    #[inline]
    pub fn new() -> (Self, AbortListener) {
        let inner = Arc::new(AtomicBool::new(false));

        (Self(inner.clone()), AbortListener(inner))
    }
}

impl Drop for AbortGuard {
    #[inline]
    fn drop(&mut self) {
        if !self.0.load(Ordering::Relaxed) {
            tracing::debug!("dropping abort guard");
        }

        self.0.store(true, Ordering::Relaxed);
    }
}

pub struct AbortReader<R: Read> {
    inner: R,
    listener: AbortListener,
}

impl<R: Read> AbortReader<R> {
    pub fn new(inner: R, listener: AbortListener) -> Self {
        Self { inner, listener }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read + Clone> Clone for AbortReader<R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            listener: self.listener.clone(),
        }
    }
}

impl<R: Read> Read for AbortReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.listener.is_aborted() {
            return Err(std::io::Error::other("Operation aborted"));
        }

        self.inner.read(buf)
    }
}

impl<R: Read + Seek> Seek for AbortReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        if self.listener.is_aborted() {
            return Err(std::io::Error::other("Operation aborted"));
        }

        self.inner.seek(pos)
    }
}

pub struct AbortWriter<W: Write> {
    inner: W,
    listener: AbortListener,
}

impl<W: Write> AbortWriter<W> {
    pub fn new(inner: W, listener: AbortListener) -> Self {
        Self { inner, listener }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write + Clone> Clone for AbortWriter<W> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            listener: self.listener.clone(),
        }
    }
}

impl<W: Write> Write for AbortWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.listener.is_aborted() {
            return Err(std::io::Error::other("Operation aborted"));
        }

        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.listener.is_aborted() {
            return Err(std::io::Error::other("Operation aborted"));
        }

        self.inner.flush()
    }
}

impl<W: Write + Seek> Seek for AbortWriter<W> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        if self.listener.is_aborted() {
            return Err(std::io::Error::other("Operation aborted"));
        }

        self.inner.seek(pos)
    }
}
