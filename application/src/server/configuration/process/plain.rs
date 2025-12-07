use super::ServerConfigurationFile;
use std::{collections::HashSet, fmt::Write};

pub struct PlainFileParser;

#[async_trait::async_trait]
impl super::ProcessConfigurationFileParser for PlainFileParser {
    async fn process_file(
        content: &str,
        config: &ServerConfigurationFile,
        server: &crate::server::Server,
    ) -> Result<Vec<u8>, anyhow::Error> {
        tracing::debug!(
            server = %server.uuid,
            "processing plain file"
        );

        let mut result = String::new();
        let mut found_matches = HashSet::new();

        for line in content.lines() {
            let mut replaced = false;

            for replacement in &config.replace {
                if !line.starts_with(&replacement.r#match) {
                    continue;
                }

                let value = ServerConfigurationFile::replace_all_placeholders(
                    server,
                    &replacement.replace_with,
                )
                .await?;

                if replacement.update_existing {
                    writeln!(result, "{}", value)?;
                    replaced = true;
                }
                found_matches.insert(&replacement.r#match);

                break;
            }

            if !replaced {
                writeln!(result, "{}", line)?;
            }
        }

        for replacement in &config.replace {
            let insert_new = replacement.insert_new.unwrap_or(false);

            if found_matches.contains(&replacement.r#match) || !insert_new {
                continue;
            }

            let value = ServerConfigurationFile::replace_all_placeholders(
                server,
                &replacement.replace_with,
            )
            .await?;

            writeln!(result, "{}", value)?;
        }

        Ok(result.into_bytes())
    }
}
