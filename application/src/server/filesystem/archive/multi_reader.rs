use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::FileExt,
    sync::Arc,
};

#[derive(Clone)]
pub struct MultiReader {
    file: Arc<File>,
    offset: u64,
}

impl MultiReader {
    pub fn new(file: Arc<File>) -> Self {
        MultiReader { file, offset: 0 }
    }
}

impl Read for MultiReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let bytes_read = self.file.read_at(buf, self.offset)?;
        self.offset += bytes_read as u64;

        Ok(bytes_read)
    }
}

impl Seek for MultiReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.offset = match pos {
            SeekFrom::Start(offset) => offset,
            SeekFrom::End(offset) => {
                let file_size = self.file.metadata()?.len();
                if offset >= 0 {
                    file_size.saturating_add(offset as u64)
                } else {
                    file_size.saturating_sub((-offset) as u64)
                }
            }
            SeekFrom::Current(offset) => {
                if offset >= 0 {
                    self.offset.saturating_add(offset as u64)
                } else {
                    self.offset.saturating_sub((-offset) as u64)
                }
            }
        };

        Ok(self.offset)
    }
}
