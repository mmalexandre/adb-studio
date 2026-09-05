use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const DEFAULT_REMOTE_DIRECTORY: &str = "output/audio";
pub const DEFAULT_LOCAL_DIRECTORY: &str = "downloads";
pub const DEFAULT_INTERVAL_MS: u64 = 4000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
        let (command_sender, command_receiver) = mpsc::channel();
        let event_sender = self.event_sender.clone();
        self.command_sender = Some(command_sender);
        thread::spawn(move || {
            sync_loop(
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

fn sync_loop(
    workspace: PathBuf,
    config: SyncConfig,
    command_receiver: Receiver<SyncCommand>,
    event_sender: Sender<SyncEvent>,
    generation: u64,
) {
    let client = match ComfyUiClient::new() {
        Ok(client) => client,
        Err(error) => {
            let _ = event_sender.send(SyncEvent::Error {
                generation,
                message: error.to_string(),
            });
            return;
        }
    };
    let destination = match config.local_path(&workspace) {
        Ok(path) => path,
        Err(error) => {
            let _ = event_sender.send(SyncEvent::Error {
                generation,
                message: error.to_string(),
            });
            return;
        }
    };
    if let Err(error) = fs::create_dir_all(&destination) {
        let _ = event_sender.send(SyncEvent::Error {
            generation,
            message: SyncError::Io(error).to_string(),
        });
        return;
    }
    let interval = Duration::from_millis(config.interval_ms.max(100));

    loop {
        let _ = event_sender.send(SyncEvent::Running { generation });
        let files = match client.list_files(&config) {
            Ok(files) => files,
            Err(error) => {
                let _ = event_sender.send(SyncEvent::Error {
                    generation,
                    message: error.to_string(),
                });
                return;
            }
        };
        let mut current = match progress(&files, &destination) {
            Ok(progress) => progress,
            Err(error) => {
                let _ = event_sender.send(SyncEvent::Error {
                    generation,
                    message: error.to_string(),
                });
                return;
            }
        };
        let _ = event_sender.send(SyncEvent::Progress {
            generation,
            progress: current.clone(),
        });

        for file in &files {
            if command_receiver.try_recv().is_ok() {
                return;
            }
            let filename = Path::new(&file.name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if filename.is_empty() || destination.join(filename).exists() {
                continue;
            }
            if let Err(error) = client.download_file(&config, file, &destination) {
                let _ = event_sender.send(SyncEvent::Error {
                    generation,
                    message: error.to_string(),
                });
                return;
            }
            current.present += 1;
            let _ = event_sender.send(SyncEvent::Progress {
                generation,
                progress: current.clone(),
            });
        }

        match command_receiver.recv_timeout(interval) {
            Ok(SyncCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
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

pub fn load_config(workspace: &Path) -> Option<SyncConfig> {
    fs::read_to_string(config_path(workspace))
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .map(SyncConfig::normalized)
}

pub fn save_config(workspace: &Path, config: &SyncConfig) -> Result<(), SyncError> {
    fs::create_dir_all(workspace.join(".adbstudio")).map_err(SyncError::Io)?;
    let contents = serde_json::to_string_pretty(&config.clone().normalized())
        .map_err(|error| SyncError::Response(error.to_string()))?;
    fs::write(config_path(workspace), contents).map_err(SyncError::Io)
}

pub fn ensure_destination(workspace: &Path, config: &SyncConfig) -> Result<PathBuf, SyncError> {
    let destination = config.local_path(workspace)?;
    fs::create_dir_all(&destination).map_err(SyncError::Io)?;
    Ok(destination)
}

pub struct ComfyUiClient {
    client: Client,
}

impl ComfyUiClient {
    pub fn new() -> Result<Self, SyncError> {
        Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent("adb-studio/0.1")
            .build()
            .map(|client| Self { client })
            .map_err(SyncError::Request)
    }

    pub fn list_files(&self, config: &SyncConfig) -> Result<Vec<RemoteFile>, SyncError> {
        validate_request_config(config)?;
        let response = self
            .client
            .get(format!("{}/adb-music-player/audio-files", config.url))
            .query(&[("directory", config.remote_directory.as_str())])
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .map_err(SyncError::Request)?
            .error_for_status()
            .map_err(SyncError::Request)?;
        let files: Vec<RemoteFile> = response.json().map_err(SyncError::Request)?;
        if files
            .iter()
            .any(|file| file.name.trim().is_empty() || file.path.trim().is_empty())
        {
            return Err(SyncError::Response(
                "ComfyUI returned a file without a name or path".to_string(),
            ));
        }
        Ok(files)
    }

    pub fn download_file(
        &self,
        config: &SyncConfig,
        file: &RemoteFile,
        destination: &Path,
    ) -> Result<(), SyncError> {
        validate_request_config(config)?;
        let filename = Path::new(&file.name)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty() && *name != "." && *name != "..")
            .ok_or_else(|| {
                SyncError::Response("ComfyUI returned an invalid filename".to_string())
            })?;
        fs::create_dir_all(destination).map_err(SyncError::Io)?;
        let final_path = destination.join(filename);
        let temporary_path = temporary_download_path(destination, filename);
        let result = (|| {
            let mut response = self
                .client
                .get(format!("{}/adb-music-player/audio-download", config.url))
                .query(&[("path", file.path.as_str())])
                .send()
                .map_err(SyncError::Request)?
                .error_for_status()
                .map_err(SyncError::Request)?;
            let mut temporary_file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
                .map_err(SyncError::Io)?;
            response
                .copy_to(&mut temporary_file)
                .map_err(SyncError::Request)?;
            temporary_file.flush().map_err(SyncError::Io)?;
            temporary_file.sync_all().map_err(SyncError::Io)?;
            fs::rename(&temporary_path, &final_path).map_err(SyncError::Io)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        result
    }
}

fn temporary_download_path(destination: &Path, filename: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    destination.join(format!(
        ".{filename}.{}.{timestamp}.{counter}",
        std::process::id()
    ))
}

pub fn progress(files: &[RemoteFile], destination: &Path) -> Result<SyncProgress, SyncError> {
    let local_names: HashSet<String> = match fs::read_dir(destination) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => HashSet::new(),
        Err(error) => return Err(SyncError::Io(error)),
    };
    let present = files
        .iter()
        .filter_map(|file| {
            Path::new(&file.name)
                .file_name()
                .and_then(|name| name.to_str())
        })
        .filter(|name| local_names.contains(*name))
        .count();
    Ok(SyncProgress {
        present,
        total: files.len(),
    })
}

fn validate_request_config(config: &SyncConfig) -> Result<(), SyncError> {
    if config.url.is_empty() {
        return Err(SyncError::InvalidConfig(
            "ComfyUI URL is required".to_string(),
        ));
    }
    if !(config.url.starts_with("http://") || config.url.starts_with("https://")) {
        return Err(SyncError::InvalidConfig(
            "ComfyUI URL must start with http:// or https://".to_string(),
        ));
    }
    if config.remote_directory.is_empty() {
        return Err(SyncError::InvalidConfig(
            "Remote directory is required".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_destination, progress, temporary_download_path, RemoteFile, SyncConfig,
        DEFAULT_INTERVAL_MS, DEFAULT_LOCAL_DIRECTORY, DEFAULT_REMOTE_DIRECTORY,
    };
    use std::{fs, path::Path};

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

    #[test]
    fn temporary_download_name_is_hidden_and_keeps_original_name() {
        let path = temporary_download_path(Path::new("/workspace/downloads"), "myfile.opus");
        let filename = path.file_name().unwrap().to_string_lossy();
        assert!(filename.starts_with(".myfile.opus."));
        assert_ne!(filename, "myfile.opus");
    }

    #[test]
    fn progress_counts_matching_basenames() {
        let directory = tempfile_directory();
        fs::write(directory.join("song.mp3"), b"audio").unwrap();
        let files = vec![
            RemoteFile {
                name: "nested/song.mp3".to_string(),
                path: "/remote/song.mp3".to_string(),
            },
            RemoteFile {
                name: "other.mp3".to_string(),
                path: "/remote/other.mp3".to_string(),
            },
        ];
        assert_eq!(progress(&files, &directory).unwrap().present, 1);
        fs::remove_dir_all(directory).unwrap();
    }

    fn tempfile_directory() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("adb-studio-sync-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
