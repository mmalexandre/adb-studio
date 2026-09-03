use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AudioFileMetadata {
    pub file_path: String,
    pub rating: u8,
    pub comments: Vec<String>,
    pub modified_date: String,
    pub waveform_cache_key: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct MetadataIndex {
    pub audio_files: Vec<AudioFileMetadata>,
}

pub fn load_index(folder: &Path) -> MetadataIndex {
    let path = folder.join(".adbstudio").join("index.json");
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn save_index(folder: &Path, index: &MetadataIndex) {
    let directory = folder.join(".adbstudio");
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    let path: PathBuf = directory.join("index.json");
    let Ok(contents) = serde_json::to_string_pretty(index) else {
        return;
    };
    let _ = fs::write(path, contents);
}