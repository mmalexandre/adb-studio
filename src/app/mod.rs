use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{mpsc, Arc},
};

use notify::RecommendedWatcher;
use slint::VecModel;

use crate::{
    audio::{
        conversion::{ConversionJob, ConversionUpdate},
        loader::{Result as AudioLoadResult, State as AudioLoadState},
    },
    settings::AppSettings,
    sync::WorkflowRunUpdate,
    workspace::file_system::TreeState,
};

pub mod conversion_controller;
pub mod env;
pub mod metadata_pane_controller;
pub mod playback_controller;
pub mod sync_ui_controller;
pub mod tree_controller;
pub mod window_controller;
pub mod workflow_controller;

pub struct AppState {
    pub settings: Rc<RefCell<AppSettings>>,
    pub tree_state: Rc<RefCell<Option<TreeState>>>,
    pub audio_folder: Rc<RefCell<Option<PathBuf>>>,
    pub workflow_files: Rc<RefCell<Vec<(String, String)>>>,
    pub edited_workflow: Rc<RefCell<Option<serde_json::Value>>>,
    pub edited_workflow_path: Rc<RefCell<Option<PathBuf>>>,
    pub workflow_loading: Rc<RefCell<bool>>,
    pub loaded_workflow_path: Rc<RefCell<Option<PathBuf>>>,
    pub recreate_workflow_pending: Rc<RefCell<bool>>,
    pub workspace_watcher: Rc<RefCell<Option<RecommendedWatcher>>>,
    pub audio_model: Rc<RefCell<Option<Rc<VecModel<crate::AudioRow>>>>>,
    pub sync_controller: Rc<RefCell<crate::sync::SyncController>>,
    pub comment_editor_original: Rc<RefCell<Option<crate::metadata::AudioComment>>>,
    pub comment_editor_duration: Rc<RefCell<f32>>,
    pub last_button_click: Rc<RefCell<Option<(PathBuf, std::time::Instant)>>>,
    pub conversion_receiver: Rc<RefCell<Option<mpsc::Receiver<ConversionUpdate>>>>,
    pub conversion_cancelled: Rc<RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>>,
    pub conversion_jobs: Rc<RefCell<Vec<ConversionJob>>>,
    pub conversion_temp_root: Rc<RefCell<Option<PathBuf>>>,
    pub conversion_target: Rc<RefCell<Option<PathBuf>>>,
    pub conversion_model: Rc<RefCell<Option<Rc<VecModel<crate::ConversionRow>>>>>,
    pub workflow_run_sender: mpsc::Sender<WorkflowRunUpdate>,
    pub workflow_run_receiver: Rc<RefCell<Option<mpsc::Receiver<WorkflowRunUpdate>>>>,
    pub workflow_run_cancelled: Rc<RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>>,
    pub audio_result_receiver: Rc<RefCell<Option<mpsc::Receiver<AudioLoadResult>>>>,
    pub audio_load_state: Arc<std::sync::Mutex<AudioLoadState>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        let (workflow_run_sender, workflow_run_receiver) = mpsc::channel::<WorkflowRunUpdate>();
        let (audio_result_sender, audio_result_receiver) = mpsc::channel::<AudioLoadResult>();
        let audio_load_state = Arc::new(std::sync::Mutex::new(AudioLoadState {
            folder: PathBuf::new(),
            paths: Vec::new(),
            requested_range: None,
            generated: std::collections::HashSet::new(),
            loading: std::collections::HashSet::new(),
            generation: 0,
            cancellation_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            running: false,
            completed: 0,
            total: 0,
            result_sender: audio_result_sender,
        }));

        Self {
            settings: Rc::new(RefCell::new(AppSettings::default())),
            tree_state: Rc::new(RefCell::new(None)),
            audio_folder: Rc::new(RefCell::new(None)),
            workflow_files: Rc::new(RefCell::new(Vec::new())),
            edited_workflow: Rc::new(RefCell::new(None)),
            edited_workflow_path: Rc::new(RefCell::new(None)),
            workflow_loading: Rc::new(RefCell::new(false)),
            loaded_workflow_path: Rc::new(RefCell::new(None)),
            recreate_workflow_pending: Rc::new(RefCell::new(false)),
            workspace_watcher: Rc::new(RefCell::new(None)),
            audio_model: Rc::new(RefCell::new(None)),
            sync_controller: Rc::new(RefCell::new(crate::sync::SyncController::new())),
            comment_editor_original: Rc::new(RefCell::new(None)),
            comment_editor_duration: Rc::new(RefCell::new(0.0_f32)),
            last_button_click: Rc::new(RefCell::new(None)),
            conversion_receiver: Rc::new(RefCell::new(None)),
            conversion_cancelled: Rc::new(RefCell::new(None)),
            conversion_jobs: Rc::new(RefCell::new(Vec::new())),
            conversion_temp_root: Rc::new(RefCell::new(None)),
            conversion_target: Rc::new(RefCell::new(None)),
            conversion_model: Rc::new(RefCell::new(None)),
            workflow_run_sender,
            workflow_run_receiver: Rc::new(RefCell::new(Some(workflow_run_receiver))),
            workflow_run_cancelled: Rc::new(RefCell::new(None)),
            audio_result_receiver: Rc::new(RefCell::new(Some(audio_result_receiver))),
            audio_load_state,
        }
    }
}
