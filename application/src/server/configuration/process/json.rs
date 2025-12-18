use super::ServerConfigurationFile;

pub struct JsonFileParser;

#[async_trait::async_trait]
impl super::ProcessConfigurationFileParser for JsonFileParser {
    async fn process_file(
        content: &str,
        config: &ServerConfigurationFile,
        server: &crate::server::Server,
    ) -> Result<Vec<u8>, anyhow::Error> {
        tracing::debug!(
            server = %server.uuid,
            "processing json file"
        );

        let mut json = if content.trim().is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(content)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
        };

        for replacement in &config.replace {
            let value = match &replacement.replace_with {
                serde_json::Value::String(_) => {
                    let resolved = ServerConfigurationFile::replace_all_placeholders(
                        server,
                        &replacement.replace_with,
                    )
                    .await?;

                    serde_json::from_str(&resolved)
                        .unwrap_or_else(|_| serde_json::Value::String(resolved.into()))
                }
                other => other.clone(),
            };

            let path: Vec<&str> = replacement.r#match.split('.').collect();
            set_nested_value(
                &mut json,
                &path,
                value,
                replacement.insert_new.unwrap_or(true),
                replacement.update_existing,
            );
        }

        Ok(serde_json::to_vec_pretty(&json)?)
    }
}

pub fn set_nested_value(
    json: &mut serde_json::Value,
    path: &[&str],
    value: serde_json::Value,
    insert_new: bool,
    update_existing: bool,
) {
    if path.is_empty() {
        return;
    }

    if !json.is_object() {
        *json = serde_json::Value::Object(serde_json::Map::new());
    }

    let map = json.as_object_mut().unwrap();

    if path.len() == 1 {
        let key = path[0].to_string();
        let exists = map.contains_key(&key);

        if (exists && update_existing) || (!exists && insert_new) {
            map.insert(key, value);
        }
        return;
    }

    let child = map
        .entry(path[0].to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

    set_nested_value(child, &path[1..], value, insert_new, update_existing);
}
