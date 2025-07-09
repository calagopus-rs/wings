use unrar::{Archive, UnrarResult};

fn main() -> UnrarResult<()> {
    // Basic args parsing
    // Usage: cargo run --example extract_named <archive> <entry-filename-to-print>
    let mut args = std::env::args_os().skip(1);
    let file = args.next().unwrap_or("archive.rar".into());
    let name = args.next().unwrap_or("README.md".into());

    let mut archive = Archive::new(&file).open_for_processing().unwrap();
    while let Some(header) = archive.read_header()? {
        archive = if header.entry().filename.as_os_str() == name {
            let (data, rest) = header.read()?;
            drop(rest); // close the archive
            match std::str::from_utf8(&data) {
                Ok(content) => {
                    if content.len() <= 10000 {
                        print!("{content}");
                        std::process::exit(0);
                    } else {
                        eprintln!("error: file too long for this example (is: {}, max: 10000)", content.len());
                        std::process::exit(1);
                    }
                }
                Err(_) => {
                    eprintln!("error: non-utf8 content");
                    std::process::exit(1);
                }
            }
        } else {
            header.skip()?
        }
    }
    Ok(())
}
