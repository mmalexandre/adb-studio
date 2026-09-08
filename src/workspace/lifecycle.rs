use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{atomic::Ordering, mpsc, Arc, Mutex},
};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use slint::{ModelRc, VecModel};

use crate::{
    audio::{loader::State as AudioLoadState, playback::PlaybackEngine},
    settings::{self, AppSettings},
    sync::{self, SyncController},
    workspace::{
        file_system,
        file_system::TreeState,
        library,
        tree_nav,
        workflow::{clear_workflow, scan_json_files},
    },
    AudioRow, MainWindow, WorkflowFileRow,
};

pub fn set_workspace(
    window: &MainWindow,
    folder: PathBuf,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    workflow_files: &Rc<RefCell<Vec<(String, String)>>>,
    sync_controller: &Rc<RefCell<SyncController>>,
    workspace_watcher: &Rc<RefCell<Option<RecommendedWatcher>>>,
    workspace_change_sender: &mpsc::Sender<Vec<PathBuf>>,
    edited_workflow: &Rc<RefCell<Option<serde_json::Value>>>,
    edited_workflow_path: &Rc<RefCell<Option<PathBuf>>>,
    loaded_workflow_path: &Rc<RefCell<Option<PathBuf>>>,
) {
    *workspace_watcher.borrow_mut() = None;
    match notify::recommended_watcher({
        let workspace_change_sender = workspace_change_sender.clone();
        move |result: notify::Result<notify::Event>| {
            let Ok(event) = result else {
                return;
            };
            let changed_paths = event
                .paths
                .into_iter()
                .filter(|path| !is_internal_path(path) || is_workflow_path(path))
                .collect::<Vec<_>>();
            if !changed_paths.is_empty() {
                let _ = workspace_change_sender.send(changed_paths);
            }
        }
    }) {
        Ok(mut watcher) => {
            if watcher.watch(&folder, RecursiveMode::Recursive).is_ok() {
                *workspace_watcher.borrow_mut() = Some(watcher);
            } else {
                window.set_audio_error("Unable to watch workspace files".into());
            }
        }
        Err(_) => window.set_audio_error("Unable to watch workspace files".into()),
    }
    *audio_folder.borrow_mut() = Some(folder.clone());
    *edited_workflow.borrow_mut() = None;
    *edited_workflow_path.borrow_mut() = None;
    *loaded_workflow_path.borrow_mut() = None;
    let folder_name = folder
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| folder.to_str().unwrap_or("Workspace"))
        .to_string();

    window.set_folder_name(folder_name.into());
    window.set_has_folder(true);
    let scanned_workflows = scan_json_files(&folder);
    *workflow_files.borrow_mut() = scanned_workflows.clone();
    window.set_workflow_json_files(ModelRc::new(VecModel::from(
        scanned_workflows
            .into_iter()
            .map(|(name, path)| WorkflowFileRow {
                name: name.into(),
                path: path.into(),
            })
            .collect::<Vec<_>>(),
    )));
    window.set_selected_workflow("".into());
    window.set_audio_file_name("".into());
    clear_workflow(window);

    settings.borrow_mut().last_folder = Some(folder.to_string_lossy().into_owned());
    let settings_snapshot = settings.borrow().clone();
    settings::save(&settings_snapshot);

    if let Some(config) = sync::load_config(&folder) {
        window.set_comfyui_sync_url(config.url.clone().into());
        window.set_comfyui_sync_remote_directory(config.remote_directory.clone().into());
        window.set_comfyui_sync_local_directory(config.local_directory.clone().into());
        window.set_comfyui_sync_interval(config.interval_ms.to_string().into());
        sync_controller.borrow_mut().start(folder.clone(), config);
        window.set_comfyui_sync_active(true);
        window.set_comfyui_sync_error_state(false);
        window.set_comfyui_sync_status("Starting".into());
    } else {
        sync_controller.borrow_mut().stop();
        window.set_comfyui_sync_active(false);
        window.set_comfyui_sync_error_state(false);
        window.set_comfyui_sync_status("Not configured".into());
        window.set_comfyui_sync_present(0);
        window.set_comfyui_sync_total(0);
    }

    let mut new_tree_state = TreeState::new(folder.clone());
    if let Some(selected_path) = settings_snapshot.last_selected_path.as_deref().map(PathBuf::from) {
        if selected_path != folder
            && selected_path.exists()
            && selected_path.strip_prefix(&folder).is_ok()
        {
            new_tree_state.select_and_expand(&selected_path);
            window.set_selected_name(
                selected_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .into(),
            );
        } else {
            window.set_selected_name("".into());
        }
    } else {
        window.set_selected_name("".into());
    }
    *tree_state.borrow_mut() = Some(new_tree_state);
    tree_nav::refresh_tree(window, tree_state);
    let audio_view_folder = settings_snapshot
        .last_selected_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|selected_path| {
            selected_path != &folder
                && selected_path.exists()
                && selected_path.strip_prefix(&folder).is_ok()
        })
        .and_then(|selected_path| {
            if selected_path.is_dir() {
                Some(selected_path)
            } else {
                selected_path.parent().map(Path::to_path_buf)
            }
        })
        .unwrap_or(folder);
    library::refresh_audio(window, audio_folder, audio_model, audio_load_state, audio_view_folder);
}

