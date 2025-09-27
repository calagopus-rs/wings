use std::{collections::HashMap, path::Path};

#[derive(Default)]
pub struct DiskUsage {
    pub size: u64,
    pub entries: HashMap<String, DiskUsage>,
}

impl DiskUsage {
    pub fn get_size(&self, path: &Path) -> Option<u64> {
        if crate::unlikely(path == Path::new("") || path == Path::new("/")) {
            return Some(self.size);
        }

        let mut current = self;
        for component in path.components() {
            let name = component.as_os_str().to_str()?;
            match current.entries.get(name) {
                Some(usage) => current = usage,
                None => return None,
            }
        }

        Some(current.size)
    }

    pub fn update_size(&mut self, path: &Path, delta: i64) {
        if crate::unlikely(path == Path::new("") || path == Path::new("/")) {
            return;
        }

        let mut current = self;
        for component in path.components() {
            current = {
                if crate::likely(
                    current
                        .entries
                        .contains_key(component.as_os_str().to_str().unwrap()),
                ) {
                    // this is perfectly safe, we have a mutable reference to `current`
                    // and we know the entry exists (using check above)
                    let entry = current
                        .entries
                        .get_mut(component.as_os_str().to_str().unwrap_or_default())
                        .unwrap();

                    if delta >= 0 {
                        entry.size = entry.size.saturating_add(delta as u64);
                    } else {
                        entry.size = entry.size.saturating_sub(delta.unsigned_abs());
                    }

                    entry
                } else {
                    let entry = current
                        .entries
                        .entry(
                            component
                                .as_os_str()
                                .to_str()
                                .unwrap_or_default()
                                .to_string(),
                        )
                        .or_default();

                    if delta >= 0 {
                        entry.size = entry.size.saturating_add(delta as u64);
                    } else {
                        entry.size = entry.size.saturating_sub(delta.unsigned_abs());
                    }

                    entry
                }
            }
        }
    }

    pub fn update_size_slice(&mut self, path: &[String], delta: i64) {
        if crate::unlikely(path.is_empty()) {
            return;
        }

        let mut current = self;
        for component in path {
            current = {
                if current.entries.contains_key(component) {
                    // this is perfectly safe, we have a mutable reference to `current`
                    // and we know the entry exists (using check above)
                    let entry = current.entries.get_mut(component).unwrap();

                    if delta >= 0 {
                        entry.size = entry.size.saturating_add(delta as u64);
                    } else {
                        entry.size = entry.size.saturating_sub(delta.unsigned_abs());
                    }

                    entry
                } else {
                    let entry = current.entries.entry(component.clone()).or_default();

                    if delta >= 0 {
                        entry.size = entry.size.saturating_add(delta as u64);
                    } else {
                        entry.size = entry.size.saturating_sub(delta.unsigned_abs());
                    }

                    entry
                }
            }
        }
    }

    pub fn remove_path(&mut self, path: &Path) -> Option<DiskUsage> {
        if crate::unlikely(path == Path::new("") || path == Path::new("/")) {
            return None;
        }

        self.recursive_remove(
            &path
                .components()
                .map(|c| c.as_os_str().to_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>(),
        )
    }

    fn recursive_remove(&mut self, path: &[String]) -> Option<DiskUsage> {
        let name = &path[0];
        if path.len() == 1 {
            if let Some(removed) = self.entries.remove(name) {
                return Some(removed);
            }

            return None;
        }

        if let Some(usage) = self.entries.get_mut(name)
            && let Some(removed) = usage.recursive_remove(&path[1..])
        {
            usage.size = usage.size.saturating_sub(removed.size);

            return Some(removed);
        }

        None
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn add_directory(&mut self, target_path: &[String], source_dir: DiskUsage) -> bool {
        if crate::unlikely(target_path.is_empty()) {
            return false;
        }

        let (leaf, parents) = target_path.split_last().unwrap();

        let mut current = self;
        for component in parents {
            let entry = current.entries.entry(component.clone()).or_default();

            current.size = current.size.saturating_add(source_dir.size);
            current = entry;
        }

        current.size = current.size.saturating_add(source_dir.size);
        current.entries.insert(leaf.clone(), source_dir);

        true
    }
}
