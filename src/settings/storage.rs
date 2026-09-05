use std::{fs, path::PathBuf};

use super::AppSettings;

pub fn load() -> AppSettings {
    let Some(path) = settings_path() else {
        return AppSettings::default();
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn save(settings: &AppSettings) {
    let Some(path) = settings_path() else {
        return;
    };
    let Some(directory) = path.parent() else {
        return;
    };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    let Ok(contents) = serde_json::to_string_pretty(settings) else {
        return;
    };
    let _ = fs::write(path, contents);
}

fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|directory| directory.join("adb-studio").join("settings.json"))
}