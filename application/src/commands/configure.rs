use base64::Engine;
use clap::ArgMatches;
use colored::Colorize;
use dialoguer::{Confirm, Input, theme::ColorfulTheme};
use std::sync::Arc;

pub async fn configure(matches: &ArgMatches, config: Option<&Arc<crate::config::Config>>) -> i32 {
    let allow_insecure = *matches.get_one::<bool>("allow_insecure").unwrap();
    let r#override = *matches.get_one::<bool>("override").unwrap();

    let panel_url = matches.get_one::<String>("panel_url");
    let join_data = matches.get_one::<String>("join_data");
    let token = matches.get_one::<String>("token");
    let node = matches.get_one::<usize>("node");

    let config_path = matches.get_one::<String>("config").unwrap();

    if config.is_some() && !r#override {
        let confirm = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("do you want to override the current configuration?")
            .default(false)
            .interact()
            .unwrap();

        if !confirm {
            return 1;
        }
    }

    if let Some(join_data) = join_data {
        let decoding_engine = base64::engine::general_purpose::GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            Default::default(),
        );
        let decoded = match decoding_engine.decode(join_data) {
            Ok(decoded) => decoded,
            Err(_) => {
                eprintln!("failed to decode join data!");
                return 1;
            }
        };

        let response = match serde_yml::from_slice(&decoded) {
            Ok(response) => response,
            Err(_) => {
                eprintln!("failed to decode join data!");
                return 1;
            }
        };

        crate::config::Config::save_new(config_path, response).unwrap();

        println!("successfully configured wings.");

        0
    } else {
        let panel_url = match panel_url {
            Some(url) => url,
            None => &Input::with_theme(&ColorfulTheme::default())
                .with_prompt("panel url")
                .interact_text()
                .unwrap(),
        };

        let panel_url = match reqwest::Url::parse(panel_url) {
            Ok(url) => url,
            Err(_) => {
                eprintln!("{}", "invalid url".red());
                return 1;
            }
        };

        let token = match token {
            Some(token) => token,
            None => &Input::with_theme(&ColorfulTheme::default())
                .with_prompt("token")
                .interact_text()
                .unwrap(),
        };

        let node = match node {
            Some(node) => *node,
            None => {
                let node = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("node id")
                    .interact_text()
                    .unwrap();
                if node == 0 {
                    eprintln!("{}", "node id cannot be 0".red());
                    return 1;
                }

                node
            }
        };

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(allow_insecure)
            .build()
            .unwrap();
        let response = client
            .get(format!(
                "{}/api/application/nodes/{}/configuration",
                panel_url.to_string().trim_end_matches('/'),
                node
            ))
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.pterodactyl.v1+json")
            .send()
            .await;
        let response = match response {
            Ok(response) => crate::remote::into_json(match response.text().await {
                Ok(text) => text,
                Err(err) => {
                    eprintln!("{} {:#?}", "failed to connect to panel:".red(), err);
                    return 1;
                }
            }),
            Err(err) => {
                eprintln!("{} {:#?}", "failed to connect to panel:".red(), err);
                return 1;
            }
        };
        let response = match response {
            Ok(response) => response,
            Err(err) => {
                eprintln!("{} {:#?}", "failed to get configuration:".red(), err);
                return 1;
            }
        };

        crate::config::Config::save_new(config_path, response).unwrap();

        println!("successfully configured wings.");

        0
    }
}
