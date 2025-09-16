use serde::Deserialize;
use serde_default::DefaultFromSerde;
use std::collections::HashMap;
use std::path::Path;
use utoipa::ToSchema;

#[derive(ToSchema, Deserialize, Clone, Copy)]
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
    #[serde(default)]
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

            tracing::debug!(
                server = %server.uuid,
                "looking up variable: {} for server {}",
                variable, server.uuid
            );

            let parts: Vec<&str> = variable.split('.').collect();
            if parts.len() >= 3 && parts[0] == "server" {
                let config = server.configuration.read().await;

                match parts[1] {
                    "build" => {
                        if parts.len() >= 3 {
                            match parts[2] {
                                "memory" => return Some(config.build.memory_limit.to_string()),
                                "io" => {
                                    return Some(
                                        config
                                            .build
                                            .io_weight
                                            .map_or_else(|| "none".to_string(), |v| v.to_string()),
                                    );
                                }
                                "cpu" => return Some(config.build.cpu_limit.to_string()),
                                "disk" => return Some(config.build.disk_space.to_string()),
                                "default" if parts.len() >= 4 => match parts[3] {
                                    "port" => {
                                        return config
                                            .allocations
                                            .default
                                            .as_ref()
                                            .map(|d| d.port.to_string());
                                    }
                                    "ip" => {
                                        return config
                                            .allocations
                                            .default
                                            .as_ref()
                                            .map(|d| d.ip.to_string());
                                    }
                                    _ => {
                                        tracing::error!(
                                            server = %server.uuid,
                                            "unknown server.build.default subpath: {}",
                                            parts[3]
                                        );
                                    }
                                },
                                _ => {
                                    tracing::error!(
                                        server = %server.uuid,
                                        "unknown server.build subpath: {}",
                                        parts[2]
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
                                tracing::error!(
                                    server = %server.uuid,
                                    "environment variable not found: {}",
                                    env_var
                                );
                            }
                        }
                    }
                    _ => {
                        tracing::error!(
                            server = %server.uuid,
                            "unknown server section: {}",
                            parts[1]
                        );
                    }
                }
            }

            tracing::error!(
                server = %server.uuid,
                "could not resolve variable: {}, returning empty string",
                variable
            );

            return Some(String::new());
        }

        tracing::debug!(
            server = %server.uuid,
            "using raw value: {}",
            value
        );

        Some(value.to_string())
    }
}

