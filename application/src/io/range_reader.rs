use futures::ready;
use std::{
    io::{self, Read, Seek, SeekFrom},
    ops::Bound,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncSeek, AsyncSeekExt};

pub struct RangeReader<R> {
    inner: R,
    start: u64,
    end: Option<u64>,
    pos: u64,
    len: u64,
}

impl<R: Read + Seek> RangeReader<R> {
    pub fn new(mut inner: R, range: (Bound<u64>, Bound<u64>), len: u64) -> io::Result<Self> {
        let start = match range.0 {
            Bound::Included(start) => start,
            Bound::Excluded(start) => start + 1,
            Bound::Unbounded => 0,
        };

        let end = match range.1 {
            Bound::Included(end) => Some(end.min(len - 1)),
            Bound::Excluded(end) => Some((end - 1).min(len - 1)),
            Bound::Unbounded => Some(len - 1),
        };

        if let Some(end) = end
            && (start > end || start >= len)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid range specified",
            ));
        }

        inner.seek(SeekFrom::Start(start))?;

        Ok(Self {
            inner,
            start,
            end,
            pos: start,
            len,
        })
    }

    pub fn len(&self) -> u64 {
        match self.end {
            Some(end) => end - self.start + 1,
            None => self.len - self.start,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<R: Read + Seek> Read for RangeReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(end) = self.end {
            if self.pos > end {
                return Ok(0);
            }

            let remaining = (end - self.pos + 1) as usize;
            let to_read = buf.len().min(remaining);

            let bytes_read = self.inner.read(&mut buf[..to_read])?;
            self.pos += bytes_read as u64;

            Ok(bytes_read)
        } else {
            let bytes_read = self.inner.read(buf)?;
            self.pos += bytes_read as u64;

            Ok(bytes_read)
        }
    }
}

pub struct AsyncRangeReader<R> {
    inner: R,
    start: u64,
    end: Option<u64>,
    pos: u64,
    len: u64,
}

impl<R: AsyncRead + AsyncSeek + Unpin> AsyncRangeReader<R> {
    pub async fn new(mut inner: R, range: (Bound<u64>, Bound<u64>), len: u64) -> io::Result<Self> {
        let start = match range.0 {
            Bound::Included(start) => start,
            Bound::Excluded(start) => start + 1,
            Bound::Unbounded => 0,
        };

        let end = match range.1 {
            Bound::Included(end) => Some(end.min(len - 1)),
            Bound::Excluded(end) => Some((end - 1).min(len - 1)),
            Bound::Unbounded => Some(len - 1),
        };

        if let Some(end) = end
            && (start > end || start >= len)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid range specified",
            ));
        }

        inner.seek(SeekFrom::Start(start)).await?;

        Ok(Self {
            inner,
            start,
            end,
            pos: start,
            len,
        })
    }

    pub fn len(&self) -> u64 {
        match self.end {
            Some(end) => end - self.start + 1,
            None => self.len - self.start,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<R: AsyncRead + AsyncSeek + Unpin> AsyncRead for AsyncRangeReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = &mut *self;

        if let Some(end) = me.end {
            if me.pos > end {
                return Poll::Ready(Ok(()));
            }

            let remaining = (end - me.pos + 1) as usize;
            let unfilled = buf.initialize_unfilled();

            let to_read = unfilled.len().min(remaining);
            let limited_buf = &mut unfilled[..to_read];

            let mut limited_read_buf = tokio::io::ReadBuf::new(limited_buf);

            ready!(Pin::new(&mut me.inner).poll_read(cx, &mut limited_read_buf))?;

            let filled_after = limited_read_buf.filled().len();
            let bytes_read = filled_after;

            buf.advance(bytes_read);

            me.pos += bytes_read as u64;

            Poll::Ready(Ok(()))
        } else {
            let filled_before = buf.filled().len();

            let result = ready!(Pin::new(&mut me.inner).poll_read(cx, buf));

            let filled_after = buf.filled().len();
            let bytes_read = filled_after - filled_before;

            me.pos += bytes_read as u64;

            Poll::Ready(result)
        }
    }
}
