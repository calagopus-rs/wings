use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::FileExt,
    sync::Arc,
};

#[derive(Clone)]
pub struct MultiReader {
    file: Arc<File>,
    file_size: u64,
    offset: u64,
}

impl MultiReader {
    pub fn new(file: Arc<File>) -> std::io::Result<Self> {
        let file_size = file.metadata()?.len();

        Ok(MultiReader {
            file,
            file_size,
            offset: 0,
        })
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
                if offset >= 0 {
                    self.file_size.saturating_add(offset as u64)
                } else {
                    self.file_size.saturating_sub((-offset) as u64)
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
