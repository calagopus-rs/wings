use super::ServerConfigurationFile;
use std::collections::HashSet;

pub struct PropertiesFileParser;

#[async_trait::async_trait]
impl super::ProcessConfigurationFileParser for PropertiesFileParser {
    async fn process_file(
        content: &str,
        config: &ServerConfigurationFile,
        server: &crate::server::Server,
    ) -> Result<Vec<u8>, anyhow::Error> {
        tracing::debug!(
            server = %server.uuid,
            "processing properties file"
        );

        let mut result = Vec::new();
        let property_iter = java_properties::PropertiesIter::new(content.as_bytes());
        let mut properties = java_properties::PropertiesWriter::new(&mut result);
        let mut found_keys = HashSet::new();

        for line in property_iter {
            match line?.consume_content() {
                java_properties::LineContent::Comment(comment) => {
                    properties.write_comment(&comment)?;
                }
                java_properties::LineContent::KVPair(key, mut existing_value) => {
                    for replacement in &config.replace {
                        if replacement.r#match != key || !replacement.update_existing {
                            continue;
                        }

                        let value = ServerConfigurationFile::replace_all_placeholders(
                            server,
                            &replacement.replace_with,
                        )
                        .await?;

                        if let Some(if_value) = &replacement.if_value
                            && &existing_value != if_value
                        {
                            tracing::debug!(
                                server = %server.uuid,
                                "skipping replacement for '{}': value '{}' != '{}'",
                                replacement.r#match, existing_value, if_value
                            );
                            continue;
                        }

                        existing_value = value;
                    }

                    properties.write(&key, &existing_value)?;
                    found_keys.insert(key);
                }
            }
        }

        for replacement in &config.replace {
            let insert_new = replacement.insert_new.unwrap_or(true);
            if found_keys.contains(&replacement.r#match) || !insert_new {
                continue;
            }

            let value = ServerConfigurationFile::replace_all_placeholders(
                server,
                &replacement.replace_with,
            )
            .await?;

            properties.write(&replacement.r#match, &value)?;
        }

        properties.finish()?;

        Ok(result)
    }
}
