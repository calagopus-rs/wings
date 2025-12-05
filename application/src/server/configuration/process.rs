use anyhow::{Context, Result};
use serde::Deserialize;
use serde_default::DefaultFromSerde;
use std::{collections::HashMap, fmt::Write, path::Path};
use utoipa::ToSchema;

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
    #[serde(alias = "value")]
    pub replace_with: serde_json::Value,
}

#[derive(ToSchema, Deserialize, Clone, Debug)]
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
    ) -> Result<String> {
        let value = match replacement {
            serde_json::Value::String(s) => s.as_str(),
            serde_json::Value::Number(n) => return Ok(n.to_string()),
            serde_json::Value::Bool(b) => return Ok(b.to_string()),
            serde_json::Value::Null => return Ok(String::new()),
            _ => return Ok(replacement.to_string()),
        };

        if !value.starts_with("{{") || !value.ends_with("}}") {
            return Ok(value.to_string());
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
            return Ok(String::new());
        }

        match parts[0] {
            "server" => Self::lookup_server_variable(server, &parts[1..]).await,
            "config" => Self::lookup_config_variable(&server.app_state.config, &parts[1..]).await,
            _ => {
                tracing::error!(
                    server = %server.uuid,
                    "unknown variable prefix: {}",
                    parts[0]
                );
                Ok(String::new())
            }
        }
    }

    async fn lookup_server_variable(
        server: &crate::server::Server,
        parts: &[&str],
    ) -> Result<String> {
        if parts.is_empty() {
            return Ok(String::new());
        }

        let config = server.configuration.read().await;

        match parts[0] {
            "build" => {
                if parts.len() < 2 {
                    return Ok(String::new());
                }

                match parts[1] {
                    "memory" => Ok(config.build.memory_limit.to_string()),
                    "swap" => Ok(config.build.swap.to_string()),
                    "io" => Ok(config
                        .build
                        .io_weight
                        .map_or_else(|| "500".to_string(), |v| v.to_string())),
                    "cpu" => Ok(config.build.cpu_limit.to_string()),
                    "disk" => Ok(config.build.disk_space.to_string()),
                    "threads" => Ok(config.build.threads.clone().unwrap_or_default()),
                    "default" => {
                        if parts.len() < 3 {
                            return Ok(String::new());
                        }
                        match parts[2] {
                            "port" => Ok(config
                                .allocations
                                .default
                                .as_ref()
                                .map(|d| d.port.to_string())
                                .unwrap_or_default()),
                            "ip" => Ok(config
                                .allocations
                                .default
                                .as_ref()
                                .map(|d| d.ip.to_string())
                                .unwrap_or_default()),
                            _ => {
                                tracing::error!(
                                    server = %server.uuid,
                                    "unknown server.build.default subpath: {}",
                                    parts[2]
                                );
                                Ok(String::new())
                            }
                        }
                    }
                    _ => {
                        tracing::error!(
                            server = %server.uuid,
                            "unknown server.build subpath: {}",
                            parts[1]
                        );
                        Ok(String::new())
                    }
                }
            }
            "env" => {
                if parts.len() < 2 {
                    return Ok(String::new());
                }
                let env_var = parts[1];
                if let Some(value) = config.environment.get(env_var) {
                    Ok(value
                        .as_str()
                        .map_or_else(|| value.to_string(), |v| v.to_string()))
                } else {
                    tracing::warn!(
                        server = %server.uuid,
                        "environment variable not found: {}",
                        env_var
                    );
                    Ok(String::new())
                }
            }
            _ => {
                tracing::error!(
                    server = %server.uuid,
                    "unknown server section: {}",
                    parts[0]
                );
                Ok(String::new())
            }
        }
    }

    async fn lookup_config_variable(
        config: &crate::config::Config,
        parts: &[&str],
    ) -> Result<String> {
        if parts.is_empty() || parts[0] == "token_id" || parts[0] == "token" {
            return Ok(String::new());
        }

        let config_json =
            serde_json::to_value(&**config).context("failed to serialize Wings configuration")?;

        let mut current = &config_json;
        for part in parts {
            match current.get(part) {
                Some(value) => current = value,
                None => {
                    tracing::warn!("config path not found: {}", parts.join("."));
                    return Ok(String::new());
                }
            }
        }

        Ok(match current {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => String::new(),
            _ => current.to_string(),
        })
    }

    async fn replace_all_placeholders(
        server: &crate::server::Server,
        input: &serde_json::Value,
    ) -> Result<String> {
        let input = match input.as_str() {
            Some(s) => s,
            None => return Self::lookup_value(server, input).await,
        };

        let mut result = String::new();
        let mut chars = input.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '{' && chars.peek() == Some(&'{') {
                chars.next();
                let mut placeholder = String::from("{{");
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
                    let value = serde_json::Value::String(placeholder.clone());
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
    pub async fn update_files(&self, server: &crate::server::Server) -> Result<()> {
        tracing::info!(
            server = %server.uuid,
            "starting configuration file updates with {} configuration files",
            self.configs.len()
        );

        if self.configs.is_empty() {
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
                && server.filesystem.async_metadata(&parent).await.is_err()
            {
                tracing::debug!(
                    server = %server.uuid,
                    "creating parent directory: {}",
                    parent.display()
                );

                server
                    .filesystem
                    .async_create_dir_all(&parent)
                    .await
                    .with_context(|| {
                        format!("failed to create parent directory: {}", parent.display())
                    })?;

                server
                    .filesystem
                    .chown_path(&parent)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to set ownership for directory: {}",
                            parent.display()
                        )
                    })?;
            }

            let mut file_content = String::new();
            if let Ok(metadata) = server.filesystem.async_symlink_metadata(&file_path).await
                && !metadata.is_dir()
            {
                file_content = server
                    .filesystem
                    .async_read_to_string(&file_path)
                    .await
                    .unwrap_or_default();
            }

            let updated_content = match config.parser {
                ServerConfigurationFileParser::Properties => {
                    process_properties_file(&file_content, &config, server).await?
                }
                ServerConfigurationFileParser::Json => {
                    process_json_file(&file_content, &config, server).await?
                }
                ServerConfigurationFileParser::Yaml => {
                    process_yaml_file(&file_content, &config, server).await?
                }
                ServerConfigurationFileParser::Ini => {
                    process_ini_file(&file_content, &config, server).await?
                }
                ServerConfigurationFileParser::Xml => {
                    process_xml_file(&file_content, &config, server).await?
                }
                ServerConfigurationFileParser::File => {
                    process_plain_file(&file_content, &config, server).await?
                }
            };

            server
                .filesystem
                .async_write(&full_path, updated_content.as_bytes().to_vec())
                .await
                .with_context(|| format!("failed to write file: {}", file_path))?;

            server
                .filesystem
                .chown_path(&file_path)
                .await
                .with_context(|| format!("failed to set ownership for file: {}", file_path))?;

            tracing::debug!(
                server = %server.uuid,
                "successfully processed configuration file: {}",
                file_path
            );
        }

        tracing::info!(
            server = %server.uuid,
            "completed all configuration file updates"
        );

        Ok(())
    }
}

