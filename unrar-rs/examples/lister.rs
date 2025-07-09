use std::io::Write;
use unrar::{Archive, ListSplit};

fn main() {
    // Basic args parsing
    // Usage: cargo run --example lister path/to/archive.rar
    let args = std::env::args();
    let mut stderr = std::io::stderr();
    let file = args.skip(1).next().unwrap_or_else(|| {
        writeln!(&mut stderr, "Please pass an archive as argument!").unwrap();
        std::process::exit(1)
    });

    let mut possible_error = None;

    match Archive::new(&file).break_open::<ListSplit>(Some(&mut possible_error)) {
        Ok(archive) => {
            if let Some(error) = possible_error {
                // If the error's data field holds an OpenArchive, an error occurred while opening,
                // the archive is partly broken (e.g. broken header), but is still readable from.
                // In this example, we are still going to use the archive and list its contents.
                writeln!(&mut stderr, "Error: {}, continuing.", error).unwrap();
            }
            let mut stderr = std::io::stderr();
            for entry in archive {
                match entry {
                    Ok(e) => println!("{}", e),
                    Err(err) => writeln!(&mut stderr, "Error: {}", err).unwrap(),
                }
            }    
        }
        Err(e) => {
            // the error we passed in is always None
            // if the archive could not be read at all
            debug_assert_eq!(possible_error, None);
            writeln!(&mut stderr, "Error: {}", e).unwrap();
        }
    }

}
