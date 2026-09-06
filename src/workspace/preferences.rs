use std::{collections::HashMap, fs, path::{Path, PathBuf}};

use serde::{Deserialize, Serialize};

#[derive(Default, Deserialize, Serialize)]
struct Preferences {
    #[serde(default)]
    pinned_tracks: HashMap<String, String>,
}

fn path(workspace: &Path) -> PathBuf {
    workspace.join(".adbstudio").join("preferences.json")
}

pub fn pinned_track(workspace: &Path, folder: &Path) -> Option<PathBuf> {
    let contents = fs::read_to_string(path(workspace)).ok()?;
    let preferences = serde_json::from_str::<Preferences>(&contents).ok()?;
    let folder = folder.strip_prefix(workspace).ok()?.to_string_lossy();
    preferences
        .pinned_tracks
        .get(folder.as_ref())
        .map(|track| workspace.join(track))
}

pub fn set_pinned_track(workspace: &Path, folder: &Path, track: Option<&Path>) {
    let preferences_path = path(workspace);
    let mut preferences = fs::read_to_string(&preferences_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<Preferences>(&contents).ok())
        .unwrap_or_default();
    let Ok(folder) = folder.strip_prefix(workspace) else {
        return;
    };
    let folder = folder.to_string_lossy().into_owned();
    match track.and_then(|track| track.strip_prefix(workspace).ok()) {
        Some(track) => {
            preferences
                .pinned_tracks
                .insert(folder, track.to_string_lossy().into_owned());
        }
        None => {
            preferences.pinned_tracks.remove(&folder);
        }
    }
    if let Some(directory) = preferences_path.parent() {
        if fs::create_dir_all(directory).is_ok() {
            if let Ok(contents) = serde_json::to_string_pretty(&preferences) {
                let _ = fs::write(preferences_path, contents);
            }
        }
    }
}
