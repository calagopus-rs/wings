use std::{
    io::{Read, Seek, SeekFrom},
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

pub struct LimitedReader<R: Read> {
    inner: R,
    bytes_per_second: u64,
    last_read_time: Instant,
    bytes_read_since_last_check: u64,
}

impl<R: Read> LimitedReader<R> {
    pub fn new_with_bytes_per_second(inner: R, bytes_per_second: u64) -> Self {
        Self {
            inner,
            bytes_per_second,
            last_read_time: Instant::now(),
            bytes_read_since_last_check: 0,
        }
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if crate::unlikely(buf.is_empty()) {
            return Ok(0);
        }

        if self.bytes_per_second == 0 {
            return self.inner.read(buf);
        }

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_read_time).as_secs_f64();

        let bytes_allowed = (elapsed * self.bytes_per_second as f64) as u64;

        if self.bytes_read_since_last_check >= bytes_allowed {
            let time_since_last_read = now.duration_since(self.last_read_time);
            if time_since_last_read < Duration::from_secs(1) {
                let bytes_over = self.bytes_read_since_last_check - bytes_allowed;
                let sleep_secs = bytes_over as f64 / self.bytes_per_second as f64;
                let sleep_duration = Duration::from_secs_f64(sleep_secs);

                let max_sleep = Duration::from_secs(1) - time_since_last_read;
                let actual_sleep = sleep_duration.min(max_sleep);

                if !actual_sleep.is_zero() {
                    std::thread::sleep(actual_sleep);
                }
            }
        }

        let bytes_read = self.inner.read(buf)?;
        self.bytes_read_since_last_check += bytes_read as u64;

        if now.duration_since(self.last_read_time) >= Duration::from_secs(1) {
            self.last_read_time = now;
            self.bytes_read_since_last_check = 0;
        }

        Ok(bytes_read)
    }
}

impl<R: Read + Seek> Seek for LimitedReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

pub struct AsyncLimitedReader<R: AsyncRead + Unpin> {
    inner: R,
    bytes_per_second: u64,
    last_read_time: Instant,
    bytes_read_since_last_check: u64,
    delay_future: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl<R: AsyncRead + Unpin> AsyncLimitedReader<R> {
    pub fn new_with_bytes_per_second(inner: R, bytes_per_second: u64) -> Self {
        Self {
            inner,
            bytes_per_second,
            last_read_time: Instant::now(),
            bytes_read_since_last_check: 0,
            delay_future: None,
        }
    }

    fn calculate_delay(&self) -> Option<Duration> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_read_time).as_secs_f64();

        let bytes_allowed = (elapsed * self.bytes_per_second as f64) as u64;

        if self.bytes_read_since_last_check > bytes_allowed {
            let time_since_last_read = now.duration_since(self.last_read_time);
            if time_since_last_read < Duration::from_secs(1) {
                let bytes_over = self.bytes_read_since_last_check - bytes_allowed;
                let sleep_secs = bytes_over as f64 / self.bytes_per_second as f64;
                let sleep_duration = Duration::from_secs_f64(sleep_secs);

                let max_sleep = Duration::from_secs(1) - time_since_last_read;
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
        if now.duration_since(self.last_read_time) >= Duration::from_secs(1) {
            self.last_read_time = now;
            self.bytes_read_since_last_check = 0;
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for AsyncLimitedReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.as_mut().get_mut();

        if crate::unlikely(buf.remaining() == 0) {
            return Poll::Ready(Ok(()));
        }

        if this.bytes_per_second == 0 {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        }

        if let Some(delay) = &mut this.delay_future {
            match delay.as_mut().poll(cx) {
                Poll::Ready(_) => {
                    this.delay_future = None;
                }
                Poll::Pending => return Poll::Pending,
            }
        }

        if this.delay_future.is_none()
            && let Some(delay_duration) = this.calculate_delay()
        {
            let delay = Box::pin(tokio::time::sleep(delay_duration));
            this.delay_future = Some(delay);
            return self.poll_read(cx, buf);
        }

        let filled_before = buf.filled().len();

        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let filled_after = buf.filled().len();
                let bytes_read = filled_after - filled_before;

                this.bytes_read_since_last_check += bytes_read as u64;
                this.check_reset_counters();

                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl<R: AsyncRead + AsyncSeek + Unpin> AsyncSeek for AsyncLimitedReader<R> {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        Pin::new(&mut self.get_mut().inner).start_seek(position)
    }

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        Pin::new(&mut self.get_mut().inner).poll_complete(cx)
    }
}
