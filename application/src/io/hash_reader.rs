use sha1::{Digest, digest::Output};
use std::io::Read;

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
