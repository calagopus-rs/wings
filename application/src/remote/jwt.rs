use std::{collections::HashMap, sync::Arc};

use hmac::digest::KeyInit;
use jwt::VerifyWithKey;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
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

        if let Some(nbf) = self.not_before {
            if nbf > now {
                return false;
            }
        }

        if let Some(iat) = self.issued_at {
            if iat > now {
                return false;
            }
        } else {
            return false;
        }

        if let Some(expired_until) = client.denied_jtokens.read().await.get(&self.jwt_id) {
            if let Some(expiration) = self.expiration_time {
                if expiration < expired_until.timestamp() {
                    return false;
                }
            }
        }

        true
    }
}

type CountingMap = HashMap<String, (u8, chrono::DateTime<chrono::Utc>)>;

pub struct JwtClient {
    pub key: hmac::Hmac<sha2::Sha256>,

    pub denied_jtokens: Arc<RwLock<HashMap<String, chrono::DateTime<chrono::Utc>>>>,
    pub seen_jtoken_ids: Arc<RwLock<CountingMap>>,
}

impl JwtClient {
    pub fn new(key: &str) -> Self {
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
            key: hmac::Hmac::new_from_slice(key.as_bytes()).unwrap(),

            denied_jtokens,
            seen_jtoken_ids,
        }
    }

    pub fn verify<T: DeserializeOwned>(&self, token: &str) -> Result<T, jwt::Error> {
        token.verify_with_key(&self.key)
    }

    pub async fn one_time_id(&self, id: &str) -> bool {
        let seen = self.seen_jtoken_ids.read().await;
        if let Some((count, _)) = seen.get(id) {
            if *count >= 2 {
                return false;
            } else {
                drop(seen);

                let mut seen = self.seen_jtoken_ids.write().await;
                if let Some((count, _)) = seen.get_mut(id) {
                    *count += 1;
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

    pub async fn deny(&self, id: &str) {
        let mut denied = self.denied_jtokens.write().await;
        denied.insert(
            id.to_string(),
            chrono::Utc::now() + chrono::Duration::minutes(15),
        );
    }
}
