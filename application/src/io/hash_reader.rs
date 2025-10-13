use sha1::{Digest, digest::Output};
use std::{
    io::Read,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::{Mutex, OwnedMutexGuard},
};

pub struct HashReader<H: Digest, R: Read> {
    inner: R,
    hasher: H,
}

impl<H: Digest, R: Read> HashReader<H, R> {
    pub fn new_with_hasher(inner: R, hasher: H) -> Self {
        Self { inner, hasher }
    }

    pub fn finish(self) -> Output<H> {
        self.hasher.finalize()
    }
}

impl<H: Digest, R: Read> Read for HashReader<H, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let bytes_read = self.inner.read(buf)?;

        self.hasher.update(&buf[..bytes_read]);

        Ok(bytes_read)
    }
}

pub struct AsyncHashReader<H: Digest + Unpin, R: AsyncRead + Unpin> {
    inner: R,
    hasher: OwnedMutexGuard<H>,
}

impl<H: Digest + Unpin, R: AsyncRead + Unpin> AsyncHashReader<H, R> {
    pub async fn new_with_hasher(inner: R, hasher: Arc<Mutex<H>>) -> Self {
        Self {
            inner,
            hasher: hasher.lock_owned().await,
        }
    }
}

impl<H: Digest + Unpin, R: AsyncRead + Unpin> AsyncRead for AsyncHashReader<H, R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let filled_before = buf.filled().len();

        let poll_result = Pin::new(&mut self.inner).poll_read(cx, buf);

        if let Poll::Ready(Ok(())) = &poll_result {
            let filled_after = buf.filled().len();
            let newly_filled = &buf.filled()[filled_before..filled_after];

            self.hasher.update(newly_filled);
        }

        poll_result
    }
}
