use std::io::{Read, Write};

pub mod abort;
pub mod compression;

pub mod counting_reader;
pub mod counting_writer;
pub mod fixed_reader;
pub mod limited_reader;
pub mod limited_writer;

pub fn copy(
    reader: &mut (impl ?Sized + Read),
    writer: &mut (impl ?Sized + Write),
) -> std::io::Result<()> {
    let mut buffer = vec![0; crate::BUFFER_SIZE];

    copy_shared(&mut buffer, reader, writer)
}

pub fn copy_shared(
    buffer: &mut [u8],
    reader: &mut (impl ?Sized + Read),
    writer: &mut (impl ?Sized + Write),
) -> std::io::Result<()> {
    loop {
        let bytes_read = reader.read(buffer)?;

        if crate::unlikely(bytes_read == 0) {
            break;
        }

        writer.write_all(&buffer[..bytes_read])?;
    }

    Ok(())
}