async fn process_properties_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> Result<String> {
    tracing::debug!(
        server = %server.uuid,
        "processing properties file"
    );

    let mut result = String::new();
    let mut in_header = true;
    let mut properties = HashMap::new();
    let mut property_order = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if in_header && (trimmed.is_empty() || trimmed.starts_with('#')) {
            writeln!(result, "{}", line)?;
            continue;
        }

        in_header = false;

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim().to_string();
            let value = line[eq_pos + 1..].to_string();

            if !properties.contains_key(&key) {
                property_order.push(key.clone());
            }
            properties.insert(key, value);
        }
    }

    for replacement in &config.replace {
        let value =
            ServerConfigurationFile::replace_all_placeholders(server, &replacement.replace_with)
                .await?;

        if let Some(if_value) = &replacement.if_value {
            if let Some(existing) = properties.get(&replacement.r#match) {
                if existing != if_value {
                    tracing::debug!(
                        server = %server.uuid,
                        "skipping replacement for '{}': value '{}' != '{}'",
                        replacement.r#match, existing, if_value
                    );
                    continue;
                }
            } else {
                continue;
            }
        }

        if !properties.contains_key(&replacement.r#match) {
            property_order.push(replacement.r#match.clone());
        }
        properties.insert(replacement.r#match.clone(), value);
    }

    for key in property_order {
        if let Some(value) = properties.get(&key) {
            let escaped_value = escape_property_value(value);
            writeln!(result, "{}={}", key, escaped_value)?;
        }
    }

    Ok(result)
}

fn escape_property_value(value: &str) -> String {
    let mut result = String::new();

    for ch in value.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            ch if ch.is_ascii() && !ch.is_control() => result.push(ch),
            ch => {
                result.push_str(&format!("\\u{:04x}", ch as u32));
            }
        }
    }

    result
}

