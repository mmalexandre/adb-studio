use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Component, Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

mod client;
mod worker;

pub use client::ComfyUiClient;

pub const DEFAULT_REMOTE_DIRECTORY: &str = "output/audio";
pub const DEFAULT_LOCAL_DIRECTORY: &str = "downloads";
pub const DEFAULT_INTERVAL_MS: u64 = 4000;

#[derive(Clone, Debug)]
pub enum WorkflowRunUpdate {
    Progress {
        progress: f32,
        step: String,
    },
    Finished {
        audio_path: PathBuf,
        workflow_path: PathBuf,
    },
    Cancelled,
    Error(String),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SyncConfig {
    pub url: String,
    #[serde(default = "default_remote_directory")]
    pub remote_directory: String,
    pub local_directory: String,
    #[serde(default = "default_interval_ms")]
    pub interval_ms: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            remote_directory: DEFAULT_REMOTE_DIRECTORY.to_string(),
            local_directory: DEFAULT_LOCAL_DIRECTORY.to_string(),
            interval_ms: DEFAULT_INTERVAL_MS,
        }
    }
}

impl SyncConfig {
    pub fn normalized(mut self) -> Self {
        self.url = self.url.trim().trim_end_matches('/').to_string();
        self.remote_directory = self.remote_directory.trim().to_string();
        self.interval_ms = self.interval_ms.max(100);
        self
    }

    pub fn local_path(&self, workspace: &Path) -> Result<PathBuf, SyncError> {
        let relative = Path::new(&self.local_directory);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| component == Component::ParentDir)
        {
            return Err(SyncError::InvalidConfig(
                "Download directory must be inside the workspace".to_string(),
            ));
        }
        Ok(workspace.join(relative))
    }
}

fn default_remote_directory() -> String {
    DEFAULT_REMOTE_DIRECTORY.to_string()
}

fn default_interval_ms() -> u64 {
    DEFAULT_INTERVAL_MS
}

#[derive(Clone, Debug, Deserialize)]
pub struct RemoteFile {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub modified: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct DownloadRecord {
    pub url: String,
    pub remote_path: String,
    pub filename: String,
    pub downloaded_at: u64,
    pub status: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct DownloadIndex {
    pub downloads: Vec<DownloadRecord>,
}

impl DownloadIndex {
    fn contains_completed(&self, config: &SyncConfig, file: &RemoteFile) -> bool {
        self.downloads.iter().any(|record| {
            record.status == "completed"
                && record.url == config.url
                && record.remote_path == file.path
        })
    }

