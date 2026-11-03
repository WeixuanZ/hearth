//! The HomeKit directory every member keeps: the bridge's keys, Home's
//! pairings, its ids and names, and Matter's fabrics, swapped whole.

use super::*;

/// The HomeKit directory: the bridge's keys and Home's pairings.
pub(super) struct Files {
    pub(super) hash: String,
    pub(super) files: BTreeMap<String, Vec<u8>>,
}
impl Files {
    pub(super) fn read() -> Result<Self> {
        let root = config::homekit_dir()?;
        let mut files = BTreeMap::new();
        let mut dirs = vec![root.clone()];
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if let Ok(name) = path.strip_prefix(&root) {
                    files.insert(name.to_string_lossy().into_owned(), fs::read(&path)?);
                }
            }
        }
        let mut hash = Sha256::new();
        for (name, bytes) in &files {
            hash.update((name.len() as u64).to_be_bytes());
            hash.update(name);
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }
        let hash = if files.is_empty() {
            String::new()
        } else {
            crypto::encode(&hash.finalize())
        };
        Ok(Self { hash, files })
    }

    pub(super) fn encoded(self) -> BTreeMap<String, String> {
        self.files
            .into_iter()
            .map(|(name, bytes)| (name, crypto::encode(&bytes)))
            .collect()
    }

    /// Replaces the directory at once, so it is never half copied.
    pub(super) fn install(files: &BTreeMap<String, String>) -> Result<()> {
        let dir = config::homekit_dir()?;
        let next = dir.with_extension("next");
        let old = dir.with_extension("old");
        let _ = fs::remove_dir_all(&next);
        for (name, bytes) in files {
            let name = Path::new(name);
            anyhow::ensure!(
                name.components()
                    .all(|part| matches!(part, Component::Normal(_))),
                "a HomeKit file outside its directory"
            );
            let path = next.join(name);
            fs::create_dir_all(path.parent().unwrap_or(&next))?;
            fs::write(path, crypto::decode(bytes)?)?;
        }
        fs::create_dir_all(&next)?;
        let _ = fs::remove_dir_all(&old);
        if dir.exists() {
            fs::rename(&dir, &old)?;
        }
        fs::rename(&next, &dir)?;
        let _ = fs::remove_dir_all(&old);
        Ok(())
    }
}
