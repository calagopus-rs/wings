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
    Gz(gzp::par::compress::ParCompress<gzp::deflate::Gzip>),
    Xz(Box<lzma_rust2::XZWriterMT<W>>),
    Bz2(bzip2::write::BzEncoder<W>),
    Lz4(lz4::Encoder<W>),
    Zstd(zstd::Encoder<'a, W>),
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
            CompressionType::Xz => CompressionWriter::Xz(Box::new(
                lzma_rust2::XZWriterMT::new(
                    writer,
                    {
                        let mut options =
                            lzma_rust2::XZOptions::with_preset(match compression_level {
                                CompressionLevel::BestSpeed => 1,
                                CompressionLevel::GoodSpeed => 4,
                                CompressionLevel::GoodCompression => 6,
                                CompressionLevel::BestCompression => 9,
                            });
                        options.set_block_size(Some(std::num::NonZeroU64::new(1 << 20).unwrap()));

                        options
                    },
                    threads as u32,
                )
                .unwrap(),
            )),
            CompressionType::Bz2 => CompressionWriter::Bz2(bzip2::write::BzEncoder::new(
                writer,
                bzip2::Compression::new(match compression_level {
                    CompressionLevel::BestSpeed => 1,
                    CompressionLevel::GoodSpeed => 4,
                    CompressionLevel::GoodCompression => 6,
                    CompressionLevel::BestCompression => 9,
                }),
            )),
            CompressionType::Lz4 => {
                CompressionWriter::Lz4(lz4::EncoderBuilder::new().build(writer).unwrap())
            }
            CompressionType::Zstd => CompressionWriter::Zstd({
                let mut encoder = zstd::Encoder::new(
                    writer,
                    match compression_level {
                        CompressionLevel::BestSpeed => 1,
                        CompressionLevel::GoodSpeed => 8,
                        CompressionLevel::GoodCompression => 14,
                        CompressionLevel::BestCompression => 22,
                    },
                )
                .unwrap();
                encoder.multithread(threads as u32).ok();

                encoder
            }),
        }
    }

    pub fn finish(self) -> std::io::Result<()> {
        match self {
            CompressionWriter::None(mut writer) => writer.flush(),
            CompressionWriter::Gz(mut writer) => writer.finish().map_err(std::io::Error::other),
            CompressionWriter::Xz(writer) => {
                writer.finish()?;
                Ok(())
            }
            CompressionWriter::Bz2(writer) => {
                writer.finish()?;
                Ok(())
            }
            CompressionWriter::Lz4(writer) => {
                let (_, result) = writer.finish();

                result?;
                Ok(())
            }
            CompressionWriter::Zstd(writer) => {
                writer.finish()?;
                Ok(())
            }
        }
    }
}

impl<'a, R: Write + Send + 'static> Write for CompressionWriter<'a, R> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            CompressionWriter::None(writer) => writer.write(buf),
            CompressionWriter::Gz(writer) => writer.write(buf),
            CompressionWriter::Xz(writer) => writer.write(buf),
            CompressionWriter::Bz2(writer) => writer.write(buf),
            CompressionWriter::Lz4(writer) => writer.write(buf),
            CompressionWriter::Zstd(writer) => writer.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            CompressionWriter::None(writer) => writer.flush(),
            CompressionWriter::Gz(writer) => writer.flush(),
            CompressionWriter::Xz(writer) => writer.flush(),
            CompressionWriter::Bz2(writer) => writer.flush(),
            CompressionWriter::Lz4(writer) => writer.flush(),
            CompressionWriter::Zstd(writer) => writer.flush(),
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

            match std::io::copy(&mut reader, &mut stream) {
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
        if let Poll::Ready(result) = Pin::new(&mut self.inner_error_receiver).poll(cx)
            && let Ok(err) = result
        {
            return Poll::Ready(Err(err));
        }

        Pin::new(&mut self.inner_writer).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if let Poll::Ready(result) = Pin::new(&mut self.inner_error_receiver).poll(cx)
            && let Ok(err) = result
        {
            return Poll::Ready(Err(err));
        }

        Pin::new(&mut self.inner_writer).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if let Poll::Ready(result) = Pin::new(&mut self.inner_error_receiver).poll(cx)
            && let Ok(err) = result
        {
            return Poll::Ready(Err(err));
        }

        Pin::new(&mut self.inner_writer).poll_shutdown(cx)
    }
}
