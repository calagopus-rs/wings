use futures::FutureExt;
use std::{
    io::{Seek, SeekFrom, Write},
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::io::{AsyncSeek, AsyncWrite};

pub struct LimitedWriter<W: Write> {
    inner: W,
    bytes_per_second: u64,
    last_write_time: Instant,
    bytes_written_since_last_check: u64,
}

impl<W: Write> LimitedWriter<W> {
    pub fn new_with_bytes_per_second(inner: W, bytes_per_second: u64) -> Self {
        Self {
            inner,
            bytes_per_second,
            last_write_time: Instant::now(),
            bytes_written_since_last_check: 0,
        }
    }
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if crate::unlikely(buf.is_empty()) {
            return Ok(0);
        }

        if self.bytes_per_second == 0 {
            return self.inner.write(buf);
        }

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_write_time).as_secs_f64();

        let bytes_allowed = (elapsed * self.bytes_per_second as f64) as u64;

        if self.bytes_written_since_last_check >= bytes_allowed {
            let time_since_last_write = now.duration_since(self.last_write_time);
            if time_since_last_write < Duration::from_secs(1) {
                let bytes_over = self.bytes_written_since_last_check - bytes_allowed;
                let sleep_secs = bytes_over as f64 / self.bytes_per_second as f64;
                let sleep_duration = Duration::from_secs_f64(sleep_secs);

                let max_sleep = Duration::from_secs(1) - time_since_last_write;
                let actual_sleep = sleep_duration.min(max_sleep);

                if !actual_sleep.is_zero() {
                    std::thread::sleep(actual_sleep);
                }
            }
        }

        let bytes_written = self.inner.write(buf)?;
        self.bytes_written_since_last_check += bytes_written as u64;

        if now.duration_since(self.last_write_time) >= Duration::from_secs(1) {
            self.last_write_time = now;
            self.bytes_written_since_last_check = 0;
        }

        Ok(bytes_written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write + Seek> Seek for LimitedWriter<W> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

pub struct AsyncLimitedWriter<W: AsyncWrite + Unpin> {
    inner: W,
    bytes_per_second: u64,
    last_write_time: Instant,
    bytes_written_since_last_check: u64,
}

impl<W: AsyncWrite + Unpin> AsyncLimitedWriter<W> {
    pub fn new_with_bytes_per_second(inner: W, bytes_per_second: u64) -> Self {
        Self {
            inner,
            bytes_per_second,
            last_write_time: Instant::now(),
            bytes_written_since_last_check: 0,
        }
    }

    fn calculate_delay(&self) -> Option<Duration> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_write_time).as_secs_f64();

        let bytes_allowed = (elapsed * self.bytes_per_second as f64) as u64;

        if self.bytes_written_since_last_check > bytes_allowed {
            let time_since_last_write = now.duration_since(self.last_write_time);
            if time_since_last_write < Duration::from_secs(1) {
                let bytes_over = self.bytes_written_since_last_check - bytes_allowed;
                let sleep_secs = bytes_over as f64 / self.bytes_per_second as f64;
                let sleep_duration = Duration::from_secs_f64(sleep_secs);

                let max_sleep = Duration::from_secs(1) - time_since_last_write;
                let actual_sleep = sleep_duration.min(max_sleep);

                if !actual_sleep.is_zero() {
                    return Some(actual_sleep);
                }
            }
        }

        None
    }

    fn check_reset_counters(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_write_time) >= Duration::from_secs(1) {
            self.last_write_time = now;
            self.bytes_written_since_last_check = 0;
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for AsyncLimitedWriter<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if crate::unlikely(buf.is_empty()) {
            return Poll::Ready(Ok(0));
        }

        if self.bytes_per_second == 0 {
            return Pin::new(&mut self.get_mut().inner).poll_write(cx, buf);
        }

        let this = self.get_mut();

        if let Some(delay_duration) = this.calculate_delay()
            && !delay_duration.is_zero()
        {
            let mut delay_future = Box::pin(tokio::time::sleep(delay_duration));

            match delay_future.poll_unpin(cx) {
                Poll::Ready(_) => {}
                Poll::Pending => return Poll::Pending,
            }
        }

        match Pin::new(&mut this.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(bytes_written)) => {
                this.bytes_written_since_last_check += bytes_written as u64;
                this.check_reset_counters();

                Poll::Ready(Ok(bytes_written))
            }
            other => other,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl<W: AsyncWrite + AsyncSeek + Unpin> AsyncSeek for AsyncLimitedWriter<W> {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        Pin::new(&mut self.get_mut().inner).start_seek(position)
    }

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        Pin::new(&mut self.get_mut().inner).poll_complete(cx)
    }
}
