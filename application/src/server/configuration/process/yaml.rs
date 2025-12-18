use super::ServerConfigurationFile;

pub struct YamlFileParser;

#[async_trait::async_trait]
impl super::ProcessConfigurationFileParser for YamlFileParser {
    async fn process_file(
        content: &str,
        config: &ServerConfigurationFile,
        server: &crate::server::Server,
    ) -> Result<Vec<u8>, anyhow::Error> {
        tracing::debug!(
            server = %server.uuid,
            "processing yaml file"
        );

        let mut json: serde_json::Value = if content.trim().is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            serde_yml::from_str(content)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
        };

        for replacement in &config.replace {
            let path_parts: Vec<&str> = replacement.r#match.split('.').collect();

            let value = match &replacement.replace_with {
                serde_json::Value::String(_) => {
                    let resolved = ServerConfigurationFile::replace_all_placeholders(
                        server,
                        &replacement.replace_with,
                    )
                    .await?;
                    serde_json::from_str(&resolved)
                        .unwrap_or(serde_json::Value::String(resolved.into()))
                }
                other => other.clone(),
            };

            super::json::set_nested_value(
                &mut json,
                &path_parts,
                value,
                replacement.insert_new.unwrap_or(true),
                replacement.update_existing,
            );
        }

        Ok(serde_yml::to_string(&json)?.into_bytes())
    }
}
