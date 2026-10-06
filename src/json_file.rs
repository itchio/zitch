//! JSON files in zitch's own config directory: settings, saved runs and
//! reports. Missing or unreadable files read as the default, and writes
//! go beside the file and rename over it, so losing power mid-write
//! leaves the old file whole.

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The file's contents, or the default when it is missing or unreadable.
pub fn load<T: DeserializeOwned + Default>(path: &Path) -> T {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
            log::warn!("reading {}: {error}", path.display());
            T::default()
        }),
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("reading {}: {error}", path.display());
            }
            T::default()
        }
    }
}

pub fn save<T: Serialize>(path: &Path, value: &T) {
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.part");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(error) = result {
        log::warn!("saving {}: {error}", path.display());
    }
}
