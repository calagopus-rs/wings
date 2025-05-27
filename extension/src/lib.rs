use wings_rs::extensions::Extension;

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn load_extension() -> Box<dyn Extension> {
    Box::new(ExampleExtension)
}

#[unsafe(no_mangle)]
pub extern "C" fn api_version() -> u32 {
    wings_rs::extensions::API_VERSION
}

#[repr(C)]
pub struct ExampleExtension;

impl Extension for ExampleExtension {
    fn info(&self) -> wings_rs::extensions::ExtensionInfo {
        wings_rs::extensions::ExtensionInfo {
            name: "Example Extension",
            description: "An example extension for demonstration purposes.",
            version: env!("CARGO_PKG_VERSION"),

            author: "Your Name",
            license: "MIT",

            additional: serde_json::Map::new(),
        }
    }

    fn on_init(&self, state: wings_rs::routes::State) {
        println!(
            "ExampleExtension initialized with app version: {:?}",
            state.version
        );
    }

    fn router(
        &self,
        state: wings_rs::routes::State,
    ) -> utoipa_axum::router::OpenApiRouter<wings_rs::routes::State> {
        utoipa_axum::router::OpenApiRouter::new()
            .route(
                "/example",
                axum::routing::get(|| async { "This is an example endpoint." }),
            )
            .with_state(state)
    }
}