async fn process_json_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> Result<String> {
    tracing::debug!(
        server = %server.uuid,
        "processing json file"
    );

    let mut json: serde_json::Value = if content.trim().is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(content)
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

                serde_json::from_str(&resolved).unwrap_or(serde_json::Value::String(resolved))
            }
            other => other.clone(),
        };

        update_json_value(&mut json, &path_parts, value);
    }

    Ok(serde_json::to_string_pretty(&json)?)
}

fn update_json_value(json: &mut serde_json::Value, path: &[&str], value: serde_json::Value) {
    if path.is_empty() {
        return;
    }

    if path.len() == 1 {
        match json {
            serde_json::Value::Object(map) => {
                map.insert(path[0].to_string(), value);
            }
            _ => {
                let mut map = serde_json::Map::new();
                map.insert(path[0].to_string(), value);
                *json = serde_json::Value::Object(map);
            }
        }
        return;
    }

    match json {
        serde_json::Value::Object(map) => {
            let entry = map
                .entry(path[0].to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            update_json_value(entry, &path[1..], value);
        }
        _ => {
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
) -> Result<String> {
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
                serde_json::from_str(&resolved).unwrap_or(serde_json::Value::String(resolved))
            }
            other => other.clone(),
        };

        update_json_value(&mut json, &path_parts, value);
    }

    Ok(serde_yml::to_string(&json)?)
}

async fn process_ini_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> Result<String> {
    tracing::debug!(
        server = %server.uuid,
        "processing ini file"
    );

    let mut result = String::new();
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut section_order = Vec::new();
    let mut current_section = String::new();
    let mut root_section = HashMap::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_section = trimmed[1..trimmed.len() - 1].to_string();
            if !sections.contains_key(&current_section) {
                section_order.push(current_section.clone());
                sections.insert(current_section.clone(), HashMap::new());
            }
            continue;
        }

        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim().to_string();
            let value = line[eq_pos + 1..].trim().to_string();

            if current_section.is_empty() {
                root_section.insert(key, value);
            } else if let Some(section) = sections.get_mut(&current_section) {
                section.insert(key, value);
            }
        }
    }

    for replacement in &config.replace {
        let value =
            ServerConfigurationFile::replace_all_placeholders(server, &replacement.replace_with)
                .await?;

        let (section_name, key_name) = parse_ini_path(&replacement.r#match);

        if section_name.is_empty() {
            root_section.insert(key_name, value);
        } else {
            if !sections.contains_key(&section_name) {
                section_order.push(section_name.clone());
                sections.insert(section_name.clone(), HashMap::new());
            }
            if let Some(section) = sections.get_mut(&section_name) {
                section.insert(key_name, value);
            }
        }
    }

    for (key, value) in &root_section {
        writeln!(result, "{}={}", key, value)?;
    }

    for section_name in section_order {
        if let Some(section) = sections.get(&section_name)
            && !section.is_empty()
        {
            writeln!(result, "\n[{}]", section_name)?;
            for (key, value) in section {
                writeln!(result, "{}={}", key, value)?;
            }
        }
    }

    Ok(result)
}

fn parse_ini_path(path: &str) -> (String, String) {
    let mut section = String::new();
    let mut key = String::new();
    let mut bracket_depth = 0;
    let mut in_section = true;

    for ch in path.chars() {
        match ch {
            '[' => {
                bracket_depth += 1;
                if in_section {
                    section.push(ch);
                } else {
                    key.push(ch);
                }
            }
            ']' => {
                bracket_depth -= 1;
                if in_section {
                    section.push(ch);
                } else {
                    key.push(ch);
                }
            }
            '.' => {
                if bracket_depth > 0 {
                    if in_section {
                        section.push(ch);
                    } else {
                        key.push(ch);
                    }
                } else if in_section && !section.is_empty() {
                    in_section = false;
                } else {
                    key.push(ch);
                }
            }
            _ => {
                if in_section {
                    section.push(ch);
                } else {
                    key.push(ch);
                }
            }
        }
    }

    if in_section {
        (String::new(), section)
    } else {
        (section, key)
    }
}