nestify::nest! {
    #[derive(ToSchema, Deserialize)]
    pub struct ProcessConfiguration {
        #[serde(default)]
        pub startup: #[derive(ToSchema, Deserialize, Clone, DefaultFromSerde)] pub struct ProcessConfigurationStartup {
            pub done: Option<Vec<String>>,
            #[serde(default)]
            pub strip_ansi: bool,
        },
        #[serde(default)]
        pub stop: #[derive(ToSchema, Deserialize, DefaultFromSerde)] pub struct ProcessConfigurationStop {
            #[serde(default)]
            pub r#type: String,
            pub value: Option<String>,
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
            tracing::info!(
                server = %server.uuid,
                "no configuration files to update"
            );
            return Ok(());
        }

        for config_file in self.configs.iter() {
            let config = config_file.clone();
            let file_path = config.file.clone();

            let full_path = Path::new(&file_path)
                .strip_prefix("/")
                .unwrap_or(Path::new(&file_path));

            if let Some(parent) = full_path.parent()
                && !parent.as_os_str().is_empty()
            {
                tracing::debug!(
                    server = %server.uuid,
                    "checking if parent directory exists: {}",
                    parent.display()
                );

                if server.filesystem.async_metadata(&parent).await.is_err() {
                    tracing::info!(
                        server = %server.uuid,
                        "creating parent directory: {}",
                        parent.display()
                    );

                    match server.filesystem.async_create_dir_all(&parent).await {
                        Ok(_) => {
                            tracing::debug!(
                                server = %server.uuid,
                                "successfully created parent directory: {}",
                                parent.display()
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                server = %server.uuid,
                                "failed to create parent directory {}: {}",
                                parent.display(),
                                e
                            );
                            continue;
                        }
                    }

                    tracing::debug!(
                        server = %server.uuid,
                        "setting ownership for directory: {}",
                        parent.display()
                    );
                    match server.filesystem.chown_path(&parent).await {
                        Ok(_) => {
                            tracing::debug!(
                                server = %server.uuid,
                                "successfully set ownership for directory: {}",
                                parent.display()
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                server = %server.uuid,
                                "failed to set ownership for directory {}: {}",
                                parent.display(),
                                e
                            );
                        }
                    }
                } else {
                    tracing::debug!(
                        server = %server.uuid,
                        "parent directory already exists: {}",
                        parent.display()
                    );
                }
            }

            let mut file_content = String::new();

            if let Ok(metadata) = server.filesystem.async_symlink_metadata(&file_path).await {
                if !metadata.is_dir() {
                    tracing::debug!(
                        server = %server.uuid,
                        "file exists, reading content: {}",
                        file_path
                    );

                    match server.filesystem.async_read_to_string(&file_path).await {
                        Ok(content) => {
                            file_content = content;
                            tracing::debug!(
                                server = %server.uuid,
                                "successfully read file content ({} bytes)",
                                file_content.len()
                            );
                        }
                        Err(e) => {
                            tracing::debug!(
                                server = %server.uuid,
                                "failed to read file {}: {}",
                                file_path, e
                            );
                        }
                    }
                } else {
                    tracing::error!(
                        server = %server.uuid,
                        "path exists but is a directory: {}",
                        file_path
                    );
                }
            } else {
                tracing::debug!(
                    server = %server.uuid,
                    "file does not exist, will create new: {}",
                    file_path
                );
            }

            let updated_content = match config.parser {
                ServerConfigurationFileParser::Properties => {
                    tracing::debug!(
                        server = %server.uuid,
                        "using properties parser"
                    );
                    process_properties_file(&file_content, &config, server).await
                }
                ServerConfigurationFileParser::Json => {
                    tracing::debug!(
                        server = %server.uuid,
                        "using json parser"
                    );
                    process_json_file(&file_content, &config, server).await
                }
                ServerConfigurationFileParser::Yaml => {
                    tracing::debug!(
                        server = %server.uuid,
                        "using yaml parser"
                    );
                    process_yaml_file(&file_content, &config, server).await
                }
                ServerConfigurationFileParser::Ini => {
                    tracing::debug!(
                        server = %server.uuid,
                        "using ini parser"
                    );
                    process_ini_file(&file_content, &config, server).await
                }
                ServerConfigurationFileParser::Xml => {
                    tracing::debug!(
                        server = %server.uuid,
                        "using xml parser"
                    );
                    process_xml_file(&file_content, &config, server).await
                }
                ServerConfigurationFileParser::File => {
                    tracing::debug!(
                        server = %server.uuid,
                        "using plain file parser"
                    );
                    process_plain_file(&file_content, &config, server).await
                }
            };

            tracing::debug!(
                server = %server.uuid,
                "finished processing content, writing updated content ({} bytes)",
                updated_content.len()
            );

            match server
                .filesystem
                .async_write(&full_path, updated_content.as_bytes().to_vec())
                .await
            {
                Ok(_) => {
                    tracing::debug!(
                        server = %server.uuid,
                        "successfully wrote content to file: {}",
                        file_path
                    );

                    match server.filesystem.chown_path(&file_path).await {
                        Ok(_) => {
                            tracing::debug!(
                                server = %server.uuid,
                                "successfully set ownership for file: {}",
                                file_path
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                server = %server.uuid,
                                "failed to set ownership for file {}: {}",
                                file_path, e
                            );
                        }
                    }

                    tracing::debug!(
                        server = %server.uuid,
                        "successfully processed configuration file {} for server {}",
                        file_path, server.uuid
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        server = %server.uuid,
                        "failed to write to file {}: {}",
                        file_path, e
                    );
                }
            }
        }

        tracing::info!(
            server = %server.uuid,
            "completed all configuration file updates for server {}",
            server.uuid
        );

        Ok(())
    }
}

