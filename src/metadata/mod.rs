use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub mod comfyui;

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
pub struct LoraMetadata {
    pub filename: String,
    #[serde(default)]
    pub custom_tag: String,
}

impl AudioFileMetadata {
    pub fn normalized_rating(&self) -> u8 {
        self.rating.min(5)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct MetadataIndex {
    pub audio_files: Vec<AudioFileMetadata>,
    #[serde(default)]
    pub loras: Vec<LoraMetadata>,
}

pub fn load_index(folder: &Path) -> MetadataIndex {
    let path = folder.join(".adbstudio").join("index.json");
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn workflow_path(folder: &Path, audio_path: &Path) -> Option<PathBuf> {
    let relative_path = audio_path.strip_prefix(folder).ok()?;
    let file_name = relative_path.file_name()?.to_str()?;
    let mut workflow_relative_path = relative_path.to_path_buf();
    workflow_relative_path.set_file_name(format!("{file_name}.workflow.json"));
    Some(folder.join(".adbstudio").join("workflows").join(workflow_relative_path))
}

pub fn rename_associated_workflow(
    folder: &Path,
    source: &Path,
    destination: &Path,
) -> std::io::Result<()> {
    let directory_workflow = folder
        .join(".adbstudio")
        .join("workflows")
        .join(source.strip_prefix(folder).unwrap_or(source));
    let is_directory = source.is_dir() || destination.is_dir() || directory_workflow.is_dir();
    let source_workflow = if is_directory {
        directory_workflow
    } else if source
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "flac" | "mp3" | "ogg" | "opus" | "wav"
            )
        }) {
        workflow_path(folder, source).unwrap_or_default()
    } else {
        return Ok(());
    };
    if !source_workflow.exists() {
        return Ok(());
    }
    let destination_workflow = if is_directory {
        folder
            .join(".adbstudio")
            .join("workflows")
            .join(destination.strip_prefix(folder).unwrap_or(destination))
    } else {
        workflow_path(folder, destination).unwrap_or_default()
    };
    if let Some(parent) = destination_workflow.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(source_workflow, destination_workflow)
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
    use std::fs;

    use super::{rename_associated_workflow, AudioComment, AudioFileMetadata};

    fn test_folder(name: &str) -> std::path::PathBuf {
        let folder = std::env::temp_dir().join(format!("adb-studio-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn renames_audio_workflow_sidecar() {
        let folder = test_folder("workflow-file");
        let source = folder.join("old.wav");
        let destination = folder.join("new.wav");
        let workflow = super::workflow_path(&folder, &source).unwrap();
        fs::create_dir_all(workflow.parent().unwrap()).unwrap();
        fs::write(&workflow, "{}").unwrap();

        rename_associated_workflow(&folder, &source, &destination).unwrap();

        assert!(!workflow.exists());
        assert!(super::workflow_path(&folder, &destination).unwrap().exists());
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn renames_directory_workflow_subtree() {
        let folder = test_folder("workflow-directory");
        let source = folder.join("old");
        let destination = folder.join("new");
        let workflow = folder.join(".adbstudio/workflows/old");
        fs::create_dir_all(&workflow).unwrap();
        fs::write(workflow.join("track.wav.workflow.json"), "{}").unwrap();

        rename_associated_workflow(&folder, &source, &destination).unwrap();

        assert!(!workflow.exists());
        assert!(folder
            .join(".adbstudio/workflows/new/track.wav.workflow.json")
            .exists());
        let _ = fs::remove_dir_all(folder);
    }

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

    #[test]
    fn clamps_rating_to_five_stars() {
        let metadata = AudioFileMetadata {
            rating: 9,
            ..Default::default()
        };
        assert_eq!(metadata.normalized_rating(), 5);
    }
}