pub fn close_workspace(
    window: &MainWindow,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    workflow_files: &Rc<RefCell<Vec<(String, String)>>>,
    sync_controller: &Rc<RefCell<SyncController>>,
    playback: &Rc<RefCell<Option<PlaybackEngine>>>,
    workspace_watcher: &Rc<RefCell<Option<RecommendedWatcher>>>,
    edited_workflow: &Rc<RefCell<Option<serde_json::Value>>>,
    edited_workflow_path: &Rc<RefCell<Option<PathBuf>>>,
    loaded_workflow_path: &Rc<RefCell<Option<PathBuf>>>,
) {
    *workspace_watcher.borrow_mut() = None;
    sync_controller.borrow_mut().stop();
    if let Some(engine) = playback.borrow_mut().as_mut() {
        engine.stop();
    }
    let mut state = audio_load_state.lock().unwrap();
    state.folder = PathBuf::new();
    state.paths.clear();
    state.requested_range = None;
    state.generated.clear();
    state.loading.clear();
    state.generation += 1;
    state.cancellation_generation.store(state.generation, Ordering::Release);
    state.completed = 0;
    state.total = 0;
    drop(state);
    *tree_state.borrow_mut() = None;
    *audio_folder.borrow_mut() = None;
    *audio_model.borrow_mut() = None;
    *edited_workflow.borrow_mut() = None;
    *edited_workflow_path.borrow_mut() = None;
    *loaded_workflow_path.borrow_mut() = None;
    workflow_files.borrow_mut().clear();

    let mut settings = settings.borrow_mut();
    settings.last_folder = None;
    settings.last_selected_path = None;
    settings::save(&settings);
    drop(settings);

    window.set_has_folder(false);
    window.set_folder_name("".into());
    window.set_tree_rows(ModelRc::new(VecModel::from(Vec::new())));
    window.set_tree_selection_count(0);
    window.set_audio_breadcrumbs(ModelRc::new(VecModel::from(Vec::new())));
    window.set_audio_rows(ModelRc::new(VecModel::from(Vec::new())));
    window.set_selected_name("".into());
    window.set_active_audio_path("".into());
    window.set_audio_file_name("".into());
    window.set_audio_playing(false);
    window.set_audio_loading(false);
    window.set_audio_completed(0);
    window.set_audio_total(0);
    window.set_audio_error("".into());
    window.set_workflow_json_files(ModelRc::new(VecModel::from(Vec::new())));
    window.set_selected_workflow("".into());
    clear_workflow(window);
    window.set_comfyui_sync_active(false);
    window.set_comfyui_sync_error_state(false);
    window.set_comfyui_sync_status("Not configured".into());
    window.set_comfyui_sync_present(0);
    window.set_comfyui_sync_total(0);
}

pub fn refresh_workspace(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    workflow_files: &Rc<RefCell<Vec<(String, String)>>>,
    changed_paths: &[PathBuf],
) {
    let Some(folder) = audio_folder.borrow().clone() else {
        return;
    };
    let audio_view_folder = {
        let mut state_ref = tree_state.borrow_mut();
        let Some(state) = state_ref.as_mut() else {
            return;
        };
        for path in state.selected_paths() {
            if !path.exists() {
                state.remove_path(&path);
            }
        }
        state
            .selected
            .as_ref()
            .filter(|path| path.exists() && path.strip_prefix(&folder).is_ok())
            .map(|path| {
                if path.is_dir() {
                    path.clone()
                } else {
                    path.parent().unwrap_or(&folder).to_path_buf()
                }
            })
            .unwrap_or_else(|| folder.clone())
    };
    tree_nav::refresh_tree(window, tree_state);

    let scanned_workflows = scan_json_files(&folder);
    *workflow_files.borrow_mut() = scanned_workflows.clone();
    window.set_workflow_json_files(ModelRc::new(VecModel::from(
        scanned_workflows
            .into_iter()
            .map(|(name, path)| WorkflowFileRow {
                name: name.into(),
                path: path.into(),
            })
            .collect::<Vec<_>>(),
    )));
    if changed_paths.iter().any(|path| {
        (path.parent() == Some(audio_view_folder.as_path())
            && file_system::FileKind::from_path(path) == file_system::FileKind::Audio)
            || workflow_change_affects_folder(path, &folder, &audio_view_folder)
    }) {
        library::refresh_audio_for_changes(
            window,
            audio_folder,
            audio_model,
            audio_load_state,
            audio_view_folder,
            changed_paths,
        );
    }
}

fn is_internal_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == ".adbstudio")
}

fn is_workflow_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".workflow.json"))
}

fn workflow_change_affects_folder(path: &Path, workspace: &Path, folder: &Path) -> bool {
    let workflows_root = workspace.join(".adbstudio").join("workflows");
    let Ok(relative_path) = path.strip_prefix(workflows_root) else {
        return false;
    };
    let Some(file_name) = relative_path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(audio_name) = file_name.strip_suffix(".workflow.json") else {
        return false;
    };
    let relative_folder = relative_path.parent().unwrap_or_else(|| Path::new(""));
    workspace.join(relative_folder) == folder
        && file_system::FileKind::from_path(Path::new(audio_name)) == file_system::FileKind::Audio
}
