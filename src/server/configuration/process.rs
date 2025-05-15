use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
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

#[derive(ToSchema, Deserialize, Clone)]
pub struct ServerConfigurationFileReplacement {
    pub r#match: String,
    pub if_value: Option<String>,
    pub replace_with: serde_json::Value,
}

#[derive(ToSchema, Deserialize, Clone)]
pub struct ServerConfigurationFile {
    pub file: String,
    pub parser: ServerConfigurationFileParser,
    pub replace: Vec<ServerConfigurationFileReplacement>,
}

impl ServerConfigurationFile {
    async fn lookup_value(
        server: &crate::server::Server,
        replacement: &serde_json::Value,
    ) -> Option<String> {
        let value = replacement.as_str()?;

        if value.starts_with("{{") && value.ends_with("}}") {
            let variable = value.trim_start_matches("{{").trim_end_matches("}}").trim();

            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                format!(
                    "Looking up variable: {} for server {}",
                    variable, server.uuid
                ),
            );

            let parts: Vec<&str> = variable.split('.').collect();
            if parts.len() >= 3 && parts[0] == "server" {
                let config = server.configuration.read().await;

                match parts[1] {
                    "build" => {
                        if parts.len() >= 3 {
                            match parts[2] {
                                "memory" => return Some(config.build.memory_limit.to_string()),
                                "io" => return Some(config.build.io_weight.to_string()),
                                "cpu" => return Some(config.build.cpu_limit.to_string()),
                                "disk" => return Some(config.build.disk_space.to_string()),
                                "default" if parts.len() >= 4 => match parts[3] {
                                    "port" => {
                                        return Some(config.allocations.default.port.to_string());
                                    }
                                    "ip" => return Some(config.allocations.default.ip.to_string()),
                                    _ => {
                                        crate::logger::log(
                                            crate::logger::LoggerLevel::Error,
                                            format!(
                                                "Unknown server.build.default subpath: {}",
                                                parts[3]
                                            ),
                                        );
                                    }
                                },
                                _ => {
                                    crate::logger::log(
                                        crate::logger::LoggerLevel::Error,
                                        format!("Unknown server.build subpath: {}", parts[2]),
                                    );
                                }
                            }
                        }
                    }
                    "env" => {
                        if parts.len() >= 3 {
                            let env_var = parts[2];
                            if let Some(value) = config.environment.get(env_var) {
                                return if let Some(value_str) = value.as_str() {
                                    Some(value_str.to_string())
                                } else {
                                    Some(value.to_string())
                                };
                            } else {
                                crate::logger::log(
                                    crate::logger::LoggerLevel::Error,
                                    format!("Environment variable not found: {}", env_var),
                                );
                            }
                        }
                    }
                    _ => {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Error,
                            format!("Unknown server section: {}", parts[1]),
                        );
                    }
                }
            }

            crate::logger::log(
                crate::logger::LoggerLevel::Error,
                format!(
                    "Could not resolve variable: {}, returning empty string",
                    variable
                ),
            );

