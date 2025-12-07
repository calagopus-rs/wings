use super::ServerConfigurationFile;

pub struct IniFileParser;

#[async_trait::async_trait]
impl super::ProcessConfigurationFileParser for IniFileParser {
    async fn process_file(
        content: &str,
        config: &ServerConfigurationFile,
        server: &crate::server::Server,
    ) -> Result<Vec<u8>, anyhow::Error> {
        tracing::debug!(
            server = %server.uuid,
            "processing ini file"
        );

        let mut ini = ini::Ini::load_from_str(content)?;

        for replacement in &config.replace {
            let insert_new = replacement.insert_new.unwrap_or(true);

            let value = ServerConfigurationFile::replace_all_placeholders(
                server,
                &replacement.replace_with,
            )
            .await?;

            let (section_name, key_name) = parse_ini_path(&replacement.r#match);

            if section_name.is_empty() {
                let exists = ini.general_section().contains_key(&key_name);

                if (exists && replacement.update_existing) || (!exists && insert_new) {
                    ini.general_section_mut().insert(key_name, value);
                }
            } else if let Some(section) = ini.section_mut(Some(&section_name)) {
                let exists = section.contains_key(&key_name);

                if (exists && replacement.update_existing) || (!exists && insert_new) {
                    section.insert(key_name, value);
                }
            } else if insert_new {
                ini.with_section(Some(&section_name)).set(key_name, value);
            }
        }

        let mut result = Vec::new();
        ini.write_to(&mut result)?;

        Ok(result)
    }
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