async fn process_properties_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> String {
    tracing::debug!(
        server = %server.uuid,
        "processing properties file with {} lines",
        content.lines().count()
    );

    let mut result = Vec::new();
    let mut processed_keys = HashMap::new();

    for (line_num, line) in content.lines().enumerate() {
        let mut updated_line = line.to_string();

        if line.trim().is_empty() || line.trim().starts_with('#') {
            tracing::debug!(
                server = %server.uuid,
                "line {}: skipping comment or empty line: '{}'",
                line_num, line
            );

            result.push(updated_line);
            continue;
        }

        let parts: Vec<&str> = line.splitn(2, '=').collect();
        if parts.len() != 2 {
            tracing::debug!(
                server = %server.uuid,
                "line {}: not a key-value pair: '{}'",
                line_num, line
            );

            result.push(updated_line);
            continue;
        }

        let key = parts[0].trim();
        let original_value = parts[1];
        processed_keys.insert(key.to_owned(), true);

        tracing::debug!(
            server = %server.uuid,
            "line {}: processing key '{}' with value '{}'",
            line_num, key, original_value
        );

        for replacement in &config.replace {
            if replacement.r#match == key {
                tracing::debug!(
                    server = %server.uuid,
                    "found replacement match for key: {}",
                    key
                );

                if let Some(if_value) = &replacement.if_value
                    && original_value != if_value
                {
                    tracing::debug!(
                        server = %server.uuid,
                        "value '{}' does not match required if_value '{}', skipping",
                        original_value, if_value
                    );
                    continue;
                }

                if let Some(value) =
                    ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
                {
                    tracing::debug!(
                        server = %server.uuid,
                        "replacing value for key '{}': '{}' -> '{}'",
                        key, original_value, value
                    );

                    updated_line = format!("{key}={value}");
                    break;
                }
            }
        }

        result.push(updated_line);
    }

    for replacement in &config.replace {
        if !processed_keys.contains_key(&replacement.r#match) {
            tracing::debug!(
                server = %server.uuid,
                "adding missing key: {}",
                replacement.r#match
            );

            if let Some(value) =
                ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
            {
                tracing::debug!(
                    server = %server.uuid,
                    "adding new key-value pair: '{}={}'",
                    replacement.r#match, value
                );
                result.push(format!("{}={}", replacement.r#match, value));
            }
        }
    }

    tracing::debug!(
        server = %server.uuid,
        "finished processing properties file, resulting in {} lines",
        result.len()
    );

    result.join("\n") + "\n"
}

async fn process_json_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> String {
    tracing::debug!(
        server = %server.uuid,
        "processing json file with {} bytes",
        content.len()
    );

    let mut json: serde_json::Value = if content.trim().is_empty() {
        tracing::debug!(
            server = %server.uuid,
            "content is empty, starting with empty object"
        );
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        match serde_json::from_str(content) {
            Ok(j) => {
                tracing::debug!(
                    server = %server.uuid,
                    "successfully parsed json content"
                );
                j
            }
            Err(e) => {
                tracing::error!(
                    server = %server.uuid,
                    "failed to parse json content: {}. starting with empty object.",
                    e
                );
                serde_json::Value::Object(serde_json::Map::new())
            }
        }
    };

    tracing::debug!(
        server = %server.uuid,
        "applying {} replacements to json",
        config.replace.len()
    );

    for (index, replacement) in config.replace.iter().enumerate() {
        let path_parts: Vec<&str> = replacement.r#match.split('.').collect();

        tracing::debug!(
            server = %server.uuid,
            "processing replacement {}/{}: path '{}'",
            index + 1,
            config.replace.len(),
            replacement.r#match
        );

        if let Some(value) =
            ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
        {
            tracing::debug!(
                server = %server.uuid,
                "updating json value at path '{}' with value '{}'",
                replacement.r#match, value
            );
            update_json_value(&mut json, &path_parts, value, server);
        }
    }

    match serde_json::to_string_pretty(&json) {
        Ok(json_str) => {
            tracing::debug!(
                server = %server.uuid,
                "successfully serialized json ({} bytes)",
                json_str.len()
            );
            json_str
        }
        Err(e) => {
            tracing::error!(
                server = %server.uuid,
                "failed to serialize json: {}",
                e
            );
            "{}".to_string()
        }
    }
}

fn update_json_value(
    json: &mut serde_json::Value,
    path: &[&str],
    value: String,
    server: &crate::server::Server,
) {
    if path.is_empty() {
        tracing::debug!(
            server = %server.uuid,
            "empty path provided to update_json_value, skipping"
        );
        return;
    }

    if path.len() == 1 {
        tracing::debug!(
            server = %server.uuid,
            "setting leaf value at path '{}' to '{}'",
            path[0], value
        );

        match json {
            serde_json::Value::Object(map) => {
                let val = if value.eq_ignore_ascii_case("true") {
                    tracing::debug!(
                        server = %server.uuid,
                        "treating value as boolean (true)"
                    );
                    serde_json::Value::Bool(true)
                } else if value.eq_ignore_ascii_case("false") {
                    tracing::debug!(
                        server = %server.uuid,
                        "treating value as boolean (false)"
                    );
                    serde_json::Value::Bool(false)
                } else if let Ok(i) = value.parse::<i64>() {
                    tracing::debug!(
                        server = %server.uuid,
                        "treating value as integer: {}",
                        i
                    );
                    serde_json::Value::Number(serde_json::Number::from(i))
                } else if let Ok(f) = value.parse::<f64>() {
                    if f.fract() != 0.0 {
                        tracing::debug!(
                            server = %server.uuid,
                            "treating value as float: {}",
                            f
                        );
                        match serde_json::Number::from_f64(f) {
                            Some(n) => serde_json::Value::Number(n),
                            None => {
                                tracing::debug!(
                                    server = %server.uuid,
                                    "failed to convert float to json number, using string"
                                );
                                serde_json::Value::String(value)
                            }
                        }
                    } else {
                        tracing::debug!(
                            server = %server.uuid,
                            "float has no fractional part, treating as string"
                        );
                        serde_json::Value::String(value)
                    }
                } else {
                    tracing::debug!(
                        server = %server.uuid,
                        "treating value as string"
                    );
                    serde_json::Value::String(value)
                };

                tracing::debug!(
                    server = %server.uuid,
                    "setting json object key '{}' to value",
                    path[0]
                );
                map.insert(path[0].to_string(), val);
            }
            _ => {
                tracing::debug!(
                    server = %server.uuid,
                    "json value is not an object, replacing with new object"
                );

                let mut map = serde_json::Map::new();
                map.insert(path[0].to_string(), serde_json::Value::String(value));
                *json = serde_json::Value::Object(map);
            }
        }
        return;
    }

    tracing::debug!(
        server = %server.uuid,
        "navigating to nested path: {}",
        path[0]
    );

    match json {
        serde_json::Value::Object(map) => {
            tracing::debug!(
                server = %server.uuid,
                "found object at path '{}', navigating deeper",
                path[0]
            );
            let entry = map.entry(path[0].to_string()).or_insert_with(|| {
                tracing::debug!(
                    server = %server.uuid,
                    "creating new object for key '{}'",
                    path[0]
                );
                serde_json::Value::Object(serde_json::Map::new())
            });
            update_json_value(entry, &path[1..], value, server);
        }
        _ => {
            tracing::debug!(
                server = %server.uuid,
                "value at '{}' is not an object, replacing with new object hierarchy",
                path[0]
            );

            let mut map = serde_json::Map::new();
            let mut new_value = serde_json::Value::Object(serde_json::Map::new());
            update_json_value(&mut new_value, &path[1..], value, server);
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
    tracing::debug!(
        server = %server.uuid,
        "processing yaml file with {} bytes",
        content.len()
    );

    let mut json = if content.trim().is_empty() {
        tracing::debug!(
            server = %server.uuid,
            "content is empty, starting with empty object"
        );
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        match serde_json::from_str(content) {
            Ok(j) => {
                tracing::debug!(
                    server = %server.uuid,
                    "successfully parsed yaml content as json"
                );
                j
            }
            Err(e) => {
                tracing::error!(
                    server = %server.uuid,
                    "failed to parse yaml content as json: {}. starting with empty document.",
                    e
                );
                serde_json::Value::Object(serde_json::Map::new())
            }
        }
    };

    tracing::debug!(
        server = %server.uuid,
        "applying {} replacements to yaml",
        config.replace.len()
    );

    for (index, replacement) in config.replace.iter().enumerate() {
        let path_parts: Vec<&str> = replacement.r#match.split('.').collect();

        tracing::debug!(
            server = %server.uuid,
            "processing replacement {}/{}: path '{}'",
            index + 1,
            config.replace.len(),
            replacement.r#match
        );

        if let Some(value) =
            ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
        {
            tracing::debug!(
                server = %server.uuid,
                "updating yaml value at path '{}' with value '{}'",
                replacement.r#match, value
            );
            update_json_value(&mut json, &path_parts, value, server);
        }
    }

    match serde_json::to_string_pretty(&json) {
        Ok(yaml_str) => yaml_str,
        Err(e) => {
            tracing::error!(
                server = %server.uuid,
                "failed to serialize yaml: {}",
                e
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
    tracing::debug!(
        server = %server.uuid,
        "processing ini file with {} bytes",
        content.len()
    );

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
                format!("{current_section}.{key}")
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
                    tracing::debug!(
                        server = %server.uuid,
                        "updating existing key '{}' in section '{}'",
                        key, section
                    );
                    lines[*line_idx] = format!("{key}={value}");
                } else {
                    if !sections.contains_key(section) {
                        tracing::debug!(
                            server = %server.uuid,
                            "adding new section: [{}]",
                            section
                        );
                        sections.insert(section.to_string(), true);
                        lines.push(format!("[{section}]"));
                    }
                    tracing::debug!(
                        server = %server.uuid,
                        "adding new key '{}' to section '{}'",
                        key, section
                    );
                    lines.push(format!("{key}={value}"));
                }
            } else {
                let key = parts[0];

                if let Some(line_idx) = processed_keys.get(key) {
                    tracing::debug!(
                        server = %server.uuid,
                        "updating existing key '{}' in root section",
                        key
                    );
                    lines[*line_idx] = format!("{key}={value}");
                } else {
                    tracing::debug!(
                        server = %server.uuid,
                        "adding new key '{}' to root section",
                        key
                    );
                    lines.push(format!("{key}={value}"));
                }
            }
        }
    }

    tracing::debug!(
        server = %server.uuid,
        "finished processing ini file, resulting in {} lines",
        lines.len()
    );

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
            let start_tag = format!("<{tag_name}>");
            let end_tag = format!("</{tag_name}>");

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
            } else if parts.len() == 1
                && let Some(root_end_idx) = xml_content.rfind("</root>")
            {
                xml_content.insert_str(
                    root_end_idx,
                    &format!("\n  <{tag_name}>{value}</{tag_name}>\n"),
                );
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

                if let Some(if_value) = &replacement.if_value
                    && !line.contains(if_value)
                {
                    continue;
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
        if !processed_matches.contains_key(&replacement.r#match)
            && let Some(value) =
                ServerConfigurationFile::lookup_value(server, &replacement.replace_with).await
        {
            result.push(value);
        }
    }

    result.join("\n") + "\n"
}
