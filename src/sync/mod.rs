use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::metadata;

pub const DEFAULT_REMOTE_DIRECTORY: &str = "output/audio";
pub const DEFAULT_LOCAL_DIRECTORY: &str = "downloads";
pub const DEFAULT_INTERVAL_MS: u64 = 4000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const API_POLL_INTERVAL: Duration = Duration::from_millis(500);
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    let mut download_index = match load_download_index(&workspace) {
        Ok(index) => index,
        Err(error) => {
            let _ = event_sender.send(SyncEvent::Error {
                generation,
                message: error.to_string(),
            });
            return;
        }
    };

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
        let mut current = match progress_with_index(&files, &destination, &config, &download_index)
        {
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

        let mut last_downloaded_path = None;
        for file in &files {
            if command_receiver.try_recv().is_ok() {
                return;
            }
            let filename = Path::new(&file.name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if filename.is_empty() {
                continue;
            }
            if !destination.join(filename).exists()
                && !download_index.contains_completed(&config, file)
            {
                if let Err(error) = client.download_file(&config, file, &destination) {
                    let _ = event_sender.send(SyncEvent::Error {
                        generation,
                        message: error.to_string(),
                    });
                    return;
                }
                let size = match fs::metadata(destination.join(filename)) {
                    Ok(metadata) => metadata.len(),
                    Err(error) => {
                        let _ = event_sender.send(SyncEvent::Error {
                            generation,
                            message: SyncError::Io(error).to_string(),
                        });
                        return;
                    }
                };
                download_index.record_completed(&config, file, size);
                if let Err(error) = save_download_index(&workspace, &download_index) {
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
                last_downloaded_path = Some(destination.join(filename));
            }
            match sync_workflow(&client, &config, file, &workspace, filename) {
                Ok(true) => {
                    let _ = event_sender.send(SyncEvent::WorkflowUpdated {
                        generation,
                        audio_path: destination.join(filename).to_string_lossy().into_owned(),
                    });
                }
                Ok(false) => {}
                Err(error) => {
                    let _ = event_sender.send(SyncEvent::Error {
                        generation,
                        message: error.to_string(),
                    });
                    return;
                }
            }
        }

        if let Some(audio_path) = last_downloaded_path {
            let _ = event_sender.send(SyncEvent::Downloaded {
                generation,
                audio_path: audio_path.to_string_lossy().into_owned(),
            });
        }

        match command_receiver.recv_timeout(interval) {
            Ok(SyncCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn sync_workflow(
    client: &ComfyUiClient,
    config: &SyncConfig,
    audio_file: &RemoteFile,
    workspace: &Path,
    audio_filename: &str,
) -> Result<bool, SyncError> {
    let workflow_path = workflow_local_path(workspace, audio_filename);
    if !workflow_path.exists() {
        let workflow_file = RemoteFile {
            name: format!("{}.workflow.json", audio_file.name),
            path: format!("{}.workflow.json", audio_file.path),
            modified: audio_file.modified,
        };
        if client.download_optional_file(config, &workflow_file, &workflow_path)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn workflow_local_path(workspace: &Path, audio_filename: &str) -> PathBuf {
    metadata::workflow_path(workspace, &workspace.join(audio_filename))
        .expect("audio filename must be inside workspace")
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

fn load_download_index(workspace: &Path) -> Result<DownloadIndex, SyncError> {
    match fs::read_to_string(download_index_path(workspace)) {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|error| SyncError::Response(error.to_string()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DownloadIndex::default()),
        Err(error) => Err(SyncError::Io(error)),
    }
}

fn save_download_index(workspace: &Path, index: &DownloadIndex) -> Result<(), SyncError> {
    fs::create_dir_all(workspace.join(".adbstudio")).map_err(SyncError::Io)?;
    let contents = serde_json::to_string_pretty(index)
        .map_err(|error| SyncError::Response(error.to_string()))?;
    fs::write(download_index_path(workspace), contents).map_err(SyncError::Io)
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
        let mut files: Vec<RemoteFile> = response.json().map_err(SyncError::Request)?;
        if files
            .iter()
            .any(|file| file.name.trim().is_empty() || file.path.trim().is_empty())
        {
            return Err(SyncError::Response(
                "ComfyUI returned a file without a name or path".to_string(),
            ));
        }
        sort_remote_files(&mut files);
        Ok(files)
    }

    pub fn upload_workflow(
        &self,
        config: &SyncConfig,
        workflow: &serde_json::Value,
    ) -> Result<(), SyncError> {
        validate_request_config(config)?;
        let response = self
            .client
            .post(format!("{}/adb-music-player/workflow", config.url))
            .json(workflow)
            .send()
            .map_err(SyncError::Request)?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            let detail = body.trim();
            return Err(SyncError::Response(if detail.is_empty() {
                format!("ComfyUI rejected workflow ({status})")
            } else {
                format!("ComfyUI rejected workflow ({status}): {detail}")
            }));
        }
        Ok(())
    }

    pub fn run_workflow(
        &self,
        config: &SyncConfig,
        workflow: &serde_json::Value,
        workspace: &Path,
        output_directory: &Path,
        output_stem: &str,
        cancelled: &std::sync::atomic::AtomicBool,
        updates: &Sender<WorkflowRunUpdate>,
    ) -> Result<(), SyncError> {
        validate_request_config(config)?;
        fs::create_dir_all(output_directory).map_err(SyncError::Io)?;
        let client_id = format!(
            "adb-studio-{}-{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let prompt_response = self
            .client
            .post(format!("{}/prompt", config.url))
            .json(&serde_json::json!({ "prompt": workflow, "client_id": client_id }))
            .send()
            .map_err(SyncError::Request)?;
        if !prompt_response.status().is_success() {
            return Err(response_error(prompt_response, "ComfyUI rejected prompt"));
        }
        let prompt: serde_json::Value = prompt_response.json().map_err(SyncError::Request)?;
        let prompt_id = prompt
            .get("prompt_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| SyncError::Response("ComfyUI did not return a prompt id".into()))?
            .to_string();

        let history = loop {
            if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                self.interrupt(config)?;
                let _ = updates.send(WorkflowRunUpdate::Cancelled);
                return Ok(());
            }
            if let Some((progress, step)) = self.progress(config)? {
                let _ = updates.send(WorkflowRunUpdate::Progress { progress, step });
            } else {
                let _ = updates.send(WorkflowRunUpdate::Progress {
                    progress: 0.05,
                    step: "Waiting for ComfyUI".into(),
                });
            }
            let response = self
                .client
                .get(format!("{}/history/{}", config.url, prompt_id))
                .send()
                .map_err(SyncError::Request)?;
            if response.status().is_success() {
                let value: serde_json::Value = response.json().map_err(SyncError::Request)?;
                if value.get(&prompt_id).is_some() {
                    break value;
                }
            }
            thread::sleep(API_POLL_INTERVAL);
        };

        let output = find_audio_output(
            history
                .get(&prompt_id)
                .and_then(|entry| entry.get("outputs"))
                .ok_or_else(|| SyncError::Response("ComfyUI returned no outputs".into()))?,
        )
        .ok_or_else(|| SyncError::Response("ComfyUI returned no audio output".into()))?;
        let audio_path =
            output_directory.join(format!("{output_stem}.{}", output_file_extension(&output)));
        self.download_view(config, &output, &audio_path)?;
        let workflow_path = metadata::workflow_path(workspace, &audio_path)
            .ok_or_else(|| SyncError::Response("Output path is outside the workspace".into()))?;
        if let Some(parent) = workflow_path.parent() {
            fs::create_dir_all(parent).map_err(SyncError::Io)?;
        }
        let contents = serde_json::to_string_pretty(workflow)
            .map_err(|error| SyncError::Response(error.to_string()))?;
        fs::write(&workflow_path, contents).map_err(SyncError::Io)?;
        let _ = updates.send(WorkflowRunUpdate::Finished {
            audio_path,
            workflow_path,
        });
        Ok(())
    }

    fn interrupt(&self, config: &SyncConfig) -> Result<(), SyncError> {
        let response = self
            .client
            .post(format!("{}/interrupt", config.url))
            .send()
            .map_err(SyncError::Request)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(response_error(
                response,
                "ComfyUI could not cancel the prompt",
            ))
        }
    }

    fn progress(&self, config: &SyncConfig) -> Result<Option<(f32, String)>, SyncError> {
        let response = self
            .client
            .get(format!("{}/progress", config.url))
            .send()
            .map_err(SyncError::Request)?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let value: serde_json::Value = response.json().map_err(SyncError::Request)?;
        let max = value
            .get("max")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0)
            .max(1.0);
        let current = value
            .get("value")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        let step = value
            .get("node")
            .and_then(serde_json::Value::as_str)
            .map(|node| format!("Running node {node}"))
            .unwrap_or_else(|| "Running workflow".into());
        Ok(Some(((current / max).clamp(0.0, 1.0) as f32, step)))
    }

    fn download_view(
        &self,
        config: &SyncConfig,
        output: &AudioOutput,
        destination: &Path,
    ) -> Result<(), SyncError> {
        let filename = destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("output.audio");
        let source_extension = Path::new(&output.filename)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("audio")
            .to_ascii_lowercase();
        let temporary_directory = destination.parent().unwrap_or_else(|| Path::new("."));
        let source_temporary =
            temporary_download_path(temporary_directory, &format!("source.{source_extension}"));
        let output_temporary = temporary_download_path(temporary_directory, filename);
        let result = (|| {
            let mut response = self
                .client
                .get(format!("{}/view", config.url))
                .query(&[
                    ("filename", output.filename.as_str()),
                    ("subfolder", output.subfolder.as_str()),
                    ("type", output.output_type.as_str()),
                ])
                .send()
                .map_err(SyncError::Request)?
                .error_for_status()
                .map_err(SyncError::Request)?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&source_temporary)
                .map_err(SyncError::Io)?;
            response.copy_to(&mut file).map_err(SyncError::Request)?;
            file.flush().map_err(SyncError::Io)?;
            file.sync_all().map_err(SyncError::Io)?;
            if !source_extension.eq_ignore_ascii_case("wav") {
                fs::rename(&source_temporary, destination).map_err(SyncError::Io)?;
            } else {
                let status = Command::new("ffmpeg")
                    .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
                    .arg(&source_temporary)
                    .args(["-c:a", "flac"])
                    .arg(&output_temporary)
                    .status()
                    .map_err(|error| {
                        SyncError::Response(format!("could not start ffmpeg: {error}"))
                    })?;
                if !status.success() {
                    return Err(SyncError::Response(format!("ffmpeg exited with {status}")));
                }
                fs::rename(&output_temporary, destination).map_err(SyncError::Io)?;
                fs::remove_file(&source_temporary).map_err(SyncError::Io)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&source_temporary);
            let _ = fs::remove_file(&output_temporary);
        }
        result
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

    fn download_optional_file(
        &self,
        config: &SyncConfig,
        file: &RemoteFile,
        destination: &Path,
    ) -> Result<bool, SyncError> {
        validate_request_config(config)?;
        let filename = destination
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty() && *name != "." && *name != "..")
            .ok_or_else(|| SyncError::Response("Invalid workflow filename".to_string()))?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(SyncError::Io)?;
        }
        let temporary_path = temporary_download_path(
            destination.parent().unwrap_or_else(|| Path::new(".")),
            filename,
        );
        let result = (|| {
            let response = self
                .client
                .get(format!("{}/adb-music-player/audio-download", config.url))
                .query(&[("path", file.path.as_str())])
                .send()
                .map_err(SyncError::Request)?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(false);
            }
            let mut response = response.error_for_status().map_err(SyncError::Request)?;
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
            fs::rename(&temporary_path, destination).map_err(SyncError::Io)?;
            Ok(true)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        result
    }
}

#[derive(Clone, Debug)]
struct AudioOutput {
    filename: String,
    subfolder: String,
    output_type: String,
}

fn output_file_extension(output: &AudioOutput) -> String {
    let extension = Path::new(&output.filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("audio");
    if extension.eq_ignore_ascii_case("wav") {
        "flac".into()
    } else {
        extension.to_ascii_lowercase()
    }
}

fn find_audio_output(value: &serde_json::Value) -> Option<AudioOutput> {
    if let Some(object) = value.as_object() {
        if let Some(filename) = object.get("filename").and_then(serde_json::Value::as_str) {
            let extension = Path::new(filename)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase());
            if matches!(
                extension.as_deref(),
                Some("flac" | "wav" | "mp3" | "ogg" | "opus")
            ) {
                return Some(AudioOutput {
                    filename: filename.into(),
                    subfolder: object
                        .get("subfolder")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    output_type: object
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("output")
                        .into(),
                });
            }
        }
        for child in object.values() {
            if let Some(output) = find_audio_output(child) {
                return Some(output);
            }
        }
    } else if let Some(array) = value.as_array() {
        for child in array {
            if let Some(output) = find_audio_output(child) {
                return Some(output);
            }
        }
    }
    None
}

fn response_error(response: reqwest::blocking::Response, prefix: &str) -> SyncError {
    let status = response.status();
    let body = response.text().unwrap_or_default();
    let detail = body.trim();
    SyncError::Response(if detail.is_empty() {
        format!("{prefix} ({status})")
    } else {
        format!("{prefix} ({status}): {detail}")
    })
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

fn sort_remote_files(files: &mut [RemoteFile]) {
    files.sort_by(|left, right| right.modified.cmp(&left.modified));
}

fn progress_with_index(
    files: &[RemoteFile],
    destination: &Path,
    config: &SyncConfig,
    download_index: &DownloadIndex,
) -> Result<SyncProgress, SyncError> {
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
    let recorded = files
        .iter()
        .filter(|file| download_index.contains_completed(config, file))
        .count();
    Ok(SyncProgress {
        present: present.max(recorded),
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
        ensure_destination, load_download_index, progress_with_index, save_download_index,
        temporary_download_path, DownloadIndex, RemoteFile, SyncConfig, DEFAULT_INTERVAL_MS,
        DEFAULT_LOCAL_DIRECTORY, DEFAULT_REMOTE_DIRECTORY,
    };
    use crate::metadata;
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

    #[test]
    fn temporary_download_name_is_hidden_and_keeps_original_name() {
        let path = temporary_download_path(Path::new("/workspace/downloads"), "myfile.opus");
        let filename = path.file_name().unwrap().to_string_lossy();
        assert!(filename.starts_with(".myfile.opus."));
        assert_ne!(filename, "myfile.opus");
    }

    #[test]
    fn workflow_path_mirrors_audio_path_inside_private_directory() {
        let workspace = Path::new("/workspace");
        assert_eq!(
            super::workflow_local_path(workspace, "folder1/folder2/song.mp3"),
            workspace.join(".adbstudio/workflows/folder1/folder2/song.mp3.workflow.json")
        );
    }

    #[test]
    fn workflow_path_does_not_modify_workspace_index() {
        let workspace = tempfile_directory();
        let audio_path = workspace.join("downloads/song.mp3");
        assert_eq!(
            metadata::workflow_path(&workspace, &audio_path),
            Some(workspace.join(".adbstudio/workflows/downloads/song.mp3.workflow.json"))
        );
        assert!(!workspace.join(".adbstudio/index.json").exists());

        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn progress_counts_matching_basenames() {
        let directory = tempfile_directory();
        fs::write(directory.join("song.mp3"), b"audio").unwrap();
        let files = vec![
            RemoteFile {
                name: "nested/song.mp3".to_string(),
                path: "/remote/song.mp3".to_string(),
                modified: 2,
            },
            RemoteFile {
                name: "other.mp3".to_string(),
                path: "/remote/other.mp3".to_string(),
                modified: 1,
            },
        ];
        assert_eq!(
            progress_with_index(
                &files,
                &directory,
                &SyncConfig::default(),
                &DownloadIndex::default()
            )
            .unwrap()
            .present,
            1
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn remote_files_are_sorted_newest_first() {
        let mut files = vec![
            RemoteFile {
                name: "older.mp3".to_string(),
                path: "/remote/older.mp3".to_string(),
                modified: 1,
            },
            RemoteFile {
                name: "newer.mp3".to_string(),
                path: "/remote/newer.mp3".to_string(),
                modified: 2,
            },
        ];
        super::sort_remote_files(&mut files);
        assert_eq!(files[0].name, "newer.mp3");
        assert_eq!(files[1].name, "older.mp3");
    }

    #[test]
    fn completed_record_counts_after_local_file_is_removed() {
        let workspace = tempfile_directory();
        let config = SyncConfig {
            url: "https://comfy.example".to_string(),
            ..SyncConfig::default()
        };
        let file = RemoteFile {
            name: "song.mp3".to_string(),
            path: "/output/audio/song.mp3".to_string(),
            modified: 0,
        };
        let mut index = DownloadIndex::default();
        index.record_completed(&config, &file, 1234);
        save_download_index(&workspace, &index).unwrap();

        let loaded = load_download_index(&workspace).unwrap();
        assert_eq!(loaded, index);
        assert_eq!(loaded.downloads[0].status, "completed");
        assert_eq!(loaded.downloads[0].size, 1234);
        assert_eq!(
            progress_with_index(&[file], &workspace, &config, &loaded)
                .unwrap()
                .present,
            1
        );
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