            return Some(String::new());
        }

        crate::logger::log(
            crate::logger::LoggerLevel::Debug,
            format!("Using raw value: {}", value),
        );

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
    pub async fn update_files(
        &self,
        server: &crate::server::Server,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        crate::logger::log(
            crate::logger::LoggerLevel::Info,
            format!(
                "Starting configuration file updates for server {} with {} configuration files",
                server.uuid,
                self.configs.len()
            ),
        );

        if self.configs.is_empty() {
            crate::logger::log(
                crate::logger::LoggerLevel::Info,
                format!(
                    "No configuration files to update for server {}",
                    server.uuid
                ),
            );
            return Ok(());
        }

        for config_file in self.configs.iter() {
            let config = config_file.clone();
            let file_path = config.file.clone();

            let full_path = Path::new(&file_path)
                .strip_prefix("/")
                .unwrap_or(Path::new(&file_path));

            if let Some(parent) = full_path.parent() {
                if !parent.as_os_str().is_empty() {
                    let parent_path = parent.to_string_lossy().to_string();

                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        format!("Checking if parent directory exists: {}", parent_path),
                    );

                    if let Some(safe_path) = server.filesystem.safe_path(&parent_path) {
                        if !safe_path.exists() {
                            crate::logger::log(
                                crate::logger::LoggerLevel::Info,
                                format!("Creating parent directory: {}", safe_path.display()),
                            );

                            match tokio::fs::create_dir_all(&safe_path).await {
                                Ok(_) => {
                                    crate::logger::log(
                                        crate::logger::LoggerLevel::Debug,
                                        format!(
                                            "Successfully created parent directory: {}",
                                            safe_path.display()
                                        ),
                                    );
                                }
                                Err(e) => {
                                    crate::logger::log(
                                        crate::logger::LoggerLevel::Error,
                                        format!(
                                            "Failed to create parent directory {}: {}",
                                            safe_path.display(),
                                            e
                                        ),
                                    );
                                    continue;
                                }
                            }

                            crate::logger::log(
                                crate::logger::LoggerLevel::Debug,
                                format!("Setting ownership for directory: {}", safe_path.display()),
                            );
                            server.filesystem.chown_path(&safe_path).await;
                        } else {
                            crate::logger::log(
                                crate::logger::LoggerLevel::Debug,
                                format!("Parent directory already exists: {}", safe_path.display()),
                            );
                        }
                    } else {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Error,
                            format!(
                                "Could not resolve safe path for parent directory: {}",
                                parent_path
                            ),
                        );
                        continue;
                    }
                }
            }

            let mut file_content = String::new();

            let safe_file_path = match server.filesystem.safe_path(&file_path) {
                Some(path) => path,
                None => {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Error,
                        format!("Could not resolve safe path for file: {}", file_path),
                    );

                    continue;
                }
            };

            if let Ok(metadata) = safe_file_path.symlink_metadata() {
                if !metadata.is_dir() {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        format!("File exists, reading content: {}", safe_file_path.display()),
                    );

                    match tokio::fs::read_to_string(&safe_file_path).await {
                        Ok(content) => {
                            file_content = content;
                            crate::logger::log(
                                crate::logger::LoggerLevel::Debug,
                                format!(
                                    "Successfully read file content ({} bytes)",
                                    file_content.len()
                                ),
                            );
                        }
                        Err(e) => {
                            crate::logger::log(
                                crate::logger::LoggerLevel::Debug,
                                format!("Failed to read file {}: {}", file_path, e),
                            );
                        }
                    }
                } else {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Error,
                        format!(
                            "Path exists but is a directory: {}",
                            safe_file_path.display()
                        ),
                    );
                }
            } else {
                crate::logger::log(
                    crate::logger::LoggerLevel::Debug,
                    format!(
                        "File does not exist, will create new: {}",
                        safe_file_path.display()
                    ),
                );
            }

            let updated_content = match config.parser {
                ServerConfigurationFileParser::Properties => {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        "Using Properties parser".to_string(),
                    );
                    process_properties_file(&file_content, &config, server).await
                }
                ServerConfigurationFileParser::Json => {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        "Using JSON parser".to_string(),
                    );
                    process_json_file(&file_content, &config, server).await
                }
                ServerConfigurationFileParser::Yaml => {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        "Using YAML parser".to_string(),
                    );
                    process_yaml_file(&file_content, &config, server).await
                }
                ServerConfigurationFileParser::Ini => {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        "Using INI parser".to_string(),
                    );
                    process_ini_file(&file_content, &config, server).await
                }
                ServerConfigurationFileParser::Xml => {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        "Using XML parser".to_string(),
                    );
                    process_xml_file(&file_content, &config, server).await
                }
                ServerConfigurationFileParser::File => {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        "Using Plain File parser".to_string(),
                    );
                    process_plain_file(&file_content, &config, server).await
                }
            };

            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                format!(
                    "Finished processing content, writing updated content ({} bytes)",
                    updated_content.len()
                ),
            );

            match tokio::fs::write(&safe_file_path, updated_content.as_bytes()).await {
                Ok(_) => {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        format!(
                            "Successfully wrote content to file: {}",
                            safe_file_path.display()
                        ),
                    );

                    server.filesystem.chown_path(&safe_file_path).await;

                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        format!(
                            "Successfully processed configuration file {} for server {}",
                            file_path, server.uuid
                        ),
                    );
                }
                Err(e) => {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        format!("Failed to write to file {}: {}", file_path, e),
                    );
                }
            }
        }

        crate::logger::log(
            crate::logger::LoggerLevel::Info,
            format!(
                "Completed all configuration file updates for server {}",
                server.uuid
            ),
        );

        Ok(())
    }
}

