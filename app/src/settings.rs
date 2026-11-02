//! Preferences that survive a relaunch, stored as JSON next to the database.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Scan locations, each with its own history.
    pub locations: Vec<PathBuf>,
    pub location: Option<PathBuf>,
    /// Plot free space instead of used space.
    pub graph_free: bool,
    /// Compute exact recursive churn per folder in the background.
    pub deep_activity: bool,
    /// Suggestions the user hid: (location, kind, path).
    pub hidden: Vec<(String, String, String)>,
    /// Sidebar favorites. `None` until first set up from the usual Finder defaults.
    pub favorites: Option<Vec<PathBuf>>,
    pub show_sidebar: bool,
    pub sidebar_width: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            locations: vec![default_location()],
            location: None,
            graph_free: false,
            deep_activity: true,
            hidden: Vec::new(),
            favorites: None,
            show_sidebar: true,
            sidebar_width: crate::sidebar::DEFAULT_WIDTH,
        }
    }
}

/// The whole startup disk: the scan most people want first.
pub fn default_location() -> PathBuf {
    if cfg!(windows) { PathBuf::from("C:\\") } else { PathBuf::from("/") }
}

impl Settings {
    /// Next to the store, so a `DISKDIFF_DB` demo store gets its own settings.
    fn path() -> PathBuf {
        match std::env::var_os("DISKDIFF_DB") {
            Some(db) => PathBuf::from(db).with_extension("settings.json"),
            None => diskdiff_engine::platform::data_dir().join("settings.json"),
        }
    }

    pub fn load() -> Settings {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, p);
            }
        }
    }
}
