use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::display::DisplayPreference;
use crate::fs_util::atomic_write;

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub display_mode: DisplayPreference,
}

fn config_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("could not determine config directory"))?
        .join("aip");
    Ok(dir.join("config.json"))
}

impl Config {
    pub fn load() -> Self {
        config_path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        atomic_write(&path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_default_when_no_file() {
        let config = Config::load();
        assert_eq!(config.display_mode, DisplayPreference::Default);
    }

    #[test]
    fn round_trip_serialization() {
        let config = Config {
            display_mode: DisplayPreference::Left,
        };
        let json = serde_json::to_string(&config).unwrap();
        let loaded: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.display_mode, DisplayPreference::Left);
    }

    #[test]
    fn deserialize_with_missing_field_uses_default() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(config.display_mode, DisplayPreference::Default);
    }

    #[test]
    fn save_and_load_via_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aip").join("config.json");

        // Save
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let config = Config {
            display_mode: DisplayPreference::Used,
        };
        let json = serde_json::to_string_pretty(&config).unwrap();
        atomic_write(&path, &json).unwrap();

        // Load
        let loaded: Config =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.display_mode, DisplayPreference::Used);
    }
}