async fn process_properties_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> String {
    crate::logger::log(
        crate::logger::LoggerLevel::Debug,
        format!(
            "Processing properties file with {} lines",
            content.lines().count()
        ),
    );

    let mut result = Vec::new();
    let mut processed_keys = HashMap::new();

    for (line_num, line) in content.lines().enumerate() {
        let mut updated_line = line.to_string();

        if line.trim().is_empty() || line.trim().starts_with('#') {
            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                format!(
                    "Line {}: Skipping comment or empty line: '{}'",
                    line_num, line
                ),
            );

            result.push(updated_line);
            continue;
        }

        let parts: Vec<&str> = line.splitn(2, '=').collect();
        if parts.len() != 2 {
            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                format!("Line {}: Not a key-value pair: '{}'", line_num, line),
            );

            result.push(updated_line);
            continue;
        }

        let key = parts[0].trim();
        let original_value = parts[1];
        processed_keys.insert(key.to_owned(), true);

        crate::logger::log(
            crate::logger::LoggerLevel::Debug,
            format!(
                "Line {}: Processing key '{}' with value '{}'",
                line_num, key, original_value
            ),
        );

        for replacement in &config.replace {
            if replacement.r#match == key {
                crate::logger::log(
                    crate::logger::LoggerLevel::Debug,
                    format!("Found replacement match for key: {}", key),
                );

                if let Some(if_value) = &replacement.if_value {
                    if original_value != if_value {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Debug,
                            format!(
                                "Value '{}' does not match required if_value '{}', skipping",
                                original_value, if_value
                            ),
                        );
                        continue;
                    }
                }

                if let Some(value) =
                    ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
                {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        format!(
                            "Replacing value for key '{}': '{}' -> '{}'",
                            key, original_value, value
                        ),
                    );

                    updated_line = format!("{}={}", key, value);
                    break;
                }
            }
        }

        result.push(updated_line);
    }

    for replacement in &config.replace {
        if !processed_keys.contains_key(&replacement.r#match) {
            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                format!("Adding missing key: {}", replacement.r#match),
            );

            if let Some(value) =
                ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
            {
                crate::logger::log(
                    crate::logger::LoggerLevel::Debug,
                    format!(
                        "Adding new key-value pair: '{}={}'",
                        replacement.r#match, value
                    ),
                );
                result.push(format!("{}={}", replacement.r#match, value));
            }
        }
    }

    crate::logger::log(
        crate::logger::LoggerLevel::Debug,
        format!(
            "Finished processing properties file, resulting in {} lines",
            result.len()
        ),
    );

    result.join("\n") + "\n"
}

async fn process_json_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> String {
    crate::logger::log(
        crate::logger::LoggerLevel::Debug,
        format!("Processing JSON file with {} bytes", content.len()),
    );

    let mut json: serde_json::Value = if content.trim().is_empty() {
        crate::logger::log(
            crate::logger::LoggerLevel::Debug,
            "Content is empty, starting with empty object".to_string(),
        );
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        match serde_json::from_str(content) {
            Ok(j) => {
                crate::logger::log(
                    crate::logger::LoggerLevel::Debug,
                    "Successfully parsed JSON content".to_string(),
                );
                j
            }
            Err(e) => {
                crate::logger::log(
                    crate::logger::LoggerLevel::Error,
                    format!(
                        "Failed to parse JSON content: {}. Starting with empty object.",
                        e
                    ),
                );
                serde_json::Value::Object(serde_json::Map::new())
            }
        }
    };

    crate::logger::log(
        crate::logger::LoggerLevel::Debug,
        format!("Applying {} replacements to JSON", config.replace.len()),
    );

    for (index, replacement) in config.replace.iter().enumerate() {
        let path_parts: Vec<&str> = replacement.r#match.split('.').collect();

        crate::logger::log(
            crate::logger::LoggerLevel::Debug,
            format!(
                "Processing replacement {}/{}: Path '{}'",
                index + 1,
                config.replace.len(),
                replacement.r#match
            ),
        );

        if let Some(value) =
            ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
        {
            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                format!(
                    "Updating JSON value at path '{}' with value '{}'",
                    replacement.r#match, value
                ),
            );
            update_json_value(&mut json, &path_parts, value);
        }
    }

    match serde_json::to_string_pretty(&json) {
        Ok(json_str) => {
            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                format!("Successfully serialized JSON ({} bytes)", json_str.len()),
            );
            json_str
        }
        Err(e) => {
            crate::logger::log(
                crate::logger::LoggerLevel::Error,
                format!("Failed to serialize JSON: {}", e),
            );
            "{}".to_string()
        }
    }
}

