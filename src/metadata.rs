use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AudioComment {
    pub start_seconds: f32,
    pub end_seconds: f32,
    pub text: String,
}

impl AudioComment {
    pub fn normalized(self, duration_seconds: f32) -> Self {
        let duration = duration_seconds.max(0.0);
        let start = self.start_seconds.clamp(0.0, duration);
        let end = self.end_seconds.clamp(0.0, duration);
        Self {
            start_seconds: start.min(end),
            end_seconds: start.max(end),
            text: self.text,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AudioFileMetadata {
    pub file_path: String,
    pub rating: u8,
    pub comments: Vec<AudioComment>,
    pub modified_date: String,
    pub waveform_cache_key: String,
    #[serde(default)]
    pub last_position_seconds: f32,
    #[serde(default)]
    pub duration_seconds: f32,
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

#[cfg(test)]
mod tests {
    use super::{AudioComment, AudioFileMetadata};

    #[test]
    fn round_trips_range_comments() {
        let metadata = AudioFileMetadata {
            comments: vec![AudioComment {
                start_seconds: 1.5,
                end_seconds: 4.0,
                text: "chorus".to_string(),
            }],
            ..Default::default()
        };

        let encoded = serde_json::to_string(&metadata).unwrap();
        let decoded: AudioFileMetadata = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.comments, metadata.comments);
    }

    #[test]
    fn normalizes_and_clamps_comment_ranges() {
        let comment = AudioComment {
            start_seconds: 8.0,
            end_seconds: -2.0,
            text: "range".to_string(),
        }
        .normalized(5.0);

        assert_eq!(comment.start_seconds, 0.0);
        assert_eq!(comment.end_seconds, 5.0);
    }
}
