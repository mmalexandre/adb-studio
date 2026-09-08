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

#[cfg(test)]
mod tests {
    use super::{pinned_track, set_pinned_track};
    use std::{fs, path::PathBuf, time::{SystemTime, UNIX_EPOCH}};

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("adb-studio-preferences-{suffix}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn pinned_track_round_trips_and_can_be_removed() {
        let temp = TempDirectory::new();
        let folder = temp.0.join("album");
        let track = folder.join("song.wav");
        fs::create_dir(&folder).unwrap();
        fs::write(&track, []).unwrap();

        assert_eq!(pinned_track(&temp.0, &folder), None);
        set_pinned_track(&temp.0, &folder, Some(&track));
        assert_eq!(pinned_track(&temp.0, &folder), Some(track.clone()));

        set_pinned_track(&temp.0, &folder, None);
        assert_eq!(pinned_track(&temp.0, &folder), None);
    }

    #[test]
    fn setting_a_folder_outside_workspace_does_not_write_preferences() {
        let temp = TempDirectory::new();
        let outside = std::env::temp_dir().join("adb-studio-outside-folder");

        set_pinned_track(&temp.0, &outside, None);

        assert!(!temp.0.join(".adbstudio/preferences.json").exists());
    }
}
