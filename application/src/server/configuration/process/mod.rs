use anyhow::Context;
use compact_str::ToCompactString;
use serde::Deserialize;
use serde_default::DefaultFromSerde;
use std::path::Path;
use utoipa::ToSchema;

mod ini;
mod json;
mod plain;
mod properties;
mod xml;
mod yaml;

fn true_fn() -> bool {
    true
}

#[derive(ToSchema, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
#[schema(rename_all = "lowercase")]
pub enum ServerConfigurationFileParser {
    File,
    #[serde(alias = "yml")]
    Yaml,
    Properties,
    Ini,
    Json,
    Xml,
}

#[derive(ToSchema, Deserialize, Clone, Debug)]
pub struct ServerConfigurationFileReplacement {
    pub r#match: String,
    pub if_value: Option<String>,
    #[schema(value_type = bool)]
    pub insert_new: Option<bool>,
    #[serde(default = "true_fn")]
    pub update_existing: bool,
    #[serde(alias = "value")]
    pub replace_with: serde_json::Value,
}

#[derive(ToSchema, Deserialize, Clone, Debug)]
pub struct ServerConfigurationFile {
    pub file: String,
    #[serde(default = "true_fn")]
    pub create_new: bool,
    pub parser: ServerConfigurationFileParser,
    #[serde(default)]
    pub replace: Vec<ServerConfigurationFileReplacement>,
}

impl ServerConfigurationFile {
    async fn lookup_value(
        server: &crate::server::Server,
        replacement: &serde_json::Value,
    ) -> Result<compact_str::CompactString, anyhow::Error> {
        let value = match replacement {
            serde_json::Value::String(s) => s.as_str(),
            serde_json::Value::Number(n) => return Ok(n.to_compact_string()),
            serde_json::Value::Bool(b) => return Ok(b.to_compact_string()),
            serde_json::Value::Null => return Ok(compact_str::CompactString::default()),
            _ => return Ok(replacement.to_compact_string()),
        };

        if !value.starts_with("{{") || !value.ends_with("}}") {
            return Ok(value.to_compact_string());
        }

        let variable = value.trim_start_matches("{{").trim_end_matches("}}").trim();

        tracing::debug!(
            server = %server.uuid,
            "looking up variable: {}",
            variable
        );

        let parts: Vec<&str> = variable.split('.').collect();
        if parts.is_empty() {
            tracing::error!(
                server = %server.uuid,
                "empty variable path"
            );
            return Ok(compact_str::CompactString::default());
        }

        match parts[0] {
            "server" => Self::lookup_server_variable(server, &parts[1..]).await,
            "config" => Self::lookup_config_variable(&server.app_state.config, &parts[1..]).await,
            "env" => {
                if parts.len() < 2 {
                    return Ok(compact_str::CompactString::default());
                }
                let config = server.configuration.read().await;
                let env_var = parts[1];
                if let Some(value) = config.environment.get(env_var) {
                    Ok(value
                        .as_str()
                        .map_or_else(|| value.to_compact_string(), |v| v.into()))
                } else {
                    tracing::warn!(
                        server = %server.uuid,
                        "environment variable not found: {}",
                        env_var
                    );
                    Ok(compact_str::CompactString::default())
                }
            }
            _ => {
                tracing::error!(
                    server = %server.uuid,
                    "unknown variable prefix: {}",
                    parts[0]
                );
                Ok(compact_str::CompactString::default())
            }
        }
    }

