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
    #[serde(default)]
    pub original_file_name: String,
    #[serde(default)]
    pub current_file_name: String,
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

pub fn comment_path(folder: &Path, audio_path: &Path) -> Option<PathBuf> {
    let relative_path = audio_path.strip_prefix(folder).ok()?;
    let mut comment_relative_path = relative_path.to_path_buf();
    comment_relative_path.set_extension("json");
    Some(folder.join(".adbstudio").join("comment").join(comment_relative_path))
}

pub fn load_audio_metadata(folder: &Path, audio_path: &Path) -> AudioFileMetadata {
    let metadata = comment_path(folder, audio_path)
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|contents| serde_json::from_str(&contents).ok());
    metadata.or_else(|| {
        let path_string = audio_path.to_string_lossy();
        load_index(folder)
            .audio_files
            .into_iter()
            .find(|item| item.file_path == path_string)
    }).unwrap_or_default()
}

pub fn save_audio_metadata(folder: &Path, audio_path: &Path, metadata: &AudioFileMetadata) {
    let Some(path) = comment_path(folder, audio_path) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(contents) = serde_json::to_string_pretty(metadata) else {
        return;
    };
    let _ = fs::write(path, contents);
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

pub fn record_downloaded_audio(folder: &Path, original_file_name: &str, current_path: &Path) {
    let mut index = load_index(folder);
    let current_path_string = current_path.to_string_lossy().into_owned();
    let current_file_name = current_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(metadata) = index.audio_files.iter_mut().find(|metadata| {
        metadata.file_path == current_path_string
            || (metadata.current_file_name == current_file_name
                && metadata.original_file_name == original_file_name)
    }) {
        metadata.original_file_name = original_file_name.to_string();
        metadata.current_file_name = current_file_name;
        metadata.file_path = current_path_string;
    } else {
        index.audio_files.push(AudioFileMetadata {
            original_file_name: original_file_name.to_string(),
            current_file_name,
            file_path: current_path_string,
            ..Default::default()
        });
    }
    save_index(folder, &index);
}

pub fn has_downloaded_audio(folder: &Path, original_file_name: &str) -> bool {
    load_index(folder).audio_files.iter().any(|metadata| {
        metadata.original_file_name == original_file_name
            && !metadata.file_path.is_empty()
            && Path::new(&metadata.file_path).exists()
    })
}

pub fn rename_audio_metadata(folder: &Path, source: &Path, destination: &Path) {
    let mut index = load_index(folder);
    let mut changed = false;
    for metadata in &mut index.audio_files {
        let path = Path::new(&metadata.file_path);
        if path == source || path.starts_with(source) {
            let Some(relative_path) = path.strip_prefix(source).ok() else {
                continue;
            };
            let current_path = destination.join(relative_path);
            metadata.file_path = current_path.to_string_lossy().into_owned();
            metadata.current_file_name = current_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            changed = true;
        }
    }
    changed |= rename_comment_metadata(folder, source, destination);
    if changed {
        save_index(folder, &index);
    }
}

fn rename_comment_metadata(folder: &Path, source: &Path, destination: &Path) -> bool {
    let Some(source_comment) = comment_path(folder, source) else {
        return false;
    };
    let Some(destination_comment) = comment_path(folder, destination) else {
        return false;
    };
    if !source_comment.exists() {
        return false;
    }
    if let Some(parent) = destination_comment.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::rename(&source_comment, &destination_comment).is_err() {
        return false;
    }
    if let Ok(contents) = fs::read_to_string(&destination_comment) {
        if let Ok(mut metadata) = serde_json::from_str::<AudioFileMetadata>(&contents) {
            metadata.current_file_name = destination
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            metadata.file_path = destination.to_string_lossy().into_owned();
            if let Ok(contents) = serde_json::to_string_pretty(&metadata) {
                let _ = fs::write(destination_comment, contents);
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        comment_path, load_audio_metadata, rename_associated_workflow, rename_audio_metadata,
        save_audio_metadata, save_index, AudioComment, AudioFileMetadata, MetadataIndex,
    };

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

    #[test]
    fn stores_audio_comments_under_workspace_relative_path() {
        let folder = test_folder("comment-file");
        let audio_path = folder.join("folder1/folder2/file-name.wav");
        let metadata = AudioFileMetadata {
            file_path: audio_path.to_string_lossy().into_owned(),
            comments: vec![AudioComment {
                start_seconds: 1.0,
                end_seconds: 2.0,
                text: "note".into(),
            }],
            ..Default::default()
        };

        save_audio_metadata(&folder, &audio_path, &metadata);

        assert_eq!(
            comment_path(&folder, &audio_path).unwrap(),
            folder.join(".adbstudio/comment/folder1/folder2/file-name.json")
        );
        assert_eq!(load_audio_metadata(&folder, &audio_path).comments, metadata.comments);
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn preserves_original_name_and_updates_current_path_after_rename() {
        let folder = test_folder("audio-metadata-rename");
        let source = folder.join("downloads/original.wav");
        let destination = folder.join("edited/final.mp3");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&source, b"audio").unwrap();
        let metadata = AudioFileMetadata {
            original_file_name: "original.wav".into(),
            current_file_name: "original.wav".into(),
            file_path: source.to_string_lossy().into_owned(),
            comments: vec![AudioComment {
                start_seconds: 1.0,
                end_seconds: 2.0,
                text: "note".into(),
            }],
            ..Default::default()
        };
        save_audio_metadata(&folder, &source, &metadata);
        let mut index = MetadataIndex::default();
        index.audio_files.push(metadata);
        save_index(&folder, &index);

        rename_audio_metadata(&folder, &source, &destination);

        let current = load_audio_metadata(&folder, &destination);
        assert_eq!(current.original_file_name, "original.wav");
        assert_eq!(current.current_file_name, "final.mp3");
        assert_eq!(current.file_path, destination.to_string_lossy());
        assert_eq!(current.comments.len(), 1);
        assert!(!comment_path(&folder, &source).unwrap().exists());
        assert!(comment_path(&folder, &destination).unwrap().exists());
        let _ = fs::remove_dir_all(folder);
    }
}
