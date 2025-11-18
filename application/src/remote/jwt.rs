use anyhow::Context;
use hmac::digest::KeyInit;
use jwt::VerifyWithKey;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

#[derive(Deserialize, Serialize)]
pub struct BasePayload {
    #[serde(rename = "iss")]
    pub issuer: String,
    #[serde(rename = "sub")]
    pub subject: Option<String>,
    #[serde(rename = "aud")]
    pub audience: Vec<String>,
    #[serde(rename = "exp")]
    pub expiration_time: Option<i64>,
    #[serde(rename = "nbf")]
    pub not_before: Option<i64>,
    #[serde(rename = "iat")]
    pub issued_at: Option<i64>,
    #[serde(rename = "jti")]
    pub jwt_id: String,
}

impl BasePayload {
    pub async fn validate(&self, client: &JwtClient) -> bool {
        let now = chrono::Utc::now().timestamp();

        if let Some(exp) = self.expiration_time {
            if exp < now {
                return false;
            }
        } else {
            return false;
        }

        if let Some(nbf) = self.not_before
            && nbf > now
        {
            return false;
        }

        if let Some(iat) = self.issued_at {
            if iat > now || iat < client.boot_time.timestamp() {
                return false;
            }
        } else {
            return false;
        }

        if let Some(expired_until) = client.denied_jtokens.read().await.get(&self.jwt_id)
            && let Some(issued) = self.issued_at
            && issued < expired_until.timestamp()
        {
            return false;
        }

        true
    }
}

type CountingMap = HashMap<String, (usize, chrono::DateTime<chrono::Utc>)>;

pub struct JwtClient {
    pub key: hmac::Hmac<sha2::Sha256>,
    pub boot_time: chrono::DateTime<chrono::Utc>,
    pub max_jwt_uses: usize,

    pub denied_jtokens: Arc<RwLock<HashMap<String, chrono::DateTime<chrono::Utc>>>>,
    pub seen_jtoken_ids: Arc<RwLock<CountingMap>>,
}

impl JwtClient {
    pub fn new(config: &crate::config::InnerConfig) -> Self {
        let denied_jtokens = Arc::new(RwLock::new(HashMap::new()));
        let seen_jtoken_ids = Arc::new(RwLock::new(HashMap::new()));

        tokio::spawn({
            let denied_jtokens = Arc::clone(&denied_jtokens);
            let seen_jtoken_ids = Arc::clone(&seen_jtoken_ids);

            async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;

                    let mut denied = denied_jtokens.write().await;
                    denied.retain(|_, &mut expiration| {
                        expiration > chrono::Utc::now() - chrono::Duration::hours(1)
                    });
                    drop(denied);

                    let mut seen = seen_jtoken_ids.write().await;
                    seen.retain(|_, &mut (_, expiration)| {
                        expiration > chrono::Utc::now() - chrono::Duration::hours(1)
                    });
                    drop(seen);
                }
            }
        });

        Self {
            key: hmac::Hmac::new_from_slice(config.token.as_bytes())
                .context("invalid token while constructing jwt client")
                .unwrap(),
            boot_time: chrono::Utc::now(),
            max_jwt_uses: config.api.max_jwt_uses,

            denied_jtokens,
            seen_jtoken_ids,
        }
    }

    #[inline]
    pub fn verify<T: DeserializeOwned>(&self, token: &str) -> Result<T, jwt::Error> {
        token.verify_with_key(&self.key)
    }

    pub async fn limited_jwt_id(&self, id: &str) -> bool {
        let seen = self.seen_jtoken_ids.read().await;
        if let Some((count, _)) = seen.get(id) {
            if *count >= self.max_jwt_uses {
                return false;
            } else {
                drop(seen);

                if self.max_jwt_uses != 0 {
                    let mut seen = self.seen_jtoken_ids.write().await;
                    if let Some((count, _)) = seen.get_mut(id) {
                        *count += 1;
                    }
                }
            }
        } else {
            drop(seen);

            self.seen_jtoken_ids
                .write()
                .await
                .insert(id.to_string(), (1, chrono::Utc::now()));
        }

        true
    }

    pub async fn deny(&self, id: impl Into<String>) {
        let mut denied = self.denied_jtokens.write().await;
        denied.insert(id.into(), chrono::Utc::now());
    }
}
