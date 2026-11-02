//! Names given in Home to a device's services, like an input or a switch,
//! by `aid.iid`. Home keeps a rename on the accessory, so the bridge keeps
//! it here to show it again after a restart. In `homekit/names.json`, which
//! the group shares with the rest of what Home knows.

use crate::config;
use anyhow::Result;
use std::{collections::BTreeMap, fs, path::PathBuf};

const FILE: &str = "names.json";

fn path() -> Result<PathBuf> {
    Ok(config::homekit_dir()?.join(FILE))
}

/// Every name kept. Before this file, they were in the config: read from
/// there the first time.
fn all() -> BTreeMap<String, String> {
    match path().and_then(|path| Ok(fs::read_to_string(path)?)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => config::load()
            .map(|config| config.names)
            .unwrap_or_default(),
    }
}

/// The name Home gave `key`, if it gave one.
pub fn get(key: &str) -> Option<String> {
    all().remove(key)
}

/// Keeps the name Home gave `key`.
pub fn set(key: &str, name: &str) -> Result<()> {
    let mut names = all();
    names.insert(key.to_owned(), name.to_owned());
    write(&names)
}

/// Moves the names a config from before this file kept into it, once.
pub fn adopt() -> Result<()> {
    if path()?.exists() {
        return Ok(());
    }
    let names = all();
    if names.is_empty() {
        return Ok(());
    }
    write(&names)
}

fn write(names: &BTreeMap<String, String>) -> Result<()> {
    let path = path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let next = path.with_extension("json.next");
    fs::write(&next, serde_json::to_vec_pretty(names)?)?;
    fs::rename(next, path)?;
    Ok(())
}
