use wings_rs::{export_extension, extensions::Extension};

export_extension!(ExampleExtension);

mod routes;

#[derive(Default)]
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

    fn on_init(&mut self, state: wings_rs::routes::State) {
        println!(
            "ExampleExtension initialized with app version: {:?}",
            state.version
        );
    }

    fn router(
        &mut self,
        state: wings_rs::routes::State,
    ) -> utoipa_axum::router::OpenApiRouter<wings_rs::routes::State> {
        utoipa_axum::router::OpenApiRouter::new()
            .route(
                "/example",
                axum::routing::get(|| async { "This is an example endpoint." }),
            )
            .merge(routes::router(&state))
            .with_state(state)
    }
}