    fn record_completed(&mut self, config: &SyncConfig, file: &RemoteFile, size: u64) {
        self.downloads
            .retain(|record| record.url != config.url || record.remote_path != file.path);
        self.downloads.push(DownloadRecord {
            url: config.url.clone(),
            remote_path: file.path.clone(),
            filename: Path::new(&file.name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
            downloaded_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or_default(),
            status: "completed".to_string(),
            size,
        });
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SyncProgress {
    pub present: usize,
    pub total: usize,
}

pub enum SyncEvent {
    Running {
        generation: u64,
    },
    Progress {
        generation: u64,
        progress: SyncProgress,
    },
    Downloaded {
        generation: u64,
        audio_path: String,
    },
    WorkflowUpdated {
        generation: u64,
        audio_path: String,
    },
    Error {
        generation: u64,
        message: String,
    },
}

enum SyncCommand {
    Stop,
}

pub struct SyncController {
    command_sender: Option<Sender<SyncCommand>>,
    event_sender: Sender<SyncEvent>,
    event_receiver: Receiver<SyncEvent>,
    generation: u64,
}

impl SyncController {
    pub fn new() -> Self {
        let (event_sender, event_receiver) = mpsc::channel();
        Self {
            command_sender: None,
            event_sender,
            event_receiver,
            generation: 0,
        }
    }

    pub fn start(&mut self, workspace: PathBuf, config: SyncConfig) {
        self.stop();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let config = config.normalized();
        let (command_sender, command_receiver) = mpsc::channel();
        let event_sender = self.event_sender.clone();
        self.command_sender = Some(command_sender);
        thread::spawn(move || {
            worker::sync_loop(
                workspace,
                config,
                command_receiver,
                event_sender,
                generation,
            )
        });
    }

    pub fn stop(&mut self) {
        if let Some(sender) = self.command_sender.take() {
            let _ = sender.send(SyncCommand::Stop);
        }
    }

    pub fn events(&self) -> &Receiver<SyncEvent> {
        &self.event_receiver
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Debug)]
pub enum SyncError {
    InvalidConfig(String),
    Request(reqwest::Error),
    Io(io::Error),
    Response(String),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(message) | Self::Response(message) => formatter.write_str(message),
            Self::Request(error) => write!(formatter, "ComfyUI request failed: {error}"),
            Self::Io(error) => write!(formatter, "File sync failed: {error}"),
        }
    }
}

impl std::error::Error for SyncError {}

pub fn config_path(workspace: &Path) -> PathBuf {
    workspace.join(".adbstudio").join("sync.json")
}

pub fn download_index_path(workspace: &Path) -> PathBuf {
    workspace.join(".adbstudio").join("sync-downloads.json")
}

pub fn load_config(workspace: &Path) -> Option<SyncConfig> {
    std::fs::read_to_string(config_path(workspace))
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .map(SyncConfig::normalized)
}

pub fn save_config(workspace: &Path, config: &SyncConfig) -> Result<(), SyncError> {
    std::fs::create_dir_all(workspace.join(".adbstudio")).map_err(SyncError::Io)?;
    let contents = serde_json::to_string_pretty(&config.clone().normalized())
        .map_err(|error| SyncError::Response(error.to_string()))?;
    std::fs::write(config_path(workspace), contents).map_err(SyncError::Io)
}

pub fn ensure_destination(workspace: &Path, config: &SyncConfig) -> Result<PathBuf, SyncError> {
    let destination = config.local_path(workspace)?;
    std::fs::create_dir_all(&destination).map_err(SyncError::Io)?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_destination, SyncConfig, DEFAULT_INTERVAL_MS, DEFAULT_LOCAL_DIRECTORY,
        DEFAULT_REMOTE_DIRECTORY,
    };
    use std::{
        fs,
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn defaults_are_stable() {
        let config = SyncConfig::default();
        assert_eq!(config.remote_directory, DEFAULT_REMOTE_DIRECTORY);
        assert_eq!(config.local_directory, DEFAULT_LOCAL_DIRECTORY);
        assert_eq!(config.interval_ms, DEFAULT_INTERVAL_MS);
    }

    #[test]
    fn normalization_removes_url_slashes_and_clamps_interval() {
        let config = SyncConfig {
            url: " https://example.test/// ".to_string(),
            interval_ms: 1,
            ..SyncConfig::default()
        }
        .normalized();
        assert_eq!(config.url, "https://example.test");
        assert_eq!(config.interval_ms, 100);
    }

    #[test]
    fn local_directory_rejects_escape() {
        let config = SyncConfig {
            local_directory: "../outside".to_string(),
            ..SyncConfig::default()
        };
        assert!(config.local_path(Path::new("/workspace")).is_err());
    }

    #[test]
    fn creates_missing_download_directory() {
        let workspace = tempfile_directory();
        let config = SyncConfig::default();
        let destination = ensure_destination(&workspace, &config).unwrap();
        assert!(destination.is_dir());
        assert_eq!(destination, workspace.join(DEFAULT_LOCAL_DIRECTORY));
        fs::remove_dir_all(destination).unwrap();
        fs::remove_dir_all(workspace).unwrap();
    }

    fn tempfile_directory() -> std::path::PathBuf {
        static TEST_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let counter = TEST_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "adb-studio-sync-{}-{timestamp}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
}

