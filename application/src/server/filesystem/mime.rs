use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

pub struct MimeCache<K> {
    entries: Arc<RwLock<HashMap<K, &'static str>>>,

    task: tokio::task::JoinHandle<()>,
}

impl<K: std::hash::Hash + Eq + Copy + Send + Sync + 'static> Default for MimeCache<K> {
    fn default() -> Self {
        let entries = Arc::new(RwLock::new(HashMap::new()));

        Self {
            entries: Arc::clone(&entries),
            task: tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;

                    let mut entries = entries.write().await;
                    if entries.len() >= 20000 {
                        let mut delete_entries = smallvec::SmallVec::<[K; 4096]>::new();

                        for k in entries.keys().take(4096) {
                            delete_entries.push(*k);
                        }
                        for k in delete_entries {
                            entries.remove(&k);
                        }
                    } else if entries.len() >= 10000 {
                        let mut delete_entries = smallvec::SmallVec::<[K; 1024]>::new();

                        for k in entries.keys().take(1024) {
                            delete_entries.push(*k);
                        }
                        for k in delete_entries {
                            entries.remove(&k);
                        }
                    }
                }
            }),
        }
    }
}

impl<K: std::hash::Hash + Eq + Copy + Send + Sync + 'static> MimeCache<K> {
    pub async fn get_mime(&self, key: &K) -> Option<&'static str> {
        self.entries.read().await.get(key).copied()
    }

    pub fn sync_get_mime(&self, key: &K) -> Option<&'static str> {
        self.entries.blocking_read().get(key).copied()
    }

    pub async fn insert_mime(&self, key: K, mime: &'static str) -> &'static str {
        let mut entries = self.entries.write().await;
        if entries.len() >= 20000 {
            let mut delete_entries = smallvec::SmallVec::<[K; 512]>::new();

            for k in entries.keys().take(512) {
                delete_entries.push(*k);
            }
            for k in delete_entries {
                entries.remove(&k);
            }
        }

        entries.insert(key, mime);
        mime
    }

    pub fn sync_insert_mime(&self, key: K, mime: &'static str) -> &'static str {
        let mut entries = self.entries.blocking_write();
        if entries.len() >= 20000 {
            let mut delete_entries = smallvec::SmallVec::<[K; 512]>::new();

            for k in entries.keys().take(512) {
                delete_entries.push(*k);
            }
            for k in delete_entries {
                entries.remove(&k);
            }
        }

        entries.insert(key, mime);
        mime
    }
}

impl<K> Drop for MimeCache<K> {
    fn drop(&mut self) {
        self.task.abort();
    }
}
