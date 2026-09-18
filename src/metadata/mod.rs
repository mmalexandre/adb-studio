use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

pub mod comfyui;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ChecksumCache {
    files: Vec<ChecksumCacheEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ChecksumCacheEntry {
    path: String,
    size: u64,
    modified_nanos: u128,
    checksum: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AudioComment {
    pub start_seconds: f32,
    pub end_seconds: f32,
    pub text: String,
    #[serde(default)]
    pub label_id: Option<u64>,
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
            label_id: self.label_id,
        }
    }
}

pub fn comment_quantization_beats(index: i32) -> f32 {
    match index {
        1..=4 => index as f32,
        5 => 8.0,
        6 => 16.0,
        _ => 0.0,
    }
}

pub fn quantize_comment_range(
    start: f32,
    end: f32,
    duration_seconds: f32,
    bpm: &str,
    quantization_index: i32,
) -> (f32, f32) {
    let duration = duration_seconds.max(0.0);
    if duration <= 0.0 {
        return (0.0, 0.0);
    }
    let start_seconds = (start * duration).clamp(0.0, duration);
    let end_seconds = (end * duration).clamp(0.0, duration);
    let beats = comment_quantization_beats(quantization_index);
    let Some(bpm) = bpm.parse::<f32>().ok().filter(|value| *value > 0.0) else {
        return (
            start_seconds.min(end_seconds) / duration,
            start_seconds.max(end_seconds) / duration,
        );
    };
    if beats <= 0.0 {
        return (
            start_seconds.min(end_seconds) / duration,
            start_seconds.max(end_seconds) / duration,
        );
    }
    let interval = 60.0 / bpm * beats;
    let snap = |seconds: f32| (seconds / interval).round() * interval;
    let snapped_start = snap(start_seconds).clamp(0.0, duration);
    let snapped_end = snap(end_seconds).clamp(0.0, duration);
    (
        snapped_start.min(snapped_end) / duration,
        snapped_start.max(snapped_end) / duration,
    )
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AudioFileMetadata {
    #[serde(default)]
    pub checksum: String,
    #[serde(default)]
    pub original_file_name: String,
    #[serde(default)]
    pub current_file_name: String,
    pub file_path: String,
    pub rating: u8,
    pub comments: Vec<AudioComment>,
    #[serde(default)]
    pub label_id: Option<u64>,
    #[serde(default)]
    pub tag_ids: Vec<u64>,
    #[serde(default)]
    pub user_comments: String,
    #[serde(default)]
    pub custom_tag: String,
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct StemExportManifest {
    pub exports: Vec<StemExport>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StemExport {
    pub parent_path: String,
    pub parent_hash: String,
    pub model: String,
    pub format: String,
    pub output_folder: String,
    pub stems: Vec<StemFile>,
    pub exported_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StemFile {
    pub kind: String,
    pub path: String,
    pub hash: String,
}

pub fn load_stem_manifest(folder: &Path) -> StemExportManifest {
    fs::read_to_string(folder.join(".adbstudio").join("stem_exports.json"))
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn save_stem_manifest(folder: &Path, manifest: &StemExportManifest) -> io::Result<()> {
    let directory = folder.join(".adbstudio");
    fs::create_dir_all(&directory)?;
    let contents = serde_json::to_string_pretty(manifest)
        .map_err(|error| io::Error::other(format!("serialize stem manifest: {error}")))?;
    fs::write(directory.join("stem_exports.json"), contents)
}

pub fn load_index(folder: &Path) -> MetadataIndex {
    let path = folder.join(".adbstudio").join("index.json");
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn checksum_for_file(folder: &Path, path: &Path) -> io::Result<String> {
    let relative_path = path
        .strip_prefix(folder)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file is outside workspace"))?
        .to_string_lossy()
        .replace('\\', "/");
    let file_metadata = fs::metadata(path)?;
    let modified_nanos = file_metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| io::Error::other(format!("file modification time is invalid: {error}")))?
        .as_nanos();
    let cache_path = folder.join(".adbstudio").join("checksums.json");
    let mut cache = fs::read_to_string(&cache_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<ChecksumCache>(&contents).ok())
        .unwrap_or_default();
    if let Some(entry) = cache.files.iter().find(|entry| {
        entry.path == relative_path
            && entry.size == file_metadata.len()
            && entry.modified_nanos == modified_nanos
    }) {
        return Ok(entry.checksum.clone());
    }

    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        digest.update(&buffer[..bytes_read]);
    }
    let checksum = format!("{:x}", digest.finalize());
    cache.files.retain(|entry| entry.path != relative_path);
    cache.files.push(ChecksumCacheEntry {
        path: relative_path,
        size: file_metadata.len(),
        modified_nanos,
        checksum: checksum.clone(),
    });
    save_checksum_cache(folder, &cache)?;
    Ok(checksum)
}

fn save_checksum_cache(folder: &Path, cache: &ChecksumCache) -> io::Result<()> {
    let directory = folder.join(".adbstudio");
    fs::create_dir_all(&directory)?;
    let contents = serde_json::to_string_pretty(cache)
        .map_err(|error| io::Error::other(format!("serialize checksum cache: {error}")))?;
    fs::write(directory.join("checksums.json"), contents)
}

pub fn workflow_path(folder: &Path, audio_path: &Path) -> Option<PathBuf> {
    let relative_path = audio_path.strip_prefix(folder).ok()?;
    let file_name = relative_path.file_name()?.to_str()?;
    let adjacent = audio_path.with_file_name(format!("{file_name}.workflow.json"));
    if adjacent.is_file() {
        return Some(adjacent);
    }
    let mut workflow_relative_path = relative_path.to_path_buf();
    workflow_relative_path.set_file_name(format!("{file_name}.workflow.json"));
    Some(
        folder
            .join(".adbstudio")
            .join("workflows")
            .join(workflow_relative_path),
    )
}

pub fn recreated_metadata_path(folder: &Path, audio_path: &Path) -> Option<PathBuf> {
    let adjacent = audio_path.with_file_name(format!(
        "{}.metadata.json",
        audio_path.file_name()?.to_str()?
    ));
    if adjacent.is_file() {
        return Some(adjacent);
    }
    let workflow = workflow_path(folder, audio_path)?;
    Some(workflow.with_file_name(format!("{}.metadata.json", workflow.file_name()?.to_str()?)))
}

pub fn clear_workflow_recreated(folder: &Path, audio_path: &Path) {
    if let Some(path) = recreated_metadata_path(folder, audio_path) {
        let _ = fs::remove_file(path);
    }
}

pub fn comment_path(folder: &Path, audio_path: &Path) -> Option<PathBuf> {
    let relative_path = audio_path.strip_prefix(folder).ok()?;
    let mut comment_relative_path = relative_path.to_path_buf();
    comment_relative_path.set_extension("json");
    Some(
        folder
            .join(".adbstudio")
            .join("comment")
            .join(comment_relative_path),
    )
}

pub fn load_audio_metadata(folder: &Path, audio_path: &Path) -> AudioFileMetadata {
    let checksum = checksum_for_file(folder, audio_path).unwrap_or_default();
    let mut index = load_index(folder);
    let sidecar_metadata = comment_path(folder, audio_path)
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|contents| serde_json::from_str::<AudioFileMetadata>(&contents).ok());
    let metadata = sidecar_metadata
        .or_else(|| {
            index
                .audio_files
                .iter()
                .find(|item| !checksum.is_empty() && item.checksum == checksum)
                .cloned()
        })
        .or_else(|| {
            let path_string = audio_path.to_string_lossy();
            index
                .audio_files
                .iter()
                .find(|item| item.file_path == path_string)
                .cloned()
        })
        .unwrap_or_default();
    if !checksum.is_empty() && metadata.checksum != checksum {
        let mut metadata = metadata;
        metadata.checksum = checksum;
        metadata.file_path = audio_path.to_string_lossy().into_owned();
        if let Some(stored) = index.audio_files.iter_mut().find(|item| {
            (!metadata.checksum.is_empty() && item.checksum == metadata.checksum)
                || item.file_path == metadata.file_path
        }) {
            *stored = metadata.clone();
        } else {
            index.audio_files.push(metadata.clone());
        }
        save_index(folder, &index);
        return metadata;
    }
    metadata
}

pub fn save_audio_metadata(folder: &Path, audio_path: &Path, metadata: &AudioFileMetadata) {
    let _ = save_audio_metadata_checked(folder, audio_path, metadata);
}

pub fn save_audio_metadata_checked(
    folder: &Path,
    audio_path: &Path,
    metadata: &AudioFileMetadata,
) -> std::io::Result<()> {
    let mut metadata = metadata.clone();
    if metadata.checksum.is_empty() {
        metadata.checksum = checksum_for_file(folder, audio_path).unwrap_or_default();
    }
    metadata.file_path = audio_path.to_string_lossy().into_owned();
    metadata.current_file_name = audio_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut index = load_index(folder);
    if let Some(stored) = index.audio_files.iter_mut().find(|item| {
        (!metadata.checksum.is_empty() && item.checksum == metadata.checksum)
            || item.file_path == metadata.file_path
    }) {
        *stored = metadata.clone();
    } else {
        index.audio_files.push(metadata.clone());
    }
    save_index(folder, &index);
    let Some(path) = comment_path(folder, audio_path) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "audio path is outside the workspace",
        ));
    };
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "metadata path has no parent",
        ));
    };
    fs::create_dir_all(parent)?;
    let contents = serde_json::to_string_pretty(&metadata)
        .map_err(|error| std::io::Error::other(format!("serialize metadata: {error}")))?;
    fs::write(path, contents)
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
        })
    {
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
    let checksum = checksum_for_file(folder, current_path).unwrap_or_default();
    let mut index = load_index(folder);
    let current_path_string = current_path.to_string_lossy().into_owned();
    let current_file_name = current_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(metadata) = index.audio_files.iter_mut().find(|metadata| {
        (!checksum.is_empty() && metadata.checksum == checksum)
            || metadata.file_path == current_path_string
            || (metadata.current_file_name == current_file_name
                && metadata.original_file_name == original_file_name)
    }) {
        metadata.checksum = checksum;
        metadata.original_file_name = original_file_name.to_string();
        metadata.current_file_name = current_file_name;
        metadata.file_path = current_path_string;
    } else {
        index.audio_files.push(AudioFileMetadata {
            checksum,
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
    let (source_comment, destination_comment) = if source.is_dir() || destination.is_dir() {
        let Some(source_relative) = source.strip_prefix(folder).ok() else {
            return false;
        };
        let Some(destination_relative) = destination.strip_prefix(folder).ok() else {
            return false;
        };
        (
            folder.join(".adbstudio/comment").join(source_relative),
            folder.join(".adbstudio/comment").join(destination_relative),
        )
    } else {
        let Some(source_comment) = comment_path(folder, source) else {
            return false;
        };
        let Some(destination_comment) = comment_path(folder, destination) else {
            return false;
        };
        (source_comment, destination_comment)
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
    update_comment_metadata(&destination_comment, source, destination)
}

fn update_comment_metadata(path: &Path, source: &Path, destination: &Path) -> bool {
    if path.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return false;
        };
        let mut changed = false;
        for entry in entries.flatten() {
            changed |= update_comment_metadata(&entry.path(), source, destination);
        }
        return changed;
    }

    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(mut metadata) = serde_json::from_str::<AudioFileMetadata>(&contents) else {
        return false;
    };
    let metadata_path = Path::new(&metadata.file_path);
    let current_path = if metadata_path == source {
        destination.to_path_buf()
    } else {
        let Some(relative_path) = metadata_path.strip_prefix(source).ok() else {
            return false;
        };
        destination.join(relative_path)
    };
    metadata.current_file_name = current_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    metadata.file_path = current_path.to_string_lossy().into_owned();
    let Ok(contents) = serde_json::to_string_pretty(&metadata) else {
        return false;
    };
    fs::write(path, contents).is_ok()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        checksum_for_file, comment_path, has_downloaded_audio, load_audio_metadata, load_index,
        quantize_comment_range, record_downloaded_audio, rename_associated_workflow,
        rename_audio_metadata, save_audio_metadata, save_index, AudioComment, AudioFileMetadata,
        MetadataIndex,
    };

    fn test_folder(name: &str) -> std::path::PathBuf {
        let folder = std::env::temp_dir().join(format!("adb-studio-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn quantizes_comment_endpoints_to_bpm_grid() {
        let (start, end) = quantize_comment_range(0.13, 0.36, 10.0, "120", 1);

        assert!((start - 0.15).abs() < 0.0001);
        assert!((end - 0.35).abs() < 0.0001);
    }

    #[test]
    fn leaves_comment_endpoints_unchanged_without_quantization() {
        let range = quantize_comment_range(0.36, 0.13, 10.0, "120", 0);

        assert!((range.0 - 0.13).abs() < 0.0001);
        assert!((range.1 - 0.36).abs() < 0.0001);
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
        assert!(super::workflow_path(&folder, &destination)
            .unwrap()
            .exists());
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
    fn renames_nested_directory_metadata_paths() {
        let folder = test_folder("metadata-directory");
        let source = folder.join("old");
        let destination = folder.join("new");
        let source_track = source.join("nested/track.wav");
        let metadata_path = folder.join(".adbstudio/comment/old/nested/track.json");
        let metadata = AudioFileMetadata {
            current_file_name: "track.wav".to_string(),
            file_path: source_track.to_string_lossy().into_owned(),
            ..Default::default()
        };
        fs::create_dir_all(source_track.parent().unwrap()).unwrap();
        fs::write(&source_track, "audio").unwrap();
        fs::create_dir_all(metadata_path.parent().unwrap()).unwrap();
        fs::write(&metadata_path, serde_json::to_string(&metadata).unwrap()).unwrap();

        fs::rename(&source, &destination).unwrap();
        rename_audio_metadata(&folder, &source, &destination);

        let metadata: AudioFileMetadata = serde_json::from_str(
            &fs::read_to_string(folder.join(".adbstudio/comment/new/nested/track.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            metadata.file_path,
            destination.join("nested/track.wav").to_string_lossy()
        );
        assert_eq!(metadata.current_file_name, "track.wav");
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn round_trips_range_comments() {
        let metadata = AudioFileMetadata {
            comments: vec![AudioComment {
                start_seconds: 1.5,
                end_seconds: 4.0,
                text: "chorus".to_string(),
                label_id: None,
            }],
            ..Default::default()
        };

        let encoded = serde_json::to_string(&metadata).unwrap();
        let decoded: AudioFileMetadata = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.comments, metadata.comments);
    }

    #[test]
    fn round_trips_user_comments() {
        let folder = test_folder("user-comments-file");
        let audio_path = folder.join("track.wav");
        let metadata = AudioFileMetadata {
            file_path: audio_path.to_string_lossy().into_owned(),
            user_comments: "Remember the alternate mix".to_string(),
            ..Default::default()
        };

        save_audio_metadata(&folder, &audio_path, &metadata);

        assert_eq!(
            load_audio_metadata(&folder, &audio_path).user_comments,
            metadata.user_comments
        );
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn reuses_checksum_for_unchanged_file() {
        let folder = test_folder("checksum-cache");
        let audio_path = folder.join("track.wav");
        fs::write(&audio_path, b"audio").unwrap();

        let first = checksum_for_file(&folder, &audio_path).unwrap();
        let cache_path = folder.join(".adbstudio/checksums.json");
        let cached = fs::read_to_string(&cache_path).unwrap();
        fs::write(&cache_path, cached.replace(&first, "cached-checksum")).unwrap();

        assert_eq!(
            checksum_for_file(&folder, &audio_path).unwrap(),
            "cached-checksum"
        );
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn moved_file_keeps_checksum_linked_metadata() {
        let folder = test_folder("checksum-move");
        let source = folder.join("old.wav");
        let destination = folder.join("new.wav");
        fs::write(&source, b"audio").unwrap();
        let mut metadata = AudioFileMetadata {
            comments: vec![AudioComment {
                start_seconds: 1.0,
                end_seconds: 2.0,
                text: "keep me".to_string(),
                label_id: None,
            }],
            rating: 4,
            ..Default::default()
        };
        metadata.file_path = source.to_string_lossy().into_owned();
        save_audio_metadata(&folder, &source, &metadata);
        fs::rename(&source, &destination).unwrap();

        let moved = load_audio_metadata(&folder, &destination);
        assert_eq!(
            moved.checksum,
            checksum_for_file(&folder, &destination).unwrap()
        );
        assert_eq!(moved.rating, 4);
        assert_eq!(moved.comments, metadata.comments);
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn normalizes_and_clamps_comment_ranges() {
        let comment = AudioComment {
            start_seconds: 8.0,
            end_seconds: -2.0,
            text: "range".to_string(),
            label_id: None,
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
    fn metadata_paths_reject_files_outside_the_workspace() {
        let folder = test_folder("outside-path");
        let outside = std::path::Path::new("/tmp/outside.wav");

        assert_eq!(super::workflow_path(&folder, outside), None);
        assert_eq!(comment_path(&folder, outside), None);
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn downloaded_audio_is_recorded_once_and_requires_an_existing_file() {
        let folder = test_folder("download-record");
        let audio_path = folder.join("downloads/song.wav");
        fs::create_dir_all(audio_path.parent().unwrap()).unwrap();
        fs::write(&audio_path, b"audio").unwrap();

        record_downloaded_audio(&folder, "remote-song.wav", &audio_path);
        record_downloaded_audio(&folder, "remote-song.wav", &audio_path);

        assert!(has_downloaded_audio(&folder, "remote-song.wav"));
        assert_eq!(load_index(&folder).audio_files.len(), 1);
        fs::remove_file(audio_path).unwrap();
        assert!(!has_downloaded_audio(&folder, "remote-song.wav"));
        let _ = fs::remove_dir_all(folder);
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
                label_id: None,
            }],
            ..Default::default()
        };

        save_audio_metadata(&folder, &audio_path, &metadata);

        assert_eq!(
            comment_path(&folder, &audio_path).unwrap(),
            folder.join(".adbstudio/comment/folder1/folder2/file-name.json")
        );
        assert_eq!(
            load_audio_metadata(&folder, &audio_path).comments,
            metadata.comments
        );
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn round_trips_audio_rating() {
        let folder = test_folder("rating-file");
        let audio_path = folder.join("track.wav");
        let metadata = AudioFileMetadata {
            file_path: audio_path.to_string_lossy().into_owned(),
            rating: 2,
            ..Default::default()
        };

        save_audio_metadata(&folder, &audio_path, &metadata);

        assert_eq!(load_audio_metadata(&folder, &audio_path).rating, 2);
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
                label_id: None,
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
