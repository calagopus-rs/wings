use compact_str::ToCompactString;
use std::path::Path;

#[derive(Default, Clone, Copy)]
pub struct UsedSpace {
    real: u64,
    apparent: u64,
}

impl UsedSpace {
    #[inline]
    pub fn get_real(&self) -> u64 {
        self.real
    }

    #[inline]
    pub fn set_real(&mut self, val: u64) {
        self.real = val;
    }

    #[inline]
    pub fn sub_real(&mut self, val: u64) {
        let real = self.get_real();
        self.set_real(real.saturating_sub(val));
    }

    #[inline]
    pub fn add_real(&mut self, val: u64) {
        let real = self.get_real();
        self.set_real(real.saturating_add(val));
    }

    #[inline]
    pub fn get_apparent(&self) -> u64 {
        self.apparent
    }

    #[inline]
    pub fn set_apparent(&mut self, val: u64) {
        self.apparent = val;
    }

    #[inline]
    pub fn sub_apparent(&mut self, val: u64) {
        let real = self.get_apparent();
        self.set_apparent(real.saturating_sub(val));
    }

    #[inline]
    pub fn add_apparent(&mut self, val: u64) {
        let real = self.get_apparent();
        self.set_apparent(real.saturating_add(val));
    }
}

#[derive(Default, Clone, Copy)]
pub struct SpaceDelta {
    pub real: i64,
    pub apparent: i64,
}

impl From<i64> for SpaceDelta {
    #[inline]
    fn from(value: i64) -> Self {
        SpaceDelta {
            real: value,
            apparent: value,
        }
    }
}

impl From<(i64, i64)> for SpaceDelta {
    #[inline]
    fn from(value: (i64, i64)) -> Self {
        SpaceDelta {
            real: value.0,
            apparent: value.1,
        }
    }
}

#[derive(Default)]
pub struct DiskUsage {
    pub space: UsedSpace,
    entries: thin_vec::ThinVec<(compact_str::CompactString, DiskUsage)>,
}

impl DiskUsage {
    fn upsert_entry(&mut self, key: &str) -> &mut DiskUsage {
        match self.entries.binary_search_by(|a| a.0.as_str().cmp(key)) {
            Ok(idx) => &mut self.entries[idx].1,
            Err(idx) => {
                self.entries
                    .insert(idx, (key.to_compact_string(), DiskUsage::default()));
                &mut self.entries[idx].1
            }
        }
    }

    fn get_entry(&mut self, key: &str) -> Option<&mut DiskUsage> {
        if let Ok(idx) = self.entries.binary_search_by(|a| a.0.as_str().cmp(key)) {
            Some(&mut self.entries[idx].1)
        } else {
            None
        }
    }

    fn remove_entry(&mut self, key: &str) -> Option<DiskUsage> {
        if let Ok(idx) = self.entries.binary_search_by(|a| a.0.as_str().cmp(key)) {
            Some(self.entries.remove(idx).1)
        } else {
            None
        }
    }

    #[inline]
    pub fn get_entries(&self) -> &[(compact_str::CompactString, DiskUsage)] {
        &self.entries
    }

    pub fn get_size(&self, path: &Path) -> Option<UsedSpace> {
        if crate::unlikely(path == Path::new("") || path == Path::new("/")) {
            return Some(self.space);
        }

        let mut current = self;
        for component in path.components() {
            let name = component.as_os_str().to_str()?;
            let idx = current
                .entries
                .binary_search_by(|(n, _)| n.as_str().cmp(name))
                .ok()?;
            current = &current.entries[idx].1;
        }

        Some(current.space)
    }

    pub fn update_size(&mut self, path: &Path, delta: SpaceDelta) {
        if crate::unlikely(path == Path::new("") || path == Path::new("/")) {
            return;
        }

        let mut current = self;
        for component in path.components() {
            let key = component.as_os_str().to_str().unwrap_or_default();
            let entry = current.upsert_entry(key);

            if delta.real >= 0 {
                entry.space.add_real(delta.real as u64);
            } else {
                entry.space.sub_real(delta.real.unsigned_abs());
            }
            if delta.apparent >= 0 {
                entry.space.add_apparent(delta.apparent as u64);
            } else {
                entry.space.sub_apparent(delta.apparent.unsigned_abs());
            }

            current = entry;
        }
    }

    pub fn update_size_iterator(
        &mut self,
        path: impl IntoIterator<Item = impl AsRef<str>>,
        delta: SpaceDelta,
    ) {
        let mut current = self;
        for component in path {
            let entry = current.upsert_entry(component.as_ref());

            if delta.real >= 0 {
                entry.space.add_real(delta.real as u64);
            } else {
                entry.space.sub_real(delta.real.unsigned_abs());
            }
            if delta.apparent >= 0 {
                entry.space.add_apparent(delta.apparent as u64);
            } else {
                entry.space.sub_apparent(delta.apparent.unsigned_abs());
            }

            current = entry;
        }
    }

    pub fn remove_path(&mut self, path: &Path) -> Option<DiskUsage> {
        if crate::unlikely(path == Path::new("") || path == Path::new("/")) {
            return None;
        }

        self.recursive_remove(&mut path.components())
    }

    fn recursive_remove<'a>(
        &mut self,
        components: &mut impl Iterator<Item = std::path::Component<'a>>,
    ) -> Option<DiskUsage> {
        let component = components.next()?;
        let name = component.as_os_str().to_str().unwrap_or_default();

        if components.size_hint().0 == 0 {
            return self.remove_entry(name);
        }

        if let Some(child) = self.get_entry(name)
            && let Some(removed) = child.recursive_remove(components)
        {
            self.space.sub_real(removed.space.get_real());
            self.space.sub_apparent(removed.space.get_apparent());
            return Some(removed);
        }

        None
    }

    #[inline]
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn add_directory(
        &mut self,
        target_path: &[impl AsRef<str>],
        source_dir: DiskUsage,
    ) -> bool {
        if crate::unlikely(target_path.is_empty()) {
            return false;
        }

        let Some((leaf, parents)) = target_path.split_last() else {
            return false;
        };

        let mut current = self;
        for component in parents {
            current.space.add_real(source_dir.space.get_real());
            current.space.add_apparent(source_dir.space.get_apparent());

            current = current.upsert_entry(component.as_ref());
        }

        current.space.add_real(source_dir.space.get_real());
        current.space.add_apparent(source_dir.space.get_apparent());
        *current.upsert_entry(leaf.as_ref()) = source_dir;

        true
    }
}