    async fn lookup_server_variable(
        server: &crate::server::Server,
        parts: &[&str],
    ) -> Result<compact_str::CompactString, anyhow::Error> {
        if parts.is_empty() {
            return Ok(compact_str::CompactString::default());
        }

        let config = server.configuration.read().await;

        match parts[0] {
            "build" => {
                if parts.len() < 2 {
                    return Ok(compact_str::CompactString::default());
                }

                match parts[1] {
                    "memory" => Ok(config.build.memory_limit.to_compact_string()),
                    "swap" => Ok(config.build.swap.to_compact_string()),
                    "io" => Ok(config
                        .build
                        .io_weight
                        .map_or_else(|| "500".into(), |v| v.to_compact_string())),
                    "cpu" => Ok(config.build.cpu_limit.to_compact_string()),
                    "disk" => Ok(config.build.disk_space.to_compact_string()),
                    "threads" => Ok(config.build.threads.clone().unwrap_or_default()),
                    "default" => {
                        if parts.len() < 3 {
                            return Ok(compact_str::CompactString::default());
                        }
                        match parts[2] {
                            "port" => Ok(config
                                .allocations
                                .default
                                .as_ref()
                                .map(|d| d.port.to_compact_string())
                                .unwrap_or_default()),
                            "ip" => Ok(config
                                .allocations
                                .default
                                .as_ref()
                                .map(|d| d.ip.to_compact_string())
                                .unwrap_or_default()),
                            _ => {
                                tracing::error!(
                                    server = %server.uuid,
                                    "unknown server.build.default subpath: {}",
                                    parts[2]
                                );
                                Ok(compact_str::CompactString::default())
                            }
                        }
                    }
                    "env" => {
                        if parts.len() < 3 {
                            return Ok(compact_str::CompactString::default());
                        }
                        let env_var = parts[2];
                        if let Some(value) = config.environment.get(env_var) {
                            Ok(value
                                .as_str()
                                .map_or_else(|| value.to_compact_string(), |v| v.into()))
                        } else {
                            tracing::warn!(
                                server = %server.uuid,
                                "environment variable not found: {}",
                                env_var
                            );
                            Ok(compact_str::CompactString::default())
                        }
                    }
                    _ => {
                        tracing::error!(
                            server = %server.uuid,
                            "unknown server.build subpath: {}",
                            parts[1]
                        );
                        Ok(compact_str::CompactString::default())
                    }
                }
            }
            "env" => {
                if parts.len() < 2 {
                    return Ok(compact_str::CompactString::default());
                }
                let env_var = parts[1];
                if let Some(value) = config.environment.get(env_var) {
                    Ok(value
                        .as_str()
                        .map_or_else(|| value.to_compact_string(), |v| v.into()))
                } else {
                    tracing::warn!(
                        server = %server.uuid,
                        "environment variable not found: {}",
                        env_var
                    );
                    Ok(compact_str::CompactString::default())
                }
            }
            _ => {
                tracing::error!(
                    server = %server.uuid,
                    "unknown server section: {}",
                    parts[0]
                );
                Ok(compact_str::CompactString::default())
            }
        }
    }

    async fn lookup_config_variable(
        config: &crate::config::Config,
        parts: &[&str],
    ) -> Result<compact_str::CompactString, anyhow::Error> {
        if parts.is_empty() || parts[0] == "token_id" || parts[0] == "token" {
            return Ok(compact_str::CompactString::default());
        }

        let config_json =
            serde_json::to_value(&**config).context("failed to serialize Wings configuration")?;

        let mut current = &config_json;
        for part in parts {
            match current.get(part) {
                Some(value) => current = value,
                None => {
                    tracing::warn!("config path not found: {}", parts.join("."));
                    return Ok(compact_str::CompactString::default());
                }
            }
        }

        Ok(match current {
            serde_json::Value::String(s) => s.to_compact_string(),
            serde_json::Value::Number(n) => n.to_compact_string(),
            serde_json::Value::Bool(b) => b.to_compact_string(),
            serde_json::Value::Null => compact_str::CompactString::default(),
            _ => current.to_compact_string(),
        })
    }

