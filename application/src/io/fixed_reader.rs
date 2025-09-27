use std::{
    io::Read,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};

pub struct FixedReader<R: Read> {
    inner: R,
    size: usize,
    bytes_read: usize,
}

impl<R: Read> FixedReader<R> {
    pub fn new_with_fixed_bytes(inner: R, size: usize) -> Self {
        Self {
            inner,
            size,
            bytes_read: 0,
        }
    }
}

impl<R: Read> Read for FixedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if crate::unlikely(self.bytes_read >= self.size) {
            return Ok(0);
        }

        let remaining = self.size - self.bytes_read;
        let to_read = std::cmp::min(buf.len(), remaining);

        let bytes_read = self.inner.read(&mut buf[..to_read])?;
        self.bytes_read += bytes_read;

        if crate::unlikely(bytes_read < to_read) {
            let zeros_needed = to_read - bytes_read;
            for i in 0..zeros_needed {
                buf[bytes_read + i] = 0;
            }
            self.bytes_read += zeros_needed;
            return Ok(to_read);
        }

        Ok(bytes_read)
    }
}

pub struct AsyncFixedReader<R: AsyncRead + Unpin> {
    inner: R,
    size: usize,
    bytes_read: usize,
}

impl<R: AsyncRead + Unpin> AsyncFixedReader<R> {
    pub fn new_with_fixed_bytes(inner: R, size: usize) -> Self {
        Self {
            inner,
            size,
            bytes_read: 0,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for AsyncFixedReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if crate::unlikely(self.bytes_read >= self.size) {
            return Poll::Ready(Ok(()));
        }

        let remaining = self.size - self.bytes_read;
        let to_read = std::cmp::min(buf.remaining(), remaining);

        let init = buf.initialize_unfilled_to(to_read);
        let mut temp_buf = ReadBuf::new(init);

        match Pin::new(&mut self.inner).poll_read(cx, &mut temp_buf) {
            Poll::Ready(Ok(())) => {
                let bytes_read = temp_buf.filled().len();
                buf.advance(bytes_read);
                self.bytes_read += bytes_read;

                if crate::unlikely(bytes_read < to_read) {
                    let unfilled = buf.initialize_unfilled_to(to_read - bytes_read);
                    for byte in unfilled {
                        *byte = 0;
                    }
                    buf.advance(to_read - bytes_read);
                    self.bytes_read += to_read - bytes_read;
                }

                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
