//! Choices remembered between runs, in `settings.json` in zitch's own
//! config directory. They belong to the device, whoever is signed in.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Missing or unknown fields fall back to defaults, so files written by
/// other versions still load.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The "Playable here" filter.
    pub playable_only: bool,
    /// The Collections tab's "Installed" toggle.
    pub collections_installed_only: bool,
    /// Types "Playable here" leaves out, by scanned platform id. Kept as
    /// what is off so a type the device gains later starts out on.
    pub playable_hidden: Vec<String>,
    /// Never ask how a game ran.
    pub reports_off: bool,
    /// Uploads a compatibility report was sent for, which are not asked
    /// about again.
    pub reported_uploads: Vec<i64>,
}

impl Settings {
    /// The saved settings, or the defaults when there are none or the
    /// file cannot be read.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::default();
            }
            Err(error) => {
                log::warn!("reading {}: {error}", path.display());
                return Self::default();
            }
        };
        serde_json::from_str(&text).unwrap_or_else(|error| {
            log::warn!("reading {}: {error}", path.display());
            Self::default()
        })
    }

    /// Writes beside, then renames, so losing power mid-write leaves the
    /// old file whole.
    pub fn save(&self, path: &Path) {
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
            let tmp = path.with_extension("json.part");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(error) = result {
            log::warn!("saving {}: {error}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_settings_load_back() {
        let dir = std::env::temp_dir().join(format!("zitch-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let settings = Settings {
            playable_only: true,
            collections_installed_only: true,
            playable_hidden: vec!["rom:snes".to_string()],
            reports_off: true,
            reported_uploads: vec![12],
        };
        settings.save(&path);
        assert_eq!(Settings::load(&path), settings);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_and_missing_fields_use_defaults() {
        let parsed: Settings =
            serde_json::from_str(r#"{"playable_only": true, "from_the_future": 1}"#).unwrap();
        assert!(parsed.playable_only);
        assert!(!parsed.collections_installed_only);
        assert!(parsed.playable_hidden.is_empty());
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        let path = std::env::temp_dir().join("zitch-settings-missing/settings.json");
        assert_eq!(Settings::load(&path), Settings::default());
    }
}
