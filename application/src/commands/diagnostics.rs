use clap::ArgMatches;
use colored::Colorize;
use dialoguer::{Confirm, theme::ColorfulTheme};
use serde::Deserialize;
use std::{fmt::Write, path::Path, sync::Arc};
use tokio::{fs::File, io::AsyncBufReadExt};

pub async fn diagnostics(matches: &ArgMatches, config: Option<&Arc<crate::config::Config>>) -> i32 {
    let log_lines = *matches.get_one::<usize>("log_lines").unwrap();

    let config = match config {
        Some(config) => config,
        None => {
            eprintln!("{}", "no config found".red());
            return 1;
        }
    };

    let include_endpoints = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("do you want to include endpoints (i.e. the FQDN/IP of your panel)?")
        .default(false)
        .interact()
        .unwrap();
    let review_before_upload = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("do you want to review the collected data before uploading to pastes.dev?")
        .default(true)
        .interact()
        .unwrap();

    let versions = bollard::Docker::connect_with_defaults();
    let versions = match versions {
        Ok(client) => client.version().await.unwrap_or_default(),
        Err(_) => Default::default(),
    };

    let mut output = String::with_capacity(1024);
    writeln!(output, "wings-rs - diagnostics report").unwrap();

    write_header(&mut output, "versions");
    write_line(
        &mut output,
        "wings-rs",
        &format!("{}:{}", crate::VERSION, crate::GIT_COMMIT),
    );
    write_line(
        &mut output,
        "docker",
        &versions.version.unwrap_or_else(|| "unknown".to_string()),
    );
    write_line(
        &mut output,
        "kernel",
        &versions
            .kernel_version
            .unwrap_or_else(|| "unknown".to_string()),
    );
    write_line(
        &mut output,
        "os",
        &versions.os.unwrap_or_else(|| "unknown".to_string()),
    );

    write_header(&mut output, "wings-rs configuration");
    write_line(&mut output, "panel location", &config.remote);
    writeln!(output).unwrap();
    write_line(
        &mut output,
        "internal webserver",
        &format!("{} : {}", config.api.host, config.api.port),
    );
    write_line(
        &mut output,
        "ssl enabled",
        &format!("{}", config.api.ssl.enabled),
    );
    write_line(&mut output, "ssl certificate", &config.api.ssl.cert);
    write_line(&mut output, "ssl key", &config.api.ssl.key);
    writeln!(output).unwrap();
    write_line(
        &mut output,
        "sftp server",
        &format!(
            "{} : {}",
            config.system.sftp.bind_address, config.system.sftp.bind_port
        ),
    );
    write_line(
        &mut output,
        "sftp read-only",
        &format!("{}", config.system.sftp.read_only),
    );
    write_line(
        &mut output,
        "sftp key algorithm",
        &config.system.sftp.key_algorithm,
    );
    write_line(
        &mut output,
        "sftp password auth",
        &format!("{}", !config.system.sftp.disable_password_auth),
    );
    writeln!(output).unwrap();
    write_line(&mut output, "root directory", &config.system.root_directory);
    write_line(&mut output, "logs directory", &config.system.log_directory);
    write_line(&mut output, "data directory", &config.system.data_directory);
    write_line(
        &mut output,
        "archive directory",
        &config.system.archive_directory,
    );
    write_line(
        &mut output,
        "backup directory",
        &config.system.backup_directory,
    );
    writeln!(output).unwrap();
    write_line(&mut output, "username", &config.system.username);
    write_line(
        &mut output,
        "server time",
        &format!("{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")),
    );
    write_line(
        &mut output,
        "timezone",
        &format!("{}", chrono::Local::now().offset()),
    );
    write_line(&mut output, "debug mode", &format!("{}", config.debug));

    write_header(&mut output, "latest wings-rs logs");
    match File::open(Path::new(&config.system.log_directory).join("wings.log")).await {
        Ok(file) => {
            let mut reader = tokio::io::BufReader::new(file);
            let mut all_lines = Vec::new();
            let mut line = String::new();

            while reader.read_line(&mut line).await.unwrap() > 0 {
                if !line.trim().is_empty() {
                    all_lines.push(line.clone());
                }
                line.clear();
            }

            let lines_to_display = if all_lines.len() > log_lines {
                all_lines
                    .iter()
                    .skip(all_lines.len() - log_lines)
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                all_lines
            };

            for line in lines_to_display {
                let mut result_line = String::new();
                let mut chars = line.chars().peekable();

                while let Some(c) = chars.next() {
                    if c == '\u{1b}' {
                        while let Some(&next) = chars.peek() {
                            chars.next();

                            if next.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    } else {
                        result_line.push(c);
                    }
                }

                write!(output, "{result_line}").unwrap();
            }
        }
        Err(err) => {
            writeln!(output, "failed to read wings-rs logs: {err}").unwrap();
        }
    }

    if !include_endpoints {
        output = output
            .replace(&config.remote, "{redacted}")
            .replace(&config.api.host.to_string(), "{redacted}")
            .replace(&config.system.sftp.bind_address.to_string(), "{redacted}");

        if !config.api.ssl.cert.is_empty() {
            output = output.replace(&config.api.ssl.cert, "{redacted}");
        }
        if !config.api.ssl.key.is_empty() {
            output = output.replace(&config.api.ssl.key, "{redacted}");
        }
    }

    println!("{output}");

    if review_before_upload {
        let confirm = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("do you want to upload the diagnostics report to pastes.dev?")
            .default(true)
            .interact()
            .unwrap();

        if !confirm {
            return 0;
        }
    }

    let client = reqwest::Client::new();
    let response: Response = client
        .post("https://api.pastes.dev/post")
        .header(
            "User-Agent",
            format!("wings-rs diagnostics/v{}", crate::VERSION),
        )
        .header("Content-Type", "text/plain")
        .header("Accept", "application/json")
        .body(output)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    #[derive(Deserialize)]
    struct Response {
        key: String,
    }

    println!(
        "uploaded diagnostics report to https://pastes.dev/{}",
        response.key
    );

    0
}

#[inline]
fn write_header(output: &mut String, name: &str) {
    writeln!(output, "\n|\n| {name}").unwrap();
    writeln!(output, "| ------------------------------").unwrap();
}

#[inline]
fn write_line(output: &mut String, name: &str, value: &str) {
    writeln!(output, "{name:>20}: {value}").unwrap();
}
