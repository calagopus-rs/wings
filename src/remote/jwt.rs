use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use hmac::digest::KeyInit;
use jwt::VerifyWithKey;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

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
    pub fn validate(&self, client: &JwtClient) -> bool {
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

        if let Some(expiration) = client.denied_jtokens.read().unwrap().get(&self.jwt_id) {
            if *expiration > chrono::Utc::now() {
                return false;
            }
        }

        true
    }
}

pub struct JwtClient {
    pub key: hmac::Hmac<sha2::Sha256>,

    pub denied_jtokens: Arc<RwLock<HashMap<String, chrono::DateTime<chrono::Utc>>>>,
    pub seen_jtoken_ids: Arc<RwLock<HashSet<String>>>,
}

impl JwtClient {
    pub fn new(key: &str) -> Self {
        Self {
            key: hmac::Hmac::new_from_slice(key.as_bytes()).unwrap(),

            denied_jtokens: Arc::new(RwLock::new(HashMap::new())),
            seen_jtoken_ids: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    pub fn verify<T: DeserializeOwned>(&self, token: &str) -> Result<T, jwt::Error> {
        token.verify_with_key(&self.key)
    }

    pub fn one_time_id(&self, id: &str) -> bool {
        let seen = self.seen_jtoken_ids.read().unwrap();
        if seen.contains(&id.to_string()) {
            return false;
        }
        drop(seen);

        self.seen_jtoken_ids.write().unwrap().insert(id.to_string());

        true
    }

    pub fn deny(&self, id: &str) {
        let mut denied = self.denied_jtokens.write().unwrap();
        denied.insert(id.to_string(), chrono::Utc::now());
    }
}
