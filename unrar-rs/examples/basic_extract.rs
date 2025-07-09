use unrar::Archive;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut archive =
        Archive::new("../archive.rar")
            .open_for_processing()
            .unwrap();
    while let Some(header) = archive.read_header()? {
        println!(
            "{} bytes: {}",
            header.entry().unpacked_size,
            header.entry().filename.to_string_lossy(),
        );
        archive = if header.entry().is_file() {
            header.extract()?
        } else {
            header.skip()?
        };
    }
    Ok(())
}
