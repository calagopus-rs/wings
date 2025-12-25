use crate::remote::AuthenticationType;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

struct Ratelimit {
    password_attempts: usize,
    pubkey_attempts: usize,
    last_attempt: std::time::Instant,
}

impl Ratelimit {
    fn new() -> Self {
        Self {
            password_attempts: 0,
            pubkey_attempts: 0,
            last_attempt: std::time::Instant::now(),
        }
    }
}

pub struct SshRatelimiter {
    password_attempts: usize,
    pubkey_attempts: usize,
    cooldown: u64,

    ratelimits: Arc<Mutex<HashMap<std::net::IpAddr, Ratelimit>>>,
}

impl SshRatelimiter {
    pub fn new(password_attempts: usize, pubkey_attempts: usize, cooldown: u64) -> Self {
        let ratelimits = Arc::new(Mutex::new(HashMap::<std::net::IpAddr, Ratelimit>::new()));

        if cooldown != 0 {
            tokio::spawn({
                let ratelimits = Arc::clone(&ratelimits);

                async move {
                    loop {
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                        let mut ratelimits = ratelimits.lock().await;
                        let now = std::time::Instant::now();
                        ratelimits.retain(|_, ratelimit| {
                            now.duration_since(ratelimit.last_attempt).as_secs() < cooldown
                        });
                    }
                }
            });
        }

        Self {
            password_attempts,
            pubkey_attempts,
            cooldown,
            ratelimits,
        }
    }

    pub async fn check_attempt(
        &self,
        ip: std::net::IpAddr,
        authentication_type: AuthenticationType,
    ) -> Result<(), russh::Error> {
        if self.cooldown == 0 {
            return Ok(());
        }

        let mut ratelimits = self.ratelimits.lock().await;
        let entry = ratelimits.entry(ip).or_insert(Ratelimit::new());

        if match authentication_type {
            AuthenticationType::Password => {
                entry.password_attempts += 1;
                entry.last_attempt = std::time::Instant::now();
                entry.password_attempts > self.password_attempts
            }
            AuthenticationType::PublicKey => {
                entry.pubkey_attempts += 1;
                entry.last_attempt = std::time::Instant::now();
                entry.pubkey_attempts > self.pubkey_attempts
            }
        } {
            Err(russh::Error::Disconnect)
        } else {
            Ok(())
        }
    }

    pub async fn finish_attempt(
        &self,
        ip: &std::net::IpAddr,
        authentication_type: AuthenticationType,
    ) {
        if self.cooldown == 0 {
            return;
        }

        let mut ratelimits = self.ratelimits.lock().await;
        if let Some(entry) = ratelimits.get_mut(ip) {
            match authentication_type {
                AuthenticationType::Password => {
                    if entry.password_attempts > 0 {
                        entry.password_attempts -= 1;
                    }
                }
                AuthenticationType::PublicKey => {
                    if entry.pubkey_attempts > 0 {
                        entry.pubkey_attempts -= 1;
                    }
                }
            }
        }
    }
}
