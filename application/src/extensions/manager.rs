use libloading::Symbol;

pub struct Manager {
    extensions: Vec<Box<dyn super::Extension>>,
    _libraries: Vec<libloading::Library>,
}

impl Manager {
    pub fn new(path: &str) -> Self {
        let mut extensions = Vec::new();
        let mut _libraries = Vec::new();

        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                if let Ok(library) = unsafe { libloading::Library::new(entry.path()) } {
                    let load_extension: Symbol<
                        unsafe extern "C" fn() -> Box<dyn super::Extension>,
                    > = unsafe { library.get(b"load_extension\0").unwrap() };

                    let api_version: Symbol<unsafe extern "C" fn() -> u32> =
                        unsafe { library.get(b"api_version\0").unwrap() };
                    let api_version = unsafe { api_version() };
                    if api_version != super::API_VERSION {
                        tracing::warn!(
                            path = %entry.path().display(),
                            "API version mismatch: expected {}, found {}",
                            super::API_VERSION,
                            api_version
                        );

                        continue;
                    }

                    let extension: Box<dyn super::Extension + 'static> =
                        unsafe { load_extension() };

                    tracing::info!(
                        info = ?extension.info(),
                        "loaded extension"
                    );

                    extensions.push(extension);
                    _libraries.push(library);
                } else {
                    tracing::warn!(
                        path = %entry.path().display(),
                        "failed to load extension"
                    );
                }
            }
        } else {
            tracing::error!(path = path, "failed to read extensions directory");
        }

        Self {
            extensions,
            _libraries,
        }
    }

    pub fn get_extensions(&self) -> &[Box<dyn super::Extension>] {
        &self.extensions
    }
}
