use serde::Deserialize;
use utoipa::ToSchema;

#[derive(ToSchema, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
#[schema(rename_all = "lowercase")]
pub enum ServerConfigurationFileParser {
    File,
    Yaml,
    Properties,
    Ini,
    Json,
    Xml,
}

#[derive(ToSchema, Deserialize)]
pub struct ServerConfigurationFileReplacement {
    pub r#match: String,
    pub if_value: Option<String>,
    pub replace_with: serde_json::Value,
}

#[derive(ToSchema, Deserialize)]
pub struct ServerConfigurationFile {
    pub file: String,
    pub parser: ServerConfigurationFileParser,
    pub replace: Vec<ServerConfigurationFileReplacement>,
}

impl ServerConfigurationFile {
    fn lookup_value(
        server: &crate::server::Server,
        replacement: &serde_json::Value,
    ) -> Option<String> {
        let value = replacement.as_str()?;

        Some(value.to_string())
    }
}

nestify::nest! {
    #[derive(ToSchema, Deserialize)]
    pub struct ProcessConfiguration {
        pub startup: #[derive(ToSchema, Deserialize, Clone)] pub struct ProcessConfigurationStartup {
            pub done: Vec<String>,
            pub strip_ansi: bool,
        },
        pub stop: #[derive(ToSchema, Deserialize)] pub struct ProcessConfigurationStop {
            pub r#type: String,
            pub value: Option<String>,
        },

        pub configs: Vec<ServerConfigurationFile>,
    }
}

impl ProcessConfiguration {
    pub async fn update_files(&self, filesystem: &crate::server::filesystem::Filesystem) {
        // todo
    }
}