    async fn replace_all_placeholders(
        server: &crate::server::Server,
        input: &serde_json::Value,
    ) -> Result<compact_str::CompactString, anyhow::Error> {
        let input = match input.as_str() {
            Some(s) => s,
            None => return Self::lookup_value(server, input).await,
        };

        let mut result = compact_str::CompactString::default();
        let mut chars = input.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '{' && chars.peek() == Some(&'{') {
                chars.next();
                let mut placeholder = compact_str::CompactString::from("{{");
                let mut found_end = false;

                while let Some(ch) = chars.next() {
                    placeholder.push(ch);
                    if ch == '}' && chars.peek() == Some(&'}') {
                        chars.next();
                        placeholder.push('}');
                        found_end = true;
                        break;
                    }
                }

                if found_end {
                    let value = serde_json::Value::String(placeholder.to_string());
                    match Self::lookup_value(server, &value).await {
                        Ok(replacement) => result.push_str(&replacement),
                        Err(e) => {
                            tracing::error!(
                                server = %server.uuid,
                                "failed to lookup variable {}: {}",
                                placeholder, e
                            );
                            result.push_str(&placeholder);
                        }
                    }
                } else {
                    result.push_str(&placeholder);
                }
            } else {
                result.push(ch);
            }
        }

        Ok(result)
    }
}

nestify::nest! {
    #[derive(ToSchema, Deserialize)]
    pub struct ProcessConfiguration {
        #[serde(default)]
        pub startup: #[derive(ToSchema, Deserialize, Clone, DefaultFromSerde)] pub struct ProcessConfigurationStartup {
            pub done: Option<Vec<compact_str::CompactString>>,
            #[serde(default)]
            pub strip_ansi: bool,
        },
        #[serde(default)]
        pub stop: #[derive(ToSchema, Deserialize, DefaultFromSerde)] pub struct ProcessConfigurationStop {
            #[serde(default)]
            pub r#type: compact_str::CompactString,
            pub value: Option<compact_str::CompactString>,
        },

        #[serde(default)]
        pub configs: Vec<ServerConfigurationFile>,
    }
}

impl ProcessConfiguration {
    pub async fn update_files(&self, server: &crate::server::Server) -> Result<(), anyhow::Error> {
        tracing::info!(
            server = %server.uuid,
            "starting configuration file updates with {} configuration files",
            self.configs.len()
        );

        if self.configs.is_empty() {
            return Ok(());
        }

        for config in self.configs.iter() {
            let file_path = server.filesystem.relative_path(Path::new(&config.file));

            if let Some(parent) = file_path.parent() {
                server.filesystem.async_create_dir_all(&parent).await?;
            }

            let mut file_content = String::new();
            if let Ok(metadata) = server.filesystem.async_metadata(&file_path).await
                && metadata.is_file()
            {
                file_content = server
                    .filesystem
                    .async_read_to_string(&file_path, 1024 * 1024)
                    .await
                    .unwrap_or_default();
            } else if !config.create_new {
                continue;
            }

            let updated_content = match config.parser {
                ServerConfigurationFileParser::Properties => {
                    properties::PropertiesFileParser::process_file(&file_content, config, server)
                        .await?
                }
                ServerConfigurationFileParser::Json => {
                    json::JsonFileParser::process_file(&file_content, config, server).await?
                }
                ServerConfigurationFileParser::Yaml => {
                    yaml::YamlFileParser::process_file(&file_content, config, server).await?
                }
                ServerConfigurationFileParser::Ini => {
                    ini::IniFileParser::process_file(&file_content, config, server).await?
                }
                ServerConfigurationFileParser::Xml => {
                    xml::XmlFileParser::process_file(&file_content, config, server).await?
                }
                ServerConfigurationFileParser::File => {
                    plain::PlainFileParser::process_file(&file_content, config, server).await?
                }
            };

            server
                .filesystem
                .async_write(&file_path, updated_content)
                .await?;

            tracing::debug!(
                server = %server.uuid,
                "successfully processed configuration file: {}",
                file_path.display()
            );
        }

        tracing::info!(
            server = %server.uuid,
            "completed all configuration file updates"
        );

        Ok(())
    }
}

#[async_trait::async_trait]
pub trait ProcessConfigurationFileParser {
    async fn process_file(
        content: &str,
        config: &ServerConfigurationFile,
        server: &crate::server::Server,
    ) -> Result<Vec<u8>, anyhow::Error>;
}
