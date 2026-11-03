//! Writing hearth's files: each whole or not at all, and readable by this
//! user alone. A file is written beside itself, flushed to disk, and renamed
//! into place, so a crash or a full disk leaves the one before.

use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

/// Puts `bytes` at `path`, mode 0600, making its directory if need be.
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut next = path.as_os_str().to_owned();
    next.push(".next");
    let next = PathBuf::from(next);
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&next)
            .with_context(|| format!("could not write {}", next.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&next, path).with_context(|| format!("could not save {}", path.display()))
}

/// `value` as pretty JSON at `path`.
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut text = serde_json::to_vec_pretty(value)?;
    text.push(b'\n');
    write(path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_file_is_replaced_whole_and_kept_private() {
        let dir = std::env::temp_dir().join(format!("hearth-files-{}", rand::random::<u64>()));
        let path = dir.join("deep").join("a.json");
        write_json(&path, &serde_json::json!({"a": 1})).unwrap();
        write_json(&path, &serde_json::json!({"a": 2})).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\n  \"a\": 2\n}\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Nothing left beside it.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        let _ = fs::remove_dir_all(dir);
    }
}