async fn process_xml_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> Result<String> {
    tracing::debug!(
        server = %server.uuid,
        "processing xml file"
    );

    let content = if content.trim().is_empty() {
        r#"<?xml version="1.0" encoding="UTF-8"?><root></root>"#
    } else {
        content
    };

    let mut root = xmltree::Element::parse(content.as_bytes())?;

    for replacement in &config.replace {
        let value =
            ServerConfigurationFile::replace_all_placeholders(server, &replacement.replace_with)
                .await?;

        let path = replacement.r#match.replace('.', "/");
        let path_parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();

        if path.contains('*') {
            update_xml_wildcard(&mut root, &path_parts, &value);
        } else {
            update_xml_element(&mut root, &path_parts, &value);
        }
    }

    let mut xml_bytes = Vec::new();
    root.write_with_config(
        &mut xml_bytes,
        xmltree::EmitterConfig::new()
            .perform_indent(true)
            .indent_string("  "),
    )?;

    Ok(String::from_utf8(xml_bytes)?)
}

fn update_xml_element(element: &mut xmltree::Element, path: &[&str], value: &str) {
    if path.is_empty() {
        return;
    }

    if path.len() == 1 {
        let tag = path[0];

        if let Some(attr_value) = value.strip_prefix('@')
            && let Some(eq_pos) = attr_value.find('=')
        {
            let attr_name = &attr_value[..eq_pos];
            let attr_val = &attr_value[eq_pos + 1..];
            element
                .attributes
                .insert(attr_name.to_string(), attr_val.to_string());
            return;
        }

        if let Some(child) = element.get_mut_child(tag) {
            child.children.clear();
            child
                .children
                .push(xmltree::XMLNode::Text(value.to_string()));
        } else {
            let mut new_child = xmltree::Element::new(tag);
            new_child
                .children
                .push(xmltree::XMLNode::Text(value.to_string()));
            element.children.push(xmltree::XMLNode::Element(new_child));
        }
        return;
    }

    let tag = path[0];
    if let Some(child) = element.get_mut_child(tag) {
        update_xml_element(child, &path[1..], value);
    } else {
        let mut new_child = xmltree::Element::new(tag);
        update_xml_element(&mut new_child, &path[1..], value);
        element.children.push(xmltree::XMLNode::Element(new_child));
    }
}

fn update_xml_wildcard(element: &mut xmltree::Element, path: &[&str], value: &str) {
    if path.is_empty() {
        return;
    }

    let tag = path[0];

    if tag == "*" {
        for child in &mut element.children {
            if let xmltree::XMLNode::Element(child_elem) = child {
                if path.len() == 1 {
                    child_elem.children.clear();
                    child_elem
                        .children
                        .push(xmltree::XMLNode::Text(value.to_string()));
                } else {
                    update_xml_wildcard(child_elem, &path[1..], value);
                }
            }
        }
    } else {
        for child in &mut element.children {
            if let xmltree::XMLNode::Element(child_elem) = child
                && child_elem.name == tag
            {
                if path.len() == 1 {
                    child_elem.children.clear();
                    child_elem
                        .children
                        .push(xmltree::XMLNode::Text(value.to_string()));
                } else {
                    update_xml_wildcard(child_elem, &path[1..], value);
                }
            }
        }
    }
}

async fn process_plain_file(
    content: &str,
    config: &ServerConfigurationFile,
    server: &crate::server::Server,
) -> Result<String> {
    tracing::debug!(
        server = %server.uuid,
        "processing plain file"
    );

    let mut result = String::new();

    for line in content.lines() {
        let mut replaced = false;

        for replacement in &config.replace {
            if line.starts_with(&replacement.r#match) {
                let value = ServerConfigurationFile::replace_all_placeholders(
                    server,
                    &replacement.replace_with,
                )
                .await?;

                writeln!(result, "{}", value)?;
                replaced = true;
                break;
            }
        }

        if !replaced {
            writeln!(result, "{}", line)?;
        }
    }

    Ok(result)
}
