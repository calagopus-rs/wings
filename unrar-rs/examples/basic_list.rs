use unrar::Archive;

fn main() {
    // Basic args parsing
    // Usage: cargo run --example basic_list path/to/archive.rar
    let args = std::env::args();
    let file = args.skip(1).next().unwrap_or("archive.rar".to_owned());

    let archive = Archive::new(&file).open_for_listing().unwrap();
    for e in archive {
        let entry = e.unwrap();
        println!("{}", entry.filename.to_string_lossy());
    }
}
