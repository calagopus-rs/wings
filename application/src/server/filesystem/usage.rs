use std::collections::HashMap;

#[derive(Default)]
pub struct DiskUsage {
    pub size: u64,
    pub entries: HashMap<String, DiskUsage>,
}

impl DiskUsage {
    pub fn get_size(&self, path: &[String]) -> Option<u64> {
        if path.is_empty() {
            return Some(self.size);
        }

        let (name, rest) = (path[0].as_str(), &path[1..]);
        self.entries.get(name).and_then(|usage| {
            if rest.is_empty() {
                Some(usage.size)
            } else {
                usage.get_size(rest)
            }
        })
    }

    pub fn update_size(&mut self, path: &[String], delta: i64) {
        if path.is_empty() {
            return;
        }

        let mut current = self;
        for component in path {
            current = {
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

    pub fn remove_path(&mut self, path: &[String]) -> Option<DiskUsage> {
        if path.is_empty() {
            return None;
        }

        self.recursive_remove(path)
    }

    fn recursive_remove(&mut self, path: &[String]) -> Option<DiskUsage> {
        let name = &path[0];
        if path.len() == 1 {
            if let Some(removed) = self.entries.remove(name) {
                return Some(removed);
            }

            return None;
        }

        if let Some(usage) = self.entries.get_mut(name) {
            if let Some(removed) = usage.recursive_remove(&path[1..]) {
                usage.size = usage.size.saturating_sub(removed.size);

                return Some(removed);
            }
        }

        None
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn add_directory(&mut self, target_path: &[String], source_dir: DiskUsage) -> bool {
        if target_path.is_empty() {
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