fn update_json_value(json: &mut serde_json::Value, path: &[&str], value: String) {
    if path.is_empty() {
        crate::logger::log(
            crate::logger::LoggerLevel::Debug,
            "Empty path provided to update_json_value, skipping".to_string(),
        );
        return;
    }

    if path.len() == 1 {
        crate::logger::log(
            crate::logger::LoggerLevel::Debug,
            format!("Setting leaf value at path '{}' to '{}'", path[0], value),
        );

        match json {
            serde_json::Value::Object(map) => {
                let val = if value.eq_ignore_ascii_case("true") {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        "Treating value as boolean (true)".to_string(),
                    );
                    serde_json::Value::Bool(true)
                } else if value.eq_ignore_ascii_case("false") {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        "Treating value as boolean (false)".to_string(),
                    );
                    serde_json::Value::Bool(false)
                } else if let Ok(i) = value.parse::<i64>() {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        format!("Treating value as integer: {}", i),
                    );
                    serde_json::Value::Number(serde_json::Number::from(i))
                } else if let Ok(f) = value.parse::<f64>() {
                    if f.fract() != 0.0 {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Debug,
                            format!("Treating value as float: {}", f),
                        );
                        match serde_json::Number::from_f64(f) {
                            Some(n) => serde_json::Value::Number(n),
                            None => {
                                crate::logger::log(
                                    crate::logger::LoggerLevel::Debug,
                                    "Failed to convert float to JSON number, using string"
                                        .to_string(),
                                );
                                serde_json::Value::String(value)
                            }
                        }
                    } else {
                        crate::logger::log(
                            crate::logger::LoggerLevel::Debug,
                            "Float has no fractional part, treating as string".to_string(),
                        );
                        serde_json::Value::String(value)
                    }
                } else {
                    crate::logger::log(
                        crate::logger::LoggerLevel::Debug,
                        "Treating value as string".to_string(),
                    );
                    serde_json::Value::String(value)
                };

                crate::logger::log(
                    crate::logger::LoggerLevel::Debug,
                    format!("Setting JSON object key '{}' to value", path[0]),
                );
                map.insert(path[0].to_string(), val);
            }
            _ => {
                crate::logger::log(
                    crate::logger::LoggerLevel::Debug,
                    "JSON value is not an object, replacing with new object".to_string(),
                );

                let mut map = serde_json::Map::new();
                map.insert(path[0].to_string(), serde_json::Value::String(value));
                *json = serde_json::Value::Object(map);
            }
        }
        return;
    }

    crate::logger::log(
        crate::logger::LoggerLevel::Debug,
        format!("Navigating to nested path: {}", path[0]),
    );

    match json {
        serde_json::Value::Object(map) => {
            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                format!("Found object at path '{}', navigating deeper", path[0]),
            );
            let entry = map.entry(path[0].to_string()).or_insert_with(|| {
                crate::logger::log(
                    crate::logger::LoggerLevel::Debug,
                    format!("Creating new object for key '{}'", path[0]),
                );
                serde_json::Value::Object(serde_json::Map::new())
            });
            update_json_value(entry, &path[1..], value);
        }
        _ => {
            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                format!(
                    "Value at '{}' is not an object, replacing with new object hierarchy",
                    path[0]
                ),
            );

            let mut map = serde_json::Map::new();
            let mut new_value = serde_json::Value::Object(serde_json::Map::new());
            update_json_value(&mut new_value, &path[1..], value);
            map.insert(path[0].to_string(), new_value);
            *json = serde_json::Value::Object(map);
        }
    }
}

async fn process_yaml_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> String {
    crate::logger::log(
        crate::logger::LoggerLevel::Debug,
        format!("Processing YAML file with {} bytes", content.len()),
    );

    let mut json = if content.trim().is_empty() {
        crate::logger::log(
            crate::logger::LoggerLevel::Debug,
            "Content is empty, starting with empty object".to_string(),
        );
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        match serde_json::from_str(content) {
            Ok(j) => {
                crate::logger::log(
                    crate::logger::LoggerLevel::Debug,
                    "Successfully parsed YAML content as JSON".to_string(),
                );
                j
            }
            Err(e) => {
                crate::logger::log(
                    crate::logger::LoggerLevel::Error,
                    format!(
                        "Failed to parse YAML content as JSON: {}. Starting with empty document.",
                        e
                    ),
                );
                serde_json::Value::Object(serde_json::Map::new())
            }
        }
    };

    crate::logger::log(
        crate::logger::LoggerLevel::Debug,
        format!("Applying {} replacements to YAML", config.replace.len()),
    );

    for (index, replacement) in config.replace.iter().enumerate() {
        let path_parts: Vec<&str> = replacement.r#match.split('.').collect();

        crate::logger::log(
            crate::logger::LoggerLevel::Debug,
            format!(
                "Processing replacement {}/{}: Path '{}'",
                index + 1,
                config.replace.len(),
                replacement.r#match
            ),
        );

        if let Some(value) =
            ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
        {
            crate::logger::log(
                crate::logger::LoggerLevel::Debug,
                format!(
                    "Updating YAML value at path '{}' with value '{}'",
                    replacement.r#match, value
                ),
            );
            update_json_value(&mut json, &path_parts, value);
        }
    }

    match serde_json::to_string_pretty(&json) {
        Ok(yaml_str) => yaml_str,
        Err(e) => {
            crate::logger::log(
                crate::logger::LoggerLevel::Error,
                format!("Failed to serialize YAML: {}", e),
            );
            "{}\n".to_string()
        }
    }
}

