use super::{CompressionLevel, CompressionType};
use gzp::ZWriter;
use std::{
    io::Write,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::AsyncWrite;

pub enum CompressionWriter<'a, W: Write + Send + 'static> {
    None(W),
    Gz(gzp::par::compress::ParCompress<'a, gzp::deflate::Gzip, W>),
    Xz(usize, Box<lzma_rust2::XzWriterMt<W>>),
    Bz2(bzip2::write::BzEncoder<W>),
    Lz4(lzzzz::lz4f::WriteCompressor<W>),
    Zstd(usize, bool, zstd::Encoder<'a, W>),
}

impl<'a, W: Write + Send + 'static> CompressionWriter<'a, W> {
    pub fn new(
        writer: W,
        compression_type: CompressionType,
        compression_level: CompressionLevel,
        threads: usize,
    ) -> Self {
        match compression_type {
            CompressionType::None => CompressionWriter::None(writer),
            CompressionType::Gz => CompressionWriter::Gz(
                gzp::par::compress::ParCompressBuilder::new()
                    .num_threads(threads)
                    .unwrap()
                    .compression_level(gzp::Compression::new(compression_level.to_deflate_level()))
                    .from_writer(writer),
            ),
            CompressionType::Xz => CompressionWriter::Xz(
                0,
                Box::new(
                    lzma_rust2::XzWriterMt::new(
                        writer,
                        {
                            let mut options =
                                lzma_rust2::XzOptions::with_preset(compression_level.to_xz_level());
                            options.set_block_size(Some(
                                std::num::NonZeroU64::new(16 * 1024).unwrap(),
                            ));

                            options
                        },
                        threads as u32,
                    )
                    .unwrap(),
                ),
            ),
            CompressionType::Bz2 => CompressionWriter::Bz2(bzip2::write::BzEncoder::new(
                writer,
                bzip2::Compression::new(compression_level.to_bz2_level()),
            )),
            CompressionType::Lz4 => CompressionWriter::Lz4(
                lzzzz::lz4f::WriteCompressor::new(
                    writer,
                    lzzzz::lz4f::PreferencesBuilder::new()
                        .compression_level(compression_level.to_lz4_level())
                        .build(),
                )
                .unwrap(),
            ),
            CompressionType::Zstd => CompressionWriter::Zstd(0, threads > 1, {
                let mut encoder =
                    zstd::Encoder::new(writer, compression_level.to_zstd_level()).unwrap();
                if threads > 1 {
                    encoder.multithread(threads as u32).ok();
                }

                encoder
            }),
        }
    }

    pub fn finish(self) -> std::io::Result<W> {
        match self {
            CompressionWriter::None(writer) => Ok(writer),
            CompressionWriter::Gz(mut writer) => {
                Ok(writer.finish().map_err(std::io::Error::other)?)
            }
            CompressionWriter::Xz(_, writer) => Ok(writer.finish()?),
            CompressionWriter::Bz2(writer) => Ok(writer.finish()?),
            CompressionWriter::Lz4(mut writer) => {
                writer.flush()?;
                Ok(writer.into_inner())
            }
            CompressionWriter::Zstd(_, _, writer) => Ok(writer.finish()?),
        }
    }
}

impl<'a, W: Write + Send + 'static> Write for CompressionWriter<'a, W> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            CompressionWriter::None(writer) => writer.write(buf),
            CompressionWriter::Gz(writer) => writer.write(buf),
            CompressionWriter::Xz(i, writer) => {
                *i += 1;

                if *i % 64 == 0 {
                    writer.flush()?;
                }

                writer.write(buf)
            }
            CompressionWriter::Bz2(writer) => writer.write(buf),
            CompressionWriter::Lz4(writer) => writer.write(buf),
            CompressionWriter::Zstd(i, mt, writer) => {
                if *mt {
                    *i += 1;

                    if *i % 64 == 0 {
                        writer.flush()?;
                    }
                }

                writer.write(buf)
            }
        }
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            CompressionWriter::None(writer) => writer.flush(),
            CompressionWriter::Gz(writer) => writer.flush(),
            CompressionWriter::Xz(_, writer) => writer.flush(),
            CompressionWriter::Bz2(writer) => writer.flush(),
            CompressionWriter::Lz4(writer) => writer.flush(),
            CompressionWriter::Zstd(_, _, writer) => writer.flush(),
        }
    }
}

pub struct AsyncCompressionWriter {
    inner_error_receiver: tokio::sync::oneshot::Receiver<std::io::Error>,
    inner_writer: tokio::io::DuplexStream,
}

impl AsyncCompressionWriter {
    pub fn new(
        writer: impl Write + Send + 'static,
        compression_type: CompressionType,
        compression_level: CompressionLevel,
        threads: usize,
    ) -> Self {
        let (inner_reader, inner_writer) = tokio::io::duplex(crate::BUFFER_SIZE * 4);
        let (inner_error_sender, inner_error_receiver) = tokio::sync::oneshot::channel();

        tokio::task::spawn_blocking(move || {
            let mut reader = tokio_util::io::SyncIoBridge::new(inner_reader);
            let mut stream =
                CompressionWriter::new(writer, compression_type, compression_level, threads);

            match crate::io::copy(&mut reader, &mut stream) {
                Ok(_) => {}
                Err(e) => {
                    let _ = inner_error_sender.send(e);
                    return;
                }
            }

            match stream.finish() {
                Ok(_) => {}
                Err(e) => {
                    let _ = inner_error_sender.send(e);
                }
            }
        });

        Self {
            inner_error_receiver,
            inner_writer,
        }
    }
}

impl AsyncWrite for AsyncCompressionWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if !self.inner_error_receiver.is_terminated()
            && let Poll::Ready(result) = Pin::new(&mut self.inner_error_receiver).poll(cx)
            && let Ok(err) = result
        {
            return Poll::Ready(Err(err));
        }

        Pin::new(&mut self.inner_writer).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if !self.inner_error_receiver.is_terminated()
            && let Poll::Ready(result) = Pin::new(&mut self.inner_error_receiver).poll(cx)
            && let Ok(err) = result
        {
            return Poll::Ready(Err(err));
        }

        Pin::new(&mut self.inner_writer).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if !self.inner_error_receiver.is_terminated()
            && let Poll::Ready(result) = Pin::new(&mut self.inner_error_receiver).poll(cx)
            && let Ok(err) = result
        {
            return Poll::Ready(Err(err));
        }

        Pin::new(&mut self.inner_writer).poll_shutdown(cx)
    }
}