async fn process_ini_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> String {
    let mut lines = Vec::new();
    let mut sections = HashMap::new();
    let mut current_section = String::new();
    let mut processed_keys = HashMap::new();

    for line in content.lines() {
        if line.trim().is_empty() || line.starts_with(';') || line.starts_with('#') {
            lines.push(line.to_string());
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            current_section = line[1..line.len() - 1].to_string();
            lines.push(line.to_string());
            continue;
        }

        let parts: Vec<&str> = line.splitn(2, '=').collect();
        if parts.len() == 2 {
            let key = parts[0].trim();
            let full_key = if current_section.is_empty() {
                key.to_string()
            } else {
                format!("{}.{}", current_section, key)
            };

            processed_keys.insert(full_key.clone(), lines.len());
            lines.push(line.to_string());
        } else {
            lines.push(line.to_string());
        }
    }

    for replacement in &config.replace {
        let parts: Vec<&str> = replacement.r#match.splitn(2, '.').collect();

        if let Some(value) =
            ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
        {
            if parts.len() == 2 {
                let section = parts[0];
                let key = parts[1];
                let full_key = replacement.r#match.clone();

                if let Some(line_idx) = processed_keys.get(&full_key) {
                    lines[*line_idx] = format!("{}={}", key, value);
                } else {
                    if !sections.contains_key(section) {
                        sections.insert(section.to_string(), true);
                        lines.push(format!("[{}]", section));
                    }
                    lines.push(format!("{}={}", key, value));
                }
            } else {
                let key = parts[0];

                if let Some(line_idx) = processed_keys.get(key) {
                    lines[*line_idx] = format!("{}={}", key, value);
                } else {
                    lines.push(format!("{}={}", key, value));
                }
            }
        }
    }

    lines.join("\n") + "\n"
}

async fn process_xml_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> String {
    let mut xml_content = if content.trim().is_empty() {
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<root>\n</root>".to_string()
    } else {
        content.to_string()
    };

    for replacement in &config.replace {
        if let Some(value) =
            ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
        {
            let parts: Vec<&str> = replacement
                .r#match
                .split('/')
                .filter(|p| !p.is_empty())
                .collect();
            if parts.is_empty() {
                continue;
            }

            let tag_name = parts.last().unwrap();
            let start_tag = format!("<{}>", tag_name);
            let end_tag = format!("</{}>", tag_name);

            if xml_content.contains(&start_tag) && xml_content.contains(&end_tag) {
                if let Some(start_pos) = xml_content.find(&start_tag) {
                    let tag_end = start_pos + start_tag.len();
                    if let Some(end_pos) = xml_content[tag_end..].find(&end_tag) {
                        let full_end = tag_end + end_pos;
                        xml_content = format!(
                            "{}{}{}{}",
                            &xml_content[..tag_end],
                            value,
                            &xml_content[full_end..full_end],
                            &xml_content[full_end..]
                        );
                    }
                }
            } else if parts.len() == 1 {
                if let Some(root_end_idx) = xml_content.rfind("</root>") {
                    xml_content.insert_str(
                        root_end_idx,
                        &format!("\n  <{}>{}</{}>\n", tag_name, value, tag_name),
                    );
                }
            }
        }
    }

    xml_content
}

async fn process_plain_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> String {
    let mut result = Vec::new();
    let mut processed_matches = HashMap::new();

    for line in content.lines() {
        let mut updated_line = line.to_string();

        for replacement in &config.replace {
            if line.trim().starts_with(&replacement.r#match) {
                processed_matches.insert(replacement.r#match.clone(), true);

                if let Some(if_value) = &replacement.if_value {
                    if !line.contains(if_value) {
                        continue;
                    }
                }

                if let Some(value) =
                    ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
                {
                    updated_line = value;
                    break;
                }
            }
        }

        result.push(updated_line);
    }

    for replacement in &config.replace {
        if !processed_matches.contains_key(&replacement.r#match) {
            if let Some(value) =
                ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
            {
                result.push(value);
            }
        }
    }

    result.join("\n") + "\n"
}
