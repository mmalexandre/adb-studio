use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::mpsc,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use audio::conversion::{self, ConversionJob, ConversionUpdate};
use audio::playback::PlaybackEngine;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

mod audio;
mod metadata;
mod settings;
mod sync;
mod workspace;
use audio::{loader, waveform};
use metadata::AudioComment;
use workspace::file_system::{self, TreeState};

slint::include_modules!();

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_NUMBER: &str = env!("ADB_BUILD_NUMBER");
use audio::loader::State as AudioLoadState;
use audio::view::{
    comment_rows, format_duration, format_seconds, scroll_to_path as scroll_audio_to_path,
    select_audio_path, select_comment, update_audio_loading_rows, update_audio_rows,
    update_comment_model,
};
use settings::AppSettings;
use sync::{ComfyUiClient, SyncConfig, SyncController, SyncEvent, WorkflowRunUpdate};
use workspace::workflow::{clear_workflow, load_workflow_for_audio, scan_json_files};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    sync_cursor_environment();
    let window = MainWindow::new()?;
    slint::set_xdg_app_id("com.adbstudio.AdbStudio")?;
    let settings = Rc::new(RefCell::new(settings::load()));
    settings::restore_window(&window, &mut settings.borrow_mut());
    let tree_state: Rc<RefCell<Option<TreeState>>> = Rc::new(RefCell::new(None));
    let audio_folder: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let workflow_files: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let edited_workflow: Rc<RefCell<Option<serde_json::Value>>> = Rc::new(RefCell::new(None));
    let workflow_loading = Rc::new(RefCell::new(false));
    let recreate_workflow_pending = Rc::new(RefCell::new(false));
    let workspace_watcher: Rc<RefCell<Option<RecommendedWatcher>>> = Rc::new(RefCell::new(None));
    let (workspace_change_sender, workspace_change_receiver) = mpsc::channel::<Vec<PathBuf>>();
    let audio_model: Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>> = Rc::new(RefCell::new(None));
    let sync_controller = Rc::new(RefCell::new(SyncController::new()));
    let comment_editor_original: Rc<RefCell<Option<AudioComment>>> = Rc::new(RefCell::new(None));
    let comment_editor_duration = Rc::new(RefCell::new(0.0_f32));
    let last_button_click: Rc<RefCell<Option<(PathBuf, Instant)>>> = Rc::new(RefCell::new(None));
    let conversion_receiver: Rc<RefCell<Option<mpsc::Receiver<ConversionUpdate>>>> =
        Rc::new(RefCell::new(None));
    let conversion_cancelled: Rc<RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>> =
        Rc::new(RefCell::new(None));
    let conversion_jobs: Rc<RefCell<Vec<ConversionJob>>> = Rc::new(RefCell::new(Vec::new()));
    let conversion_temp_root: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let conversion_target: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let conversion_model: Rc<RefCell<Option<Rc<VecModel<ConversionRow>>>>> =
        Rc::new(RefCell::new(None));
    let (workflow_run_sender, workflow_run_receiver) = mpsc::channel::<WorkflowRunUpdate>();
    let workflow_run_receiver: Rc<RefCell<Option<mpsc::Receiver<WorkflowRunUpdate>>>> =
        Rc::new(RefCell::new(Some(workflow_run_receiver)));
    let workflow_run_cancelled: Rc<RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>> =
        Rc::new(RefCell::new(None));
    let (playback, playback_error) = match PlaybackEngine::new() {
        Ok(engine) => (Rc::new(RefCell::new(Some(engine))), None),
        Err(error) => (Rc::new(RefCell::new(None)), Some(error)),
    };
    if let Some(error) = playback_error {
        window.set_audio_error(error.into());
    }
    let (audio_result_sender, audio_result_receiver) = mpsc::channel();
    let audio_load_state = Arc::new(Mutex::new(AudioLoadState {
        folder: PathBuf::new(),
        paths: Vec::new(),
        requested_range: None,
        generated: std::collections::HashSet::new(),
        loading: std::collections::HashSet::new(),
        generation: 0,
        cancellation_generation: Arc::new(AtomicU64::new(0)),
        running: false,
        completed: 0,
        total: 0,
        result_sender: audio_result_sender,
    }));
    window.set_build_number(BUILD_NUMBER.into());
    window.set_light_theme(settings.borrow().light_theme);
    window.set_theme_index(if settings.borrow().light_theme { 1 } else { 0 });
    window.set_loop_mode(settings.borrow().loop_mode);
    window.set_auto_play_new_tracks(settings.borrow().auto_play_new_tracks);
    window.set_seek_seconds(settings.borrow().seek_seconds.round() as i32);
    window.set_sort_order(settings.borrow().sort_order);
    window.set_shortcut_fullscreen(settings.borrow().shortcut_fullscreen);
    window.set_shortcut_metadata(settings.borrow().shortcut_metadata);
    window.set_shortcut_play_pause(settings.borrow().shortcut_play_pause);
    window.set_shortcut_navigate_up(settings.borrow().shortcut_navigate_up);
    window.set_shortcut_navigate_down(settings.borrow().shortcut_navigate_down);
    window.set_shortcut_cancel_edit(settings.borrow().shortcut_cancel_edit);
    window.set_shortcut_seek_backward(settings.borrow().shortcut_seek_backward);
    window.set_shortcut_seek_forward(settings.borrow().shortcut_seek_forward);
    window.set_shortcut_trash(settings.borrow().shortcut_trash);
    window.set_hide_tips_of_the_day(settings.borrow().hide_tips_of_the_day);
    window.set_tips_visible(!settings.borrow().hide_tips_of_the_day);
    window.set_audio_volume(1.0);
    window.set_left_pane_width(settings.borrow().left_pane_width.into());
    window.set_metadata_pane_height(settings.borrow().metadata_pane_height.into());
    window.set_metadata_visible(settings.borrow().metadata_visible);
    window.set_comment_background_hex(settings.borrow().comment_background_color.clone().into());
    window.set_comment_text_hex(settings.borrow().comment_text_color.clone().into());
    window.set_comment_background_color(settings::parse_color(
        &settings.borrow().comment_background_color,
        slint::Color::from_argb_u8(255, 0, 0, 0),
    ));
    window.set_comment_text_color(settings::parse_color(
        &settings.borrow().comment_text_color,
        slint::Color::from_argb_u8(255, 255, 255, 255),
    ));

    fn recreate_workflow(
        window: &MainWindow,
        folder: &Path,
        config: &SyncConfig,
        edited_workflow: &Rc<RefCell<Option<serde_json::Value>>>,
    ) {
        let audio_path = window.get_selected_audio_path().to_string();
        if audio_path.is_empty() {
            window.set_audio_error("Select an audio file with a workflow first".into());
            return;
        }
        let Some(workflow_path) = metadata::workflow_path(folder, Path::new(&audio_path)) else {
            window.set_audio_error("Audio file is outside the workspace".into());
            return;
        };
        if !workflow_path.is_file() {
            window.set_audio_error("Select a workflow before recreating it".into());
            return;
        }
        let mut workflow = if let Some(workflow) = edited_workflow.borrow().clone() {
            workflow
        } else {
            match fs::read_to_string(&workflow_path)
                .map_err(|error| error.to_string())
                .and_then(|contents| {
                    serde_json::from_str::<serde_json::Value>(&contents)
                        .map_err(|error| error.to_string())
                }) {
                Ok(workflow) => workflow,
                Err(error) => {
                    window.set_audio_error(format!("Workflow JSON: {error}").into());
                    return;
                }
            }
        };
        for (field, value) in [
            ("bpm", window.get_workflow_bpm().to_string()),
            ("key", window.get_workflow_key().to_string()),
            ("seed", window.get_workflow_seed().to_string()),
            ("prompt", window.get_workflow_prompt().to_string()),
            ("lyrics", window.get_workflow_lyrics().to_string()),
        ] {
            metadata::comfyui::update_metadata(&mut workflow, field, &value);
        }
        match ComfyUiClient::new().and_then(|client| client.upload_workflow(config, &workflow)) {
            Ok(()) => {
                *edited_workflow.borrow_mut() = None;
                window.set_workflow_modified(false);
                match open_comfyui_workflow(config) {
                    Ok(()) => window.set_audio_error("Workflow opened in ComfyUI".into()),
                    Err(error) => window
                        .set_audio_error(format!("ComfyUI opened upload failed: {error}").into()),
                }
            }
            Err(error) => window.set_audio_error(format!("ComfyUI: {error}").into()),
        }
    }

    fn ensure_edit_copy(
        window: &MainWindow,
        folder: &Path,
        edited_workflow: &Rc<RefCell<Option<serde_json::Value>>>,
    ) -> bool {
        if edited_workflow.borrow().is_some() {
            return true;
        }
        let audio_path = window.get_selected_audio_path().to_string();
        let Some(workflow_path) = metadata::workflow_path(folder, Path::new(&audio_path)) else {
            return false;
        };
        let Ok(contents) = fs::read_to_string(workflow_path) else {
            return false;
        };
        let Ok(workflow) = serde_json::from_str(&contents) else {
            return false;
        };
        *edited_workflow.borrow_mut() = Some(workflow);
        true
    }

    fn write_modified_workflow(
        workflow_path: &Path,
        workflow: &serde_json::Value,
    ) -> Result<PathBuf, String> {
        let filename = workflow_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("workflow");
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let modified_path = env::temp_dir().join(format!(
            "{filename}-{}-{timestamp}.workflow.json",
            std::process::id()
        ));
        let contents = serde_json::to_string_pretty(workflow).map_err(|error| error.to_string())?;
        fs::write(&modified_path, contents).map_err(|error| error.to_string())?;
        Ok(modified_path)
    }

    fn open_comfyui_workflow(config: &SyncConfig) -> Result<(), String> {
        let url = format!("{}/?adb-music-player=open-workflow", config.url);
        #[cfg(target_os = "linux")]
        let mut command = Command::new("xdg-open");
        #[cfg(target_os = "macos")]
        let mut command = Command::new("open");
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = Command::new("cmd");
            command.args(["/C", "start", ""]);
            command
        };
        command
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    {
        let playback = Rc::clone(&playback);
        window.on_volume_changed(move |volume| {
            if let Some(engine) = playback.borrow_mut().as_mut() {
                engine.set_volume(volume);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        let playback = Rc::clone(&playback);
        let comment_editor_original = Rc::clone(&comment_editor_original);
        let comment_editor_duration = Rc::clone(&comment_editor_duration);
        window.on_comment_range_requested(move |path, start, end| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let duration = comment_duration(&folder, &path, &playback);
            if duration <= 0.0 {
                window.set_audio_error("Unable to determine audio duration".into());
                return;
            }
            select_comment(&audio_model, &path, start, end);
            *comment_editor_original.borrow_mut() = None;
            *comment_editor_duration.borrow_mut() = duration;
            window.set_comment_editor_path(path.to_string_lossy().into_owned().into());
            window.set_comment_editor_start(format_seconds(start * duration).into());
            window.set_comment_editor_end(format_seconds(end * duration).into());
            window.set_comment_editor_text("".into());
            window.set_comment_editor_visible(true);
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        let edited_workflow = Rc::clone(&edited_workflow);
        let workflow_loading = Rc::clone(&workflow_loading);
        window.on_audio_row_selected(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            window.set_selected_audio_path(path.clone());
            let path = Path::new(path.as_str());
            *edited_workflow.borrow_mut() = None;
            window.set_workflow_modified(false);
            select_audio_path(&audio_model, path);
            select_tree_path(&window, &tree_state, &settings, path);
            if let Some(folder) = audio_folder.borrow().clone() {
                load_workflow_for_audio(&window, &folder, path, &workflow_loading);
            }
        });
    }

    window.on_tree_reveal_requested(move |path| {
        let path = PathBuf::from(path.as_str());
        if path.exists() {
            let _ = opener::reveal(path);
        }
    });

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        window.on_chevron_clicked(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let mut state_ref = tree_state.borrow_mut();
            let Some(state) = state_ref.as_mut() else {
                return;
            };
            state.toggle(&path);
            drop(state_ref);
            refresh_tree(&window, &tree_state);
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        window.on_breadcrumb_requested(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            if !path.is_dir() {
                return;
            }
            select_tree_path(&window, &tree_state, &settings, &path);
            refresh_audio(
                &window,
                &audio_folder,
                &audio_model,
                &audio_load_state,
                path,
            );
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        window.on_tree_trash_requested(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let source = PathBuf::from(path.as_str());
            if !source.exists() {
                return;
            }
            if let Err(error) = trash::delete(&source) {
                window.set_audio_error(format!("File operation: {error}").into());
                return;
            }
            let selection_removed = {
                let mut state_ref = tree_state.borrow_mut();
                let Some(state) = state_ref.as_mut() else {
                    return;
                };
                state.remove_path(&source)
            };
            if selection_removed {
                settings.borrow_mut().last_selected_path = None;
                let settings_snapshot = settings.borrow().clone();
                settings::save(&settings_snapshot);
                window.set_selected_name("".into());
            }
            window.set_audio_error("".into());
            refresh_tree(&window, &tree_state);
        });
    }

    {
        let target = Rc::clone(&conversion_target);
        let weak_window = window.as_weak();
        window.on_conversion_requested(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            if conversion::collect_files(&path).is_empty() {
                window.set_audio_error("No supported audio files found".into());
                return;
            }
            *target.borrow_mut() = Some(path);
            window.set_conversion_options_visible(true);
            window.set_audio_error("".into());
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        window.on_pin_requested(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(workspace) = audio_folder.borrow().clone() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(folder) = path.parent() else {
                return;
            };
            let already_pinned = audio_model
                .borrow()
                .as_ref()
                .and_then(|model| {
                    (0..model.row_count())
                        .filter_map(|index| model.row_data(index))
                        .find(|row| Path::new(row.path.as_str()) == path)
                })
                .is_some_and(|row| row.is_pinned);
            workspace::preferences::set_pinned_track(
                &workspace,
                folder,
                (!already_pinned).then_some(path.as_path()),
            );
            let pinned_path = (!already_pinned).then_some(path.as_path());
            if let Some(model) = audio_model.borrow().clone() {
                for index in 0..model.row_count() {
                    let Some(row) = model.row_data(index) else {
                        continue;
                    };
                    let is_pinned = !already_pinned && Path::new(row.path.as_str()) == path;
                    if row.is_pinned != is_pinned {
                        model.set_row_data(
                            index,
                            AudioRow {
                                path: row.path.clone(),
                                name: row.name,
                                modified_date: row.modified_date,
                                peaks: row.peaks,
                                is_loading: row.is_loading,
                                comments: row.comments,
                                differences: track_differences(
                                    &workspace,
                                    pinned_path,
                                    Path::new(row.path.as_str()),
                                ),
                                rating: row.rating,
                                is_pinned,
                                is_selected: row.is_selected,
                                is_active: row.is_active,
                                is_playing: row.is_playing,
                                progress: row.progress,
                                loop_enabled: row.loop_enabled,
                                selected_comment_start: row.selected_comment_start,
                                selected_comment_end: row.selected_comment_end,
                            },
                        );
                    }
                }
            }
            window.set_audio_error("".into());
        });
    }

    {
        let weak_window = window.as_weak();
        let target = Rc::clone(&conversion_target);
        let jobs_state = Rc::clone(&conversion_jobs);
        let model_state = Rc::clone(&conversion_model);
        let receiver_state = Rc::clone(&conversion_receiver);
        let cancelled_state = Rc::clone(&conversion_cancelled);
        let temp_state = Rc::clone(&conversion_temp_root);
        let playback = Rc::clone(&playback);
        let audio_folder = Rc::clone(&audio_folder);
        window.on_conversion_started(move |format, quality| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(target) = target.borrow().clone() else {
                return;
            };
            let Some(workspace) = audio_folder.borrow().clone() else {
                return;
            };
            let files = conversion::collect_files(&target);
            let source_set: HashSet<PathBuf> = files.iter().cloned().collect();
            let mut destinations = HashSet::new();
            for source in &files {
                let destination = source.with_extension(format.as_str());
                if !destinations.insert(destination.clone())
                    || (destination.exists() && !source_set.contains(&destination))
                {
                    window.set_audio_error("Conversion would overwrite an existing file".into());
                    return;
                }
            }
            let temp_root = workspace
                .join(".adbstudio")
                .join(format!("conversion-{}", std::process::id()));
            if let Err(error) = fs::create_dir_all(&temp_root) {
                window.set_audio_error(format!("Conversion: {error}").into());
                return;
            }
            let mut jobs = Vec::with_capacity(files.len());
            let mut rows = Vec::with_capacity(files.len());
            for (index, source) in files.into_iter().enumerate() {
                let destination = source.with_extension(format.as_str());
                let temporary = temp_root.join(format!("{index}.{format}"));
                jobs.push(ConversionJob {
                    source: source.clone(),
                    temporary,
                    destination,
                });
                rows.push(ConversionRow {
                    name: source.to_string_lossy().into_owned().into(),
                    path: source.to_string_lossy().into_owned().into(),
                    progress: 0.0,
                    status: "Waiting".into(),
                });
            }
            if let Some(engine) = playback.borrow_mut().as_mut() {
                engine.stop();
            }
            window.set_audio_playing(false);
            let model = Rc::new(VecModel::from(rows));
            window.set_conversion_rows(ModelRc::new(Rc::clone(&model)));
            *model_state.borrow_mut() = Some(model);
            *jobs_state.borrow_mut() = jobs.clone();
            *temp_state.borrow_mut() = Some(temp_root);
            let (sender, receiver) = mpsc::channel();
            let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            conversion::start(
                jobs,
                format.to_string(),
                quality.to_string(),
                sender,
                Arc::clone(&cancelled),
            );
            *receiver_state.borrow_mut() = Some(receiver);
            *cancelled_state.borrow_mut() = Some(cancelled);
            window.set_conversion_complete(false);
            window.set_conversion_progress_visible(true);
        });
    }

    {
        let cancelled_state = Rc::clone(&conversion_cancelled);
        window.on_conversion_cancelled(move || {
            if let Some(cancelled) = cancelled_state.borrow().as_ref() {
                cancelled.store(true, Ordering::Release);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let jobs_state = Rc::clone(&conversion_jobs);
        let temp_state = Rc::clone(&conversion_temp_root);
        let receiver_state = Rc::clone(&conversion_receiver);
        let cancelled_state = Rc::clone(&conversion_cancelled);
        let model_state = Rc::clone(&conversion_model);
        window.on_conversion_closed(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            if let Some(root) = temp_state.borrow_mut().take() {
                let _ = fs::remove_dir_all(root);
            }
            jobs_state.borrow_mut().clear();
            *receiver_state.borrow_mut() = None;
            *cancelled_state.borrow_mut() = None;
            *model_state.borrow_mut() = None;
            window.set_conversion_progress_visible(false);
            window.set_conversion_rows(ModelRc::new(VecModel::from(Vec::new())));
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        window.on_rename_requested(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = if !window.get_tree_menu_path().is_empty() {
                PathBuf::from(window.get_tree_menu_path().as_str())
            } else {
                let state_ref = tree_state.borrow();
                let Some(state) = state_ref.as_ref() else {
                    return;
                };
                let Some(path) = state.selected.as_ref() else {
                    return;
                };
                path.clone()
            };
            if !path.exists() || path.file_name().is_none() {
                return;
            }
            window.set_tree_edit_path(path.to_string_lossy().into_owned().into());
            window.set_tree_edit_text(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            );
            window.set_tree_edit_mode(1);
            window.set_tree_menu_path("".into());
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        window.on_new_folder_requested(move |parent| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let parent = PathBuf::from(parent.as_str());
            let parent = if parent.is_dir() {
                parent
            } else if parent.is_file() {
                let Some(parent) = parent.parent() else {
                    return;
                };
                parent.to_path_buf()
            } else {
                return;
            };
            {
                let mut state_ref = tree_state.borrow_mut();
                let Some(state) = state_ref.as_mut() else {
                    return;
                };
                state.select_and_expand(&parent);
            }
            window.set_tree_edit_path(parent.to_string_lossy().into_owned().into());
            window.set_tree_edit_text("".into());
            window.set_tree_edit_mode(2);
            window.set_tree_menu_path("".into());
            refresh_tree(&window, &tree_state);
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        let audio_folder = Rc::clone(&audio_folder);
        window.on_tree_edit_accepted(move |path, text, mode| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let name = text.trim();
            if name.is_empty()
                || name == "."
                || name == ".."
                || name
                    .chars()
                    .any(|character| character == '/' || character == '\\')
            {
                window.set_tree_edit_path("".into());
                window.set_tree_edit_text("".into());
                window.set_tree_edit_mode(0);
                return;
            }
            let source = PathBuf::from(path.as_str());
            let destination = if mode == 2 {
                source.join(name)
            } else {
                let Some(parent) = source.parent() else {
                    return;
                };
                parent.join(name)
            };
            let result = if mode == 2 {
                fs::create_dir(&destination)
            } else {
                fs::rename(&source, &destination)
            };
            if let Err(error) = result {
                window.set_audio_error(format!("File operation: {error}").into());
                return;
            }
            if mode != 2 {
                if let Some(folder) = audio_folder.borrow().as_ref() {
                    if let Err(error) = metadata::rename_associated_workflow(
                        folder,
                        &source,
                        &destination,
                    ) {
                        window.set_audio_error(format!("Workflow file operation: {error}").into());
                    }
                }
            }
            {
                let mut state_ref = tree_state.borrow_mut();
                let Some(state) = state_ref.as_mut() else {
                    return;
                };
                state.select_and_expand(&destination);
            }
            settings.borrow_mut().last_selected_path =
                Some(destination.to_string_lossy().into_owned());
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            window.set_selected_name(
                destination
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            );
            window.set_tree_edit_path("".into());
            window.set_tree_edit_text("".into());
            window.set_tree_edit_mode(0);
            window.set_audio_error("".into());
            refresh_tree(&window, &tree_state);
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        let audio_folder = Rc::clone(&audio_folder);
        window.on_tree_drop_requested(move |source, source_index, pointer_y| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let source = PathBuf::from(source.as_str());
            let (sources, target) = {
                let state_ref = tree_state.borrow();
                let Some(state) = state_ref.as_ref() else {
                    return;
                };
                let rows = file_system::build_visible_rows(
                    state,
                    file_system::SortOrder::from_i32(window.get_sort_order()),
                );
                let target_index = (source_index as f32 + ((pointer_y - 13.0) / 26.0).round())
                    .clamp(0.0, (rows.len() - 1) as f32)
                    as usize;
                let Some(row) = rows.get(target_index) else {
                    return;
                };
                let mut sources = state.selected_paths();
                if !sources.iter().any(|path| path == &source) {
                    sources = vec![source.clone()];
                }
                (sources, PathBuf::from(&row.path))
            };
            if sources.is_empty() || !target.is_dir() {
                return;
            }
            let mut destinations = Vec::with_capacity(sources.len());
            for source in &sources {
                let Some(name) = source.file_name() else {
                    return;
                };
                let Some(parent) = source.parent() else {
                    return;
                };
                if !source.exists() || source == &target || parent == target {
                    return;
                }
                if source.is_dir() && target.strip_prefix(source).is_ok() {
                    window
                        .set_audio_error("File operation: cannot move a folder into itself".into());
                    return;
                }
                let destination = target.join(name);
                if destination.exists() || destinations.contains(&destination) {
                    window.set_audio_error("File operation: destination already exists".into());
                    return;
                }
                destinations.push(destination);
            }
            for (source, destination) in sources.iter().zip(&destinations) {
                if let Err(error) = fs::rename(source, destination) {
                    window.set_audio_error(format!("File operation: {error}").into());
                    return;
                }
                if let Some(folder) = audio_folder.borrow().as_ref() {
                    if let Err(error) = metadata::rename_associated_workflow(
                        folder,
                        source,
                        destination,
                    ) {
                        window.set_audio_error(format!("Workflow file operation: {error}").into());
                    }
                }
            }
            {
                let mut state_ref = tree_state.borrow_mut();
                let Some(state) = state_ref.as_mut() else {
                    return;
                };
                state.select_paths(destinations.clone(), destinations.last().cloned().unwrap());
            }
            let primary = destinations.last().unwrap();
            settings.borrow_mut().last_selected_path = Some(primary.to_string_lossy().into_owned());
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            window.set_selected_name(
                primary
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            );
            window.set_audio_error("".into());
            refresh_tree(&window, &tree_state);
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_tree_edit_cancelled(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_tree_edit_path("".into());
                window.set_tree_edit_text("".into());
                window.set_tree_edit_mode(0);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_metadata_toggle(move || {
            if let Some(window) = weak_window.upgrade() {
                let visible = !window.get_metadata_visible();
                window.set_metadata_visible(visible);
                let mut settings = settings.borrow_mut();
                settings.metadata_visible = visible;
                settings::save(&settings);
            }
        });
        let weak_window = window.as_weak();
        let edited_workflow_for_edit = Rc::clone(&edited_workflow);
        let workflow_loading_for_edit = Rc::clone(&workflow_loading);
        let audio_folder_for_edit = Rc::clone(&audio_folder);
        window.on_workflow_metadata_changed(move |field, value| {
            if *workflow_loading_for_edit.borrow() {
                return;
            }
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            if window.get_workflow_loading() {
                return;
            }
            let Some(folder) = audio_folder_for_edit.borrow().clone() else {
                return;
            };
            if !ensure_edit_copy(&window, &folder, &edited_workflow_for_edit) {
                return;
            }
            window.set_workflow_modified(true);
            let _ = edited_workflow_for_edit
                .borrow_mut()
                .as_mut()
                .is_some_and(|workflow| {
                    metadata::comfyui::update_metadata(workflow, field.as_str(), value.as_str())
                });
        });
        let weak_window = window.as_weak();
        let edited_workflow_for_number = Rc::clone(&edited_workflow);
        let workflow_loading_for_number = Rc::clone(&workflow_loading);
        let audio_folder_for_number = Rc::clone(&audio_folder);
        window.on_workflow_number_changed(move |field, value| {
            if *workflow_loading_for_number.borrow() {
                return;
            }
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            if window.get_workflow_loading() {
                return;
            }
            let Some(folder) = audio_folder_for_number.borrow().clone() else {
                return;
            };
            if !ensure_edit_copy(&window, &folder, &edited_workflow_for_number) {
                return;
            }
            window.set_workflow_modified(true);
            let _ = edited_workflow_for_number
                .borrow_mut()
                .as_mut()
                .is_some_and(|workflow| {
                    metadata::comfyui::update_metadata(workflow, field.as_str(), &value.to_string())
                });
        });
        let weak_window = window.as_weak();
        window.on_lora_edit_requested(move |filename, tag| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            window.set_lora_editor_filename(filename);
            window.set_lora_editor_tag(tag);
            window.set_lora_editor_visible(true);
        });
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let workflow_loading = Rc::clone(&workflow_loading);
        window.on_lora_save(move |filename, tag| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let mut index = metadata::load_index(&folder);
            if let Some(lora) = index
                .loras
                .iter_mut()
                .find(|lora| lora.filename == filename.as_str())
            {
                lora.custom_tag = tag.to_string();
            } else {
                index.loras.push(metadata::LoraMetadata {
                    filename: filename.to_string(),
                    custom_tag: tag.to_string(),
                });
            }
            metadata::save_index(&folder, &index);
            window.set_lora_editor_visible(false);
            let audio_path = window.get_selected_audio_path().to_string();
            if !audio_path.is_empty() {
                load_workflow_for_audio(
                    &window,
                    &folder,
                    Path::new(&audio_path),
                    &workflow_loading,
                );
            }
        });
        let weak_window = window.as_weak();
        window.on_lora_cancel(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_lora_editor_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_comment_colors_selected(move |background, text| {
            let background = background.to_string();
            let text = text.to_string();
            let Some(background_color) = settings::parse_hex_color(&background) else {
                return;
            };
            let Some(text_color) = settings::parse_hex_color(&text) else {
                return;
            };
            {
                let mut settings = settings.borrow_mut();
                settings.comment_background_color = background.clone();
                settings.comment_text_color = text.clone();
            }
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_comment_background_hex(background.into());
                window.set_comment_text_hex(text.into());
                window.set_comment_background_color(background_color);
                window.set_comment_text_color(text_color);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        window.on_rating_requested(move |path, rating| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let path_string = path.to_string();
            let mut index = metadata::load_index(&folder);
            if let Some(file) = index
                .audio_files
                .iter_mut()
                .find(|item| item.file_path == path_string)
            {
                file.rating = rating.clamp(0, 5) as u8;
            } else {
                index.audio_files.push(metadata::AudioFileMetadata {
                    file_path: path_string.clone(),
                    rating: rating.clamp(0, 5) as u8,
                    ..Default::default()
                });
            }
            metadata::save_index(&folder, &index);
            if let Some(model) = audio_model.borrow().clone() {
                for index in 0..model.row_count() {
                    let Some(row) = model.row_data(index) else {
                        continue;
                    };
                    if row.path.as_str() == path_string {
                        model.set_row_data(
                            index,
                            AudioRow {
                                path: row.path,
                                name: row.name,
                                modified_date: row.modified_date,
                                peaks: row.peaks,
                                is_loading: row.is_loading,
                                comments: row.comments,
                                differences: row.differences,
                                rating: rating.clamp(0, 5),
                                is_pinned: row.is_pinned,
                                is_selected: row.is_selected,
                                is_active: row.is_active,
                                is_playing: row.is_playing,
                                progress: row.progress,
                                loop_enabled: row.loop_enabled,
                                selected_comment_start: row.selected_comment_start,
                                selected_comment_end: row.selected_comment_end,
                            },
                        );
                        break;
                    }
                }
            }
            window.set_audio_error("".into());
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let recreate_workflow_pending = Rc::clone(&recreate_workflow_pending);
        let edited_workflow = Rc::clone(&edited_workflow);
        window.on_recreate_workflow_requested(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                window.set_audio_error("Open a workspace before recreating a workflow".into());
                return;
            };
            let Some(config) = sync::load_config(&folder).filter(|config| !config.url.is_empty())
            else {
                *recreate_workflow_pending.borrow_mut() = true;
                let config = sync::load_config(&folder).unwrap_or_default();
                window.set_comfyui_sync_url(config.url.into());
                window.set_comfyui_sync_remote_directory(config.remote_directory.into());
                window.set_comfyui_sync_local_directory(config.local_directory.into());
                window.set_comfyui_sync_interval(config.interval_ms.to_string().into());
                window.set_comfyui_sync_error(
                    "Configure ComfyUI sync to recreate this workflow".into(),
                );
                window.set_comfyui_sync_test_message("".into());
                window.set_comfyui_sync_test_success(false);
                window.set_comfyui_sync_visible(true);
                return;
            };
            recreate_workflow(&window, &folder, &config, &edited_workflow);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let edited_workflow = Rc::clone(&edited_workflow);
        window.on_open_workflow_requested(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                window.set_audio_error("Open a workspace before opening a workflow".into());
                return;
            };
            let selected_audio_path = window.get_selected_audio_path();
            let audio_path = Path::new(selected_audio_path.as_str());
            let Some(workflow_path) = metadata::workflow_path(&folder, audio_path) else {
                window.set_audio_error("Audio file is outside the workspace".into());
                return;
            };
            let path_to_open = if window.get_workflow_modified() {
                if edited_workflow.borrow().is_none()
                    && !ensure_edit_copy(&window, &folder, &edited_workflow)
                {
                    window.set_audio_error("Select a workflow before opening it".into());
                    return;
                }
                let metadata = [
                    ("bpm", window.get_workflow_bpm().to_string()),
                    ("key", window.get_workflow_key().to_string()),
                    ("seed", window.get_workflow_seed().to_string()),
                    ("prompt", window.get_workflow_prompt().to_string()),
                    ("lyrics", window.get_workflow_lyrics().to_string()),
                ];
                let mut edited_workflow = edited_workflow.borrow_mut();
                let Some(workflow) = edited_workflow.as_mut() else {
                    window.set_audio_error("Select a workflow before opening it".into());
                    return;
                };
                for (field, value) in metadata {
                    metadata::comfyui::update_metadata(workflow, field, &value);
                }
                match write_modified_workflow(&workflow_path, workflow) {
                    Ok(path) => path,
                    Err(error) => {
                        window.set_audio_error(format!("Write modified workflow: {error}").into());
                        return;
                    }
                }
            } else {
                if !workflow_path.is_file() {
                    window.set_audio_error("Select a workflow before opening it".into());
                    return;
                }
                workflow_path
            };
            if let Err(error) = opener::open(&path_to_open) {
                window.set_audio_error(format!("Open workflow: {error}").into());
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let edited_workflow = Rc::clone(&edited_workflow);
        let cancelled_state = Rc::clone(&workflow_run_cancelled);
        let updates = workflow_run_sender.clone();
        window.on_run_workflow_requested(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(workspace) = audio_folder.borrow().clone() else {
                window.set_audio_error("Open a workspace before running a workflow".into());
                return;
            };
            let audio_path = PathBuf::from(window.get_selected_audio_path().as_str());
            let Some(output_directory) = audio_path.parent().map(Path::to_path_buf) else {
                window.set_audio_error("Select an audio file before running a workflow".into());
                return;
            };
            let Some(output_stem) = audio_path
                .file_stem()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
            else {
                window.set_audio_error("Selected audio file has no usable name".into());
                return;
            };
            let workflow = if let Some(workflow) = edited_workflow.borrow().clone() {
                workflow
            } else {
                let Some(workflow_path) = metadata::workflow_path(&workspace, &audio_path) else {
                    window.set_audio_error("Audio file is outside the workspace".into());
                    return;
                };
                let Ok(contents) = fs::read_to_string(workflow_path) else {
                    window.set_audio_error("Select a workflow before running it in ComfyUI".into());
                    return;
                };
                let Ok(workflow) = serde_json::from_str(&contents) else {
                    window.set_audio_error("Workflow JSON is invalid".into());
                    return;
                };
                workflow
            };
            let Some(config) =
                sync::load_config(&workspace).filter(|config| !config.url.is_empty())
            else {
                window.set_audio_error("Configure ComfyUI sync before running a workflow".into());
                return;
            };
            let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            *cancelled_state.borrow_mut() = Some(Arc::clone(&cancelled));
            window.set_comfyui_run_progress(0.0);
            window.set_comfyui_run_step("Submitting workflow to ComfyUI".into());
            window.set_comfyui_run_cancel_requested(false);
            window.set_comfyui_run_complete(false);
            window.set_comfyui_run_visible(true);
            let worker_updates = updates.clone();
            thread::spawn(move || {
                let result = ComfyUiClient::new().and_then(|client| {
                    client.run_workflow(
                        &config,
                        &workflow,
                        &workspace,
                        &output_directory,
                        &output_stem,
                        &cancelled,
                        &worker_updates,
                    )
                });
                if let Err(error) = result {
                    let _ = worker_updates.send(WorkflowRunUpdate::Error(error.to_string()));
                }
            });
        });
    }

    {
        let weak_window = window.as_weak();
        let cancelled_state = Rc::clone(&workflow_run_cancelled);
        window.on_run_workflow_cancelled(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_comfyui_run_cancel_requested(true);
                window.set_comfyui_run_step("Cancelling on ComfyUI...".into());
            }
            if let Some(cancelled) = cancelled_state.borrow().as_ref() {
                cancelled.store(true, std::sync::atomic::Ordering::Release);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let cancelled_state = Rc::clone(&workflow_run_cancelled);
        window.on_run_workflow_closed(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_comfyui_run_visible(false);
            }
            *cancelled_state.borrow_mut() = None;
        });
    }

    {
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        window.on_comment_range_moved(move |path, old_start, old_end, start, end, text| {
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let path_string = path.to_string();
            let audio_path = Path::new(path.as_str());
            let mut file = metadata::load_audio_metadata(&folder, audio_path);
            if file.file_path.is_empty() {
                return;
            }
            if file.file_path != path_string {
                return;
            }
            let comments = {
                let duration = file.duration_seconds;
                if duration <= 0.0 {
                    return;
                }
                if let Some(comment) = file.comments.iter_mut().find(|comment| {
                    (comment.start_seconds / duration - old_start).abs() < 0.001
                        && (comment.end_seconds / duration - old_end).abs() < 0.001
                        && comment.text == text.as_str()
                }) {
                    comment.start_seconds = (start * duration).clamp(0.0, duration);
                    comment.end_seconds = (end * duration).clamp(comment.start_seconds, duration);
                } else {
                    return;
                }
                comment_rows(&file)
            };
            metadata::save_audio_metadata(&folder, audio_path, &file);
            update_comment_model(&audio_model, audio_path, comments);
            select_comment(&audio_model, audio_path, start, end);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let playback = Rc::clone(&playback);
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        let weak_window_for_move = weak_window.clone();
        let audio_folder_for_move = Rc::clone(&audio_folder);
        let audio_model_for_move = Rc::clone(&audio_model);
        let audio_load_state_for_move = Arc::clone(&audio_load_state);
        let playback_for_move = Rc::clone(&playback);
        let tree_state_for_move = Rc::clone(&tree_state);
        let settings_for_move = Rc::clone(&settings);
        let settings_for_request = Rc::clone(&settings);
        let move_to_trash: Rc<dyn Fn(SharedString)> = Rc::new(move |path| {
            let Some(window) = weak_window_for_move.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder_for_move.borrow().clone() else {
                return;
            };
            let source = PathBuf::from(path.as_str());
            let Some(name) = source.file_name() else {
                return;
            };
            let trash_folder = folder.join(".adbstudio").join("trash");
            let destination = trash_folder.join(name);
            if destination.exists() {
                window.set_audio_error("File operation: trash destination already exists".into());
                return;
            }
            if let Err(error) =
                fs::create_dir_all(&trash_folder).and_then(|_| fs::rename(&source, &destination))
            {
                window.set_audio_error(format!("File operation: {error}").into());
                return;
            }
            if playback_for_move
                .borrow()
                .as_ref()
                .and_then(|engine| engine.path())
                == Some(source.as_path())
            {
                if let Some(engine) = playback_for_move.borrow_mut().as_mut() {
                    engine.stop();
                }
                window.set_active_audio_path("".into());
                window.set_audio_file_name("".into());
                window.set_audio_playing(false);
            }
            let parent = source.parent().unwrap_or(&folder).to_path_buf();
            select_tree_path(&window, &tree_state_for_move, &settings_for_move, &parent);
            refresh_audio(
                &window,
                &audio_folder_for_move,
                &audio_model_for_move,
                &audio_load_state_for_move,
                parent,
            );
            window.set_audio_error("".into());
        });
        let move_to_trash_for_request = Rc::clone(&move_to_trash);
        window.on_trash_requested(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let workspace_key = folder.to_string_lossy().into_owned();
            if !settings_for_request
                .borrow()
                .trash_confirmation_disabled_workspaces
                .contains(&workspace_key)
            {
                window.set_trash_confirm_path(path);
                window.set_trash_confirm_dont_ask(false);
                window.set_trash_confirm_visible(true);
                return;
            }
            move_to_trash_for_request(path);
        });

        let move_to_trash_for_confirmation = Rc::clone(&move_to_trash);
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_trash_confirmed(move |dont_ask| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = window.get_trash_confirm_path();
            if dont_ask {
                if let Some(folder) = settings.borrow().last_folder.clone() {
                    settings
                        .borrow_mut()
                        .trash_confirmation_disabled_workspaces
                        .insert(folder);
                    settings::save(&settings.borrow());
                }
            }
            window.set_trash_confirm_visible(false);
            if !path.is_empty() {
                move_to_trash_for_confirmation(path);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_trash_cancelled(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_trash_confirm_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        let playback = Rc::clone(&playback);
        window.on_comment_selected(move |path, start, end, _text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(workspace) = audio_folder.borrow().clone() else {
                return;
            };
            let duration = comment_duration(&workspace, &path, &playback);
            if duration <= 0.0 {
                window.set_audio_error("Unable to determine audio duration".into());
                return;
            }
            select_comment(&audio_model, &path, start, end);
            let mut playback_ref = playback.borrow_mut();
            let Some(engine) = playback_ref.as_mut() else {
                return;
            };
            let position =
                Duration::from_secs_f32((start.clamp(0.0, 1.0) * duration).min(duration));
            let result = if engine.path() == Some(path.as_path()) && engine.can_resume() {
                let result = engine.seek(position);
                if result.is_ok() {
                    engine.resume();
                }
                result
            } else {
                engine.play(&path, position)
            };
            if let Err(error) = result {
                window.set_audio_error(error.into());
                return;
            }
            engine.set_comment_loop(
                Duration::from_secs_f32(start.clamp(0.0, 1.0) * duration),
                Duration::from_secs_f32(end.clamp(0.0, 1.0) * duration),
            );
            window.set_audio_error("".into());
            window.set_active_audio_path(path.to_string_lossy().into_owned().into());
            window.set_audio_file_name(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .into(),
            );
            window.set_audio_playing(engine.is_playing());
            update_audio_rows(
                &audio_model,
                engine.path(),
                engine.is_playing(),
                engine.position(),
                engine.duration(),
            );
            save_playback_position(&workspace, engine);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        let playback = Rc::clone(&playback);
        let comment_editor_original = Rc::clone(&comment_editor_original);
        let comment_editor_duration = Rc::clone(&comment_editor_duration);
        window.on_comment_edit_requested(move |path, start, end, text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let duration = comment_duration(&folder, &path, &playback);
            if duration <= 0.0 {
                return;
            }
            let original = AudioComment {
                start_seconds: start * duration,
                end_seconds: end * duration,
                text: text.to_string(),
            };
            *comment_editor_duration.borrow_mut() = duration;
            select_comment(&audio_model, &path, start, end);
            *comment_editor_original.borrow_mut() = Some(original.clone());
            window.set_comment_editor_path(path.to_string_lossy().into_owned().into());
            window.set_comment_editor_start(format_seconds(original.start_seconds).into());
            window.set_comment_editor_end(format_seconds(original.end_seconds).into());
            window.set_comment_editor_text(original.text.into());
            window.set_comment_editor_visible(true);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let comment_editor_original = Rc::clone(&comment_editor_original);
        let comment_editor_duration = Rc::clone(&comment_editor_duration);
        window.on_comment_save(move |path, start, end, text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let (Ok(start_seconds), Ok(end_seconds)) = (start.parse::<f32>(), end.parse::<f32>())
            else {
                window.set_audio_error("Comment times must be numbers".into());
                return;
            };
            let stored_metadata = metadata::load_audio_metadata(&folder, &path);
            let duration = (stored_metadata.duration_seconds > 0.0)
                .then_some(stored_metadata.duration_seconds)
                .unwrap_or(*comment_editor_duration.borrow());
            if duration <= 0.0 {
                window.set_audio_error("Unable to determine audio duration".into());
                return;
            }
            let comment = AudioComment {
                start_seconds,
                end_seconds,
                text: text.to_string(),
            }
            .normalized(duration);
            let selected_start = comment.start_seconds / duration;
            let selected_end = comment.end_seconds / duration;
            let mut file = stored_metadata;
            file.file_path = path.to_string_lossy().into_owned();
            if let Some(original) = comment_editor_original.borrow_mut().take() {
                file.comments.retain(|item| item != &original);
            }
            file.comments.push(comment);
            metadata::save_audio_metadata(&folder, &path, &file);
            let view_folder = path.parent().unwrap_or(&folder).to_path_buf();
            refresh_audio(
                &window,
                &audio_folder,
                &audio_model,
                &audio_load_state,
                view_folder,
            );
            select_comment(&audio_model, &path, selected_start, selected_end);
            window.set_comment_editor_visible(false);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let comment_editor_original = Rc::clone(&comment_editor_original);
        window.on_comment_delete(move |path, _start, _end, _text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(workspace) = audio_folder.borrow().clone() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let mut file = metadata::load_audio_metadata(&workspace, &path);
            if file.file_path.is_empty() {
                return;
            }
            if let Some(original) = comment_editor_original.borrow_mut().take() {
                file.comments.retain(|item| item != &original);
                metadata::save_audio_metadata(&workspace, &path, &file);
                let view_folder = path.parent().unwrap_or(&workspace).to_path_buf();
                refresh_audio(
                    &window,
                    &audio_folder,
                    &audio_model,
                    &audio_load_state,
                    view_folder,
                );
            }
            window.set_comment_editor_visible(false);
        });
    }

    {
        let weak_window = window.as_weak();
        let comment_editor_original = Rc::clone(&comment_editor_original);
        window.on_comment_cancel(move || {
            *comment_editor_original.borrow_mut() = None;
            if let Some(window) = weak_window.upgrade() {
                window.set_comment_editor_visible(false);
            }
        });
    }

    {
        let settings = Rc::clone(&settings);
        window.on_left_pane_width_changed(move |width| {
            settings.borrow_mut().left_pane_width = width;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
        });
    }

    {
        let settings = Rc::clone(&settings);
        window.on_metadata_pane_height_changed(move |height| {
            settings.borrow_mut().metadata_pane_height = height;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
        });
    }

    let last_folder = settings.borrow().last_folder.clone();
    if let Some(last_folder) = last_folder {
        let folder = PathBuf::from(last_folder);
        if folder.is_dir() {
            set_workspace(
                &window,
                folder,
                &settings,
                &tree_state,
                &audio_folder,
                &audio_model,
                &audio_load_state,
                &workflow_files,
                &sync_controller,
                &workspace_watcher,
                &workspace_change_sender,
            );
        }
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let playback = Rc::clone(&playback);
        let audio_folder = Rc::clone(&audio_folder);
        let settings = Rc::clone(&settings);
        let last_persisted_position = Rc::new(RefCell::new(Instant::now()));
        let last_persisted_position_for_timer = Rc::clone(&last_persisted_position);
        let audio_result_receiver = Rc::new(RefCell::new(audio_result_receiver));
        let sync_controller = Rc::clone(&sync_controller);
        let workflow_loading = Rc::clone(&workflow_loading);
        let workspace_change_receiver = Rc::new(RefCell::new(workspace_change_receiver));
        let tree_state = Rc::clone(&tree_state);
        let workflow_files = Rc::clone(&workflow_files);
        let conversion_receiver = Rc::clone(&conversion_receiver);
        let conversion_jobs = Rc::clone(&conversion_jobs);
        let conversion_model = Rc::clone(&conversion_model);
        let conversion_temp_root = Rc::clone(&conversion_temp_root);
        let conversion_cancelled = Rc::clone(&conversion_cancelled);
        let workflow_run_receiver = Rc::clone(&workflow_run_receiver);
        let workflow_run_cancelled = Rc::clone(&workflow_run_cancelled);
        let tree_state_for_conversion = Rc::clone(&tree_state);
        let mut conversion_updates = 0usize;
        let mut spinner_frame = 0usize;
        let mut sync_spinner_frame = 0usize;
        let mut workspace_change_pending = false;
        let mut last_workspace_refresh = Instant::now();
        let mut workspace_change_paths = Vec::new();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(40),
            move || {
                if let Some(window) = weak_window.upgrade() {
                    if let Some(receiver) = workflow_run_receiver.borrow_mut().as_mut() {
                        while let Ok(update) = receiver.try_recv() {
                            match update {
                                WorkflowRunUpdate::Progress { progress, step } => {
                                    window.set_comfyui_run_progress(progress);
                                    window.set_comfyui_run_step(step.into());
                                }
                                WorkflowRunUpdate::Finished {
                                    audio_path,
                                    workflow_path,
                                } => {
                                    window.set_comfyui_run_progress(1.0);
                                    window.set_comfyui_run_step(
                                        format!(
                                            "Saved {} and {}",
                                            audio_path.display(),
                                            workflow_path.display()
                                        )
                                        .into(),
                                    );
                                    window.set_comfyui_run_complete(true);
                                    *workflow_run_cancelled.borrow_mut() = None;
                                    refresh_tree(&window, &tree_state);
                                }
                                WorkflowRunUpdate::Cancelled => {
                                    window.set_comfyui_run_step("Cancelled on ComfyUI".into());
                                    window.set_comfyui_run_complete(true);
                                    *workflow_run_cancelled.borrow_mut() = None;
                                }
                                WorkflowRunUpdate::Error(message) => {
                                    window.set_comfyui_run_step(format!("Error: {message}").into());
                                    window.set_comfyui_run_complete(true);
                                    window.set_audio_error(format!("ComfyUI: {message}").into());
                                    *workflow_run_cancelled.borrow_mut() = None;
                                }
                            }
                        }
                    }
                    let mut conversion_finished = false;
                    if let Some(receiver) = conversion_receiver.borrow_mut().as_mut() {
                        while let Ok(update) = receiver.try_recv() {
                            conversion_updates += 1;
                            if let Some(model) = conversion_model.borrow().as_ref() {
                                if let Some(mut row) = model.row_data(update.index) {
                                    row.progress = update.progress;
                                    row.status = update.status.into();
                                    model.set_row_data(update.index, row);
                                }
                            }
                        }
                        conversion_finished = conversion_updates >= conversion_jobs.borrow().len()
                            && !conversion_jobs.borrow().is_empty();
                    }
                    if conversion_finished {
                        let jobs = conversion_jobs.borrow().clone();
                        let mut errors = Vec::new();
                        for (index, job) in jobs.iter().enumerate() {
                            let complete = conversion_model
                                .borrow()
                                .as_ref()
                                .and_then(|model| model.row_data(index))
                                .is_some_and(|row| row.status == "Complete");
                            if !complete {
                                continue;
                            }
                            if let Err(error) = trash::delete(&job.source) {
                                errors.push(error.to_string());
                            } else if let Err(error) = fs::rename(&job.temporary, &job.destination) {
                                errors.push(error.to_string());
                            } else if let Some(folder) = audio_folder.borrow().as_ref() {
                                if let Err(error) = metadata::rename_associated_workflow(
                                    folder,
                                    &job.source,
                                    &job.destination,
                                ) {
                                    errors.push(error.to_string());
                                }
                            }
                        }
                        if let Some(root) = conversion_temp_root.borrow_mut().take() {
                            let _ = fs::remove_dir_all(root);
                        }
                        *conversion_receiver.borrow_mut() = None;
                        *conversion_cancelled.borrow_mut() = None;
                        conversion_updates = 0;
                        window.set_conversion_complete(true);
                        refresh_tree(&window, &tree_state_for_conversion);
                        let audio_view_folder = {
                            let state_ref = tree_state_for_conversion.borrow();
                            state_ref
                                .as_ref()
                                .and_then(|state| state.selected.as_ref())
                                .and_then(|path| {
                                    if path.is_dir() {
                                        Some(path.clone())
                                    } else {
                                        path.parent().map(Path::to_path_buf)
                                    }
                                })
                                .or_else(|| audio_folder.borrow().clone())
                        };
                        if let Some(audio_view_folder) = audio_view_folder {
                            refresh_audio(
                                &window,
                                &audio_folder,
                                &audio_model,
                                &audio_load_state,
                                audio_view_folder,
                            );
                        }
                        if !errors.is_empty() {
                            window.set_audio_error(
                                format!("Conversion: {}", errors.join("; ")).into(),
                            );
                        }
                    }
                    let mut workspace_changed = false;
                    while let Ok(paths) = workspace_change_receiver.borrow_mut().try_recv() {
                        workspace_changed = true;
                        workspace_change_paths.extend(paths);
                    }
                    if workspace_changed {
                        workspace_change_pending = true;
                        last_workspace_refresh = Instant::now();
                    }
                    if workspace_change_pending
                        && last_workspace_refresh.elapsed() >= Duration::from_millis(150)
                    {
                        workspace_change_pending = false;
                        last_workspace_refresh = Instant::now();
                        refresh_workspace(
                            &window,
                            &audio_folder,
                            &audio_model,
                            &audio_load_state,
                            &tree_state,
                            &workflow_files,
                            &workspace_change_paths,
                        );
                        workspace_change_paths.clear();
                    }
                    let mut folder_next_path = None;
                    if let Some(engine) = playback.borrow_mut().as_mut() {
                        engine.update_position();
                        let comment_loop = engine.comment_loop();
                        let should_loop = comment_loop
                            .map(|(_, end)| engine.position() >= end)
                            .unwrap_or_else(|| {
                                engine.has_finished()
                                    || (!engine.is_playing()
                                        && engine.position() >= engine.duration())
                            });
                        let loop_mode = settings.borrow().loop_mode;
                        if (loop_mode == 1 || (loop_mode == 2 && comment_loop.is_some()))
                            && !engine.duration().is_zero()
                            && should_loop
                        {
                            if let Some(path) = engine.path().map(Path::to_path_buf) {
                                let loop_start = comment_loop
                                    .map(|(start, _)| start)
                                    .unwrap_or(Duration::ZERO);
                                let _ = engine.play(&path, loop_start);
                            }
                        } else if loop_mode == 2
                            && !engine.duration().is_zero()
                            && should_loop
                        {
                            if let Some(path) = engine.path() {
                                if let Some(model) = audio_model.borrow().clone() {
                                    let row_count = model.row_count();
                                    let current_index = (0..row_count).find(|index| {
                                        model.row_data(*index).is_some_and(|row| {
                                            row.path.as_str() == path.to_string_lossy().as_ref()
                                        })
                                    });
                                    if let Some(current_index) = current_index {
                                        let next_index = (current_index + 1) % row_count;
                                        folder_next_path = model
                                            .row_data(next_index)
                                            .map(|row| row.path);
                                    }
                                }
                            }
                        }
                        let active_path = engine
                            .path()
                            .map(|path| path.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let position = engine.position();
                        let duration = engine.duration();
                        let playing = engine.is_playing();
                        window.set_active_audio_path(active_path.into());
                        window.set_audio_file_name(
                            engine
                                .path()
                                .and_then(|path| path.file_name())
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default()
                                .into(),
                        );
                        window.set_audio_playing(playing);
                        window.set_audio_current_time(format_duration(position).into());
                        window.set_audio_total_duration(format_duration(duration).into());
                        update_audio_rows(&audio_model, engine.path(), playing, position, duration);
                        if last_persisted_position_for_timer.borrow().elapsed()
                            >= Duration::from_millis(500)
                        {
                            if let Some(folder) = audio_folder.borrow().clone() {
                                save_playback_position(&folder, engine);
                            }
                            *last_persisted_position_for_timer.borrow_mut() = Instant::now();
                        }
                    }
                    if let Some(path) = folder_next_path {
                        window.invoke_audio_play(path);
                    }
                    let state = audio_load_state.lock().unwrap();
                    window.set_audio_loading(state.running);
                    window.set_audio_completed(state.completed as i32);
                    window.set_audio_total(state.total as i32);
                    if state.running {
                        window.set_audio_spinner(["|", "/", "-", "\\"][spinner_frame].into());
                        spinner_frame = (spinner_frame + 1) % 4;
                    }
                    let current_generation = sync_controller.borrow().generation();
                    for event in sync_controller.borrow().events().try_iter() {
                        match event {
                            SyncEvent::Running { generation }
                                if generation == current_generation =>
                            {
                                window.set_comfyui_sync_active(true);
                                window.set_comfyui_sync_error_state(false);
                                window.set_comfyui_sync_status("Syncing".into());
                            }
                            SyncEvent::Progress {
                                generation,
                                progress,
                            } if generation == current_generation => {
                                window.set_comfyui_sync_active(true);
                                window.set_comfyui_sync_error_state(false);
                                window.set_comfyui_sync_present(progress.present as i32);
                                window.set_comfyui_sync_total(progress.total as i32);
                                window.set_comfyui_sync_status("Syncing".into());
                            }
                            SyncEvent::Downloaded {
                                generation,
                                audio_path,
                            } if generation == current_generation
                                && settings.borrow().auto_play_new_tracks =>
                            {
                                let path = PathBuf::from(&audio_path);
                                let Some(folder) = audio_folder.borrow().clone() else {
                                    continue;
                                };
                                let Some(parent) = path.parent().map(Path::to_path_buf) else {
                                    continue;
                                };
                                select_tree_path(&window, &tree_state, &settings, &path);
                                refresh_audio(
                                    &window,
                                    &audio_folder,
                                    &audio_model,
                                    &audio_load_state,
                                    parent,
                                );
                                window.set_selected_audio_path(audio_path.clone().into());
                                select_audio_path(&audio_model, &path);
                                let mut playback_ref = playback.borrow_mut();
                                let Some(engine) = playback_ref.as_mut() else {
                                    continue;
                                };
                                engine.clear_comment_loop();
                                if let Err(error) = engine.play(&path, Duration::ZERO) {
                                    window.set_audio_error(error.into());
                                    continue;
                                }
                                window.set_audio_error("".into());
                                window.set_active_audio_path(audio_path.clone().into());
                                window.set_audio_file_name(
                                    path.file_name()
                                        .and_then(|name| name.to_str())
                                        .unwrap_or_default()
                                        .into(),
                                );
                                window.set_audio_playing(engine.is_playing());
                                update_audio_rows(
                                    &audio_model,
                                    engine.path(),
                                    engine.is_playing(),
                                    engine.position(),
                                    engine.duration(),
                                );
                                scroll_audio_to_path(&window, &audio_model, &path);
                                load_workflow_for_audio(&window, &folder, &path, &workflow_loading);
                                save_playback_position(&folder, engine);
                            }
                            SyncEvent::WorkflowUpdated {
                                generation,
                                audio_path,
                            } if generation == current_generation
                                && window.get_active_audio_path() == audio_path.as_str() =>
                            {
                                if let Some(folder) = audio_folder.borrow().clone() {
                                    load_workflow_for_audio(
                                        &window,
                                        &folder,
                                        Path::new(audio_path.as_str()),
                                        &workflow_loading,
                                    );
                                }
                            }
                            SyncEvent::Error {
                                generation,
                                message,
                            } if generation == current_generation => {
                                window.set_comfyui_sync_active(false);
                                window.set_comfyui_sync_error_state(true);
                                window.set_comfyui_sync_status("Sync stopped".into());
                                window.set_comfyui_sync_error(message.into());
                            }
                            _ => {}
                        }
                    }
                    if window.get_comfyui_sync_active() {
                        window.set_comfyui_sync_spinner(
                            ["|", "/", "-", "\\"][sync_spinner_frame].into(),
                        );
                        sync_spinner_frame = (sync_spinner_frame + 1) % 4;
                    }
                }
                let Some(model) = audio_model.borrow().clone() else {
                    return;
                };
                let loading = audio_load_state.lock().unwrap().loading.clone();
                update_audio_loading_rows(&audio_model, &loading);
                for result in audio_result_receiver.borrow_mut().try_iter().take(3) {
                    let current_generation = audio_load_state.lock().unwrap().generation;
                    if result.generation != current_generation || result.index >= model.row_count()
                    {
                        continue;
                    }
                    let Some(row) = model.row_data(result.index) else {
                        continue;
                    };
                    model.set_row_data(
                        result.index,
                        AudioRow {
                            path: result.path.into(),
                            name: row.name,
                            modified_date: row.modified_date,
                            peaks: ModelRc::new(VecModel::from(waveform::aggregate_peaks(
                                &result.peaks,
                            ))),
                            is_loading: false,
                            comments: row.comments,
                            differences: row.differences,
                            rating: row.rating,
                            is_pinned: row.is_pinned,
                            is_selected: row.is_selected,
                            is_active: row.is_active,
                            is_playing: row.is_playing,
                            progress: row.progress,
                            loop_enabled: row.loop_enabled,
                            selected_comment_start: row.selected_comment_start,
                            selected_comment_end: row.selected_comment_end,
                        },
                    );
                }
            },
        );
        std::mem::forget(timer);
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        let tree_state = Rc::clone(&tree_state);
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let sync_controller = Rc::clone(&sync_controller);
        let workflow_files = Rc::clone(&workflow_files);
        let workspace_watcher = Rc::clone(&workspace_watcher);
        let workspace_change_sender = workspace_change_sender.clone();
        window.on_open_folder(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };

            if let Some(folder) = rfd::FileDialog::new()
                .set_title("Open Adb Studio Workspace")
                .pick_folder()
            {
                set_workspace(
                    &window,
                    folder,
                    &settings,
                    &tree_state,
                    &audio_folder,
                    &audio_model,
                    &audio_load_state,
                    &workflow_files,
                    &sync_controller,
                    &workspace_watcher,
                    &workspace_change_sender,
                );
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        let tree_state = Rc::clone(&tree_state);
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let workflow_files = Rc::clone(&workflow_files);
        let sync_controller = Rc::clone(&sync_controller);
        let playback = Rc::clone(&playback);
        let workspace_watcher = Rc::clone(&workspace_watcher);
        window.on_close_folder(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            close_workspace(
                &window,
                &settings,
                &tree_state,
                &audio_folder,
                &audio_model,
                &audio_load_state,
                &workflow_files,
                &sync_controller,
                &playback,
                &workspace_watcher,
            );
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_loop_changed(move |mode| {
            settings.borrow_mut().loop_mode = mode.clamp(0, 2);
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_loop_mode(mode.clamp(0, 2));
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_auto_play_new_tracks_changed(move |enabled| {
            settings.borrow_mut().auto_play_new_tracks = enabled;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_auto_play_new_tracks(enabled);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_seek_seconds_changed(move |seconds| {
            let seconds = seconds.max(1);
            settings.borrow_mut().seek_seconds = seconds as f32;
            settings::save(&settings.borrow());
            if let Some(window) = weak_window.upgrade() {
                window.set_seek_seconds(seconds);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_shortcut_changed(move |action, key| {
            let mut settings = settings.borrow_mut();
            match action {
                0 => settings.shortcut_fullscreen = key,
                1 => settings.shortcut_metadata = key,
                2 => settings.shortcut_play_pause = key,
                3 => settings.shortcut_navigate_up = key,
                4 => settings.shortcut_navigate_down = key,
                5 => settings.shortcut_cancel_edit = key,
                6 => settings.shortcut_seek_backward = key,
                7 => settings.shortcut_seek_forward = key,
                8 => settings.shortcut_trash = key,
                _ => return,
            }
            settings::save(&settings);
            drop(settings);
            if let Some(window) = weak_window.upgrade() {
                match action {
                    0 => window.set_shortcut_fullscreen(key),
                    1 => window.set_shortcut_metadata(key),
                    2 => window.set_shortcut_play_pause(key),
                    3 => window.set_shortcut_navigate_up(key),
                    4 => window.set_shortcut_navigate_down(key),
                    5 => window.set_shortcut_cancel_edit(key),
                    6 => window.set_shortcut_seek_backward(key),
                    7 => window.set_shortcut_seek_forward(key),
                    8 => window.set_shortcut_trash(key),
                    _ => {}
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        let playback = Rc::clone(&playback);
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        window.on_global_key_pressed(move |key, shift| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let (
                shortcut_fullscreen,
                shortcut_metadata,
                shortcut_play_pause,
                shortcut_navigate_up,
                shortcut_navigate_down,
                shortcut_cancel_edit,
                shortcut_seek_backward,
                shortcut_seek_forward,
                shortcut_trash,
                seek_seconds,
            ) = {
                let settings = settings.borrow();
                (
                    settings.shortcut_fullscreen,
                    settings.shortcut_metadata,
                    settings.shortcut_play_pause,
                    settings.shortcut_navigate_up,
                    settings.shortcut_navigate_down,
                    settings.shortcut_cancel_edit,
                    settings.shortcut_seek_backward,
                    settings.shortcut_seek_forward,
                    settings.shortcut_trash,
                    settings.seek_seconds,
                )
            };
            let matches = |configured: i32| configured == key;
            if matches(shortcut_fullscreen) {
                window.invoke_toggle_fullscreen();
                return;
            }
            if matches(shortcut_metadata) {
                window.invoke_metadata_toggle();
                return;
            }
            if matches(shortcut_play_pause) {
                if !window.get_selected_audio_path().is_empty() {
                    window.invoke_audio_play(window.get_selected_audio_path());
                } else {
                    window.invoke_audio_play_pause();
                }
                return;
            }
            if matches(shortcut_navigate_up) {
                window.invoke_audio_navigate(-1);
                return;
            }
            if matches(shortcut_navigate_down) {
                window.invoke_audio_navigate(1);
                return;
            }
            if matches(shortcut_cancel_edit) {
                if !window.get_tree_edit_path().is_empty() {
                    window.invoke_tree_edit_cancelled();
                }
                return;
            }
            if matches(shortcut_trash) && !shift {
                if !window.get_active_audio_path().is_empty() {
                    window.invoke_trash_requested(window.get_active_audio_path());
                }
                return;
            }
            let seek_key = if matches(shortcut_seek_backward) {
                -1.0
            } else if matches(shortcut_seek_forward) {
                1.0
            } else {
                return;
            };
            let path = PathBuf::from(window.get_active_audio_path().as_str());
            let mut playback_ref = playback.borrow_mut();
            let Some(engine) = playback_ref.as_mut() else {
                return;
            };
            if engine.path() != Some(path.as_path()) {
                return;
            }
            let seek_end = engine.duration().saturating_sub(Duration::from_micros(100));
            let target = if shift {
                if seek_key < 0.0 {
                    Duration::ZERO
                } else {
                    seek_end
                }
            } else {
                let delta = Duration::from_secs_f32(seek_seconds.max(1.0));
                if seek_key < 0.0 {
                    engine.position().saturating_sub(delta)
                } else {
                    engine.position().saturating_add(delta).min(seek_end)
                }
            };
            if engine.seek(target).is_ok() {
                update_audio_rows(
                    &audio_model,
                    engine.path(),
                    engine.is_playing(),
                    engine.position(),
                    engine.duration(),
                );
                if let Some(folder) = audio_folder.borrow().clone() {
                    save_playback_position(&folder, engine);
                }
                window.set_audio_current_time(format_duration(engine.position()).into());
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let playback = Rc::clone(&playback);
        let workflow_loading = Rc::clone(&workflow_loading);
        window.on_row_clicked(move |path, shift| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());

            let mut state_ref = tree_state.borrow_mut();
            let Some(state) = state_ref.as_mut() else {
                return;
            };
            let was_selected = state.selected.as_ref() == Some(&path);
            state.select_with_shift(
                &path,
                shift,
                file_system::SortOrder::from_i32(window.get_sort_order()),
            );
            if path.is_dir() && was_selected {
                state.toggle(&path);
            }
            if !path.is_dir() {
                state.expand_to(&path);
            }
            let selected_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            settings.borrow_mut().last_selected_path = Some(path.to_string_lossy().into_owned());
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            drop(state_ref);

            window.set_selected_name(selected_name.into());
            refresh_tree(&window, &tree_state);
            if path.is_dir() {
                refresh_audio(
                    &window,
                    &audio_folder,
                    &audio_model,
                    &audio_load_state,
                    path,
                );
            } else if file_system::FileKind::from_path(&path) == file_system::FileKind::Audio {
                let Some(folder) = path.parent().map(Path::to_path_buf) else {
                    return;
                };
                window.set_selected_audio_path(path.to_string_lossy().into_owned().into());
                refresh_audio(
                    &window,
                    &audio_folder,
                    &audio_model,
                    &audio_load_state,
                    folder,
                );
                let mut playback_ref = playback.borrow_mut();
                let Some(engine) = playback_ref.as_mut() else {
                    return;
                };
                if let Err(error) = engine.play(&path, Duration::ZERO) {
                    window.set_audio_error(error.into());
                    return;
                }
                window.set_audio_error("".into());
                window.set_active_audio_path(path.to_string_lossy().into_owned().into());
                window.set_audio_file_name(
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .into(),
                );
                window.set_audio_playing(engine.is_playing());
                update_audio_rows(
                    &audio_model,
                    engine.path(),
                    engine.is_playing(),
                    engine.position(),
                    engine.duration(),
                );
                scroll_audio_to_path(&window, &audio_model, &path);
                if let Some(folder) = audio_folder.borrow().clone() {
                    load_workflow_for_audio(&window, &folder, &path, &workflow_loading);
                    save_playback_position(&folder, engine);
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        window.on_filter_changed(move |filter| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            window.set_audio_filter(filter);
            let folder = audio_folder.borrow().clone();
            if let Some(folder) = folder {
                refresh_audio(
                    &window,
                    &audio_folder,
                    &audio_model,
                    &audio_load_state,
                    folder,
                );
            }
        });
    }

    {
        let audio_load_state = Arc::clone(&audio_load_state);
        window.on_audio_viewport_changed(move |start_index, visible_rows| {
            request_audio_generation(
                &audio_load_state,
                start_index as usize,
                visible_rows.max(1) as usize,
            );
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(&audio_model);
        window.on_audio_navigate(move |direction| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(model) = audio_model.borrow().clone() else {
                return;
            };
            let row_count = model.row_count();
            if row_count == 0 {
                return;
            }
            let selected_index = (0..row_count)
                .find(|index| model.row_data(*index).is_some_and(|row| row.is_selected));
            let next_index = selected_index
                .map(|index| (index as i32 + direction).clamp(0, row_count as i32 - 1) as usize)
                .unwrap_or(if direction < 0 { 0 } else { row_count - 1 });
            if selected_index == Some(next_index) {
                return;
            }
            let Some(row) = model.row_data(next_index) else {
                return;
            };
            let path = PathBuf::from(row.path.as_str());
            window.set_selected_audio_path(row.path.clone());
            select_audio_path(&audio_model, &path);
            window.invoke_audio_play(row.path);
        });
    }

    {
        let weak_window = window.as_weak();
        let playback = Rc::clone(&playback);
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        window.on_audio_play_pause(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let mut playback_ref = playback.borrow_mut();
            let Some(engine) = playback_ref.as_mut() else {
                return;
            };
            if engine.is_playing() {
                engine.pause();
            } else if engine.path().is_some() {
                engine.resume();
            } else {
                return;
            }
            update_audio_rows(
                &audio_model,
                engine.path(),
                engine.is_playing(),
                engine.position(),
                engine.duration(),
            );
            window.set_audio_playing(engine.is_playing());
            if let Some(folder) = audio_folder.borrow().clone() {
                save_playback_position(&folder, engine);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let playback = Rc::clone(&playback);
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        let last_button_click = Rc::clone(&last_button_click);
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        let workflow_loading = Rc::clone(&workflow_loading);
        window.on_audio_play(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let now = Instant::now();
            let restart =
                last_button_click
                    .borrow()
                    .as_ref()
                    .is_some_and(|(last_path, last_time)| {
                        last_path == &path && last_time.elapsed() <= Duration::from_millis(350)
                    });
            *last_button_click.borrow_mut() = Some((path.clone(), now));
            let mut playback_ref = playback.borrow_mut();
            let Some(engine) = playback_ref.as_mut() else {
                return;
            };
            engine.clear_comment_loop();
            if restart {
                if let Err(error) = engine.play(&path, Duration::ZERO) {
                    window.set_audio_error(error.into());
                    return;
                }
            } else if engine.path() == Some(path.as_path()) && engine.can_resume() {
                if engine.is_playing() {
                    engine.pause();
                } else {
                    engine.resume();
                }
            } else {
                if let Err(error) = engine.play(&path, Duration::ZERO) {
                    window.set_audio_error(error.into());
                    return;
                }
            }
            select_tree_path(&window, &tree_state, &settings, &path);
            window.set_audio_error("".into());
            window.set_active_audio_path(path.to_string_lossy().into_owned().into());
            window.set_audio_file_name(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .into(),
            );
            window.set_audio_playing(engine.is_playing());
            update_audio_rows(
                &audio_model,
                engine.path(),
                engine.is_playing(),
                engine.position(),
                engine.duration(),
            );
            let should_scroll = audio_model
                .borrow()
                .clone()
                .and_then(|model| {
                    (0..model.row_count()).find(|index| {
                        model
                            .row_data(*index)
                            .is_some_and(|row| Path::new(row.path.as_str()) == path)
                    })
                })
                .map(|index| {
                    let viewport_start = window.get_audio_viewport_start().max(0) as usize;
                    let viewport_end =
                        viewport_start + window.get_audio_visible_rows().max(0) as usize;
                    index < viewport_start || index >= viewport_end
                })
                .unwrap_or(true);
            if should_scroll {
                scroll_audio_to_path(&window, &audio_model, &path);
            }
            if let Some(folder) = audio_folder.borrow().clone() {
                load_workflow_for_audio(&window, &folder, &path, &workflow_loading);
                save_playback_position(&folder, engine);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let playback = Rc::clone(&playback);
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        let workflow_loading = Rc::clone(&workflow_loading);
        window.on_audio_seek(move |path, progress| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let mut playback_ref = playback.borrow_mut();
            let Some(engine) = playback_ref.as_mut() else {
                return;
            };
            engine.clear_comment_loop();
            let progress = progress.clamp(0.0, 1.0);
            let result = if engine.path() == Some(path.as_path()) {
                engine.seek(engine.duration().mul_f32(progress))
            } else {
                engine
                    .play(&path, Duration::ZERO)
                    .and_then(|_| engine.seek(engine.duration().mul_f32(progress)))
            };
            if let Err(error) = result {
                window.set_audio_error(error.into());
                return;
            }
            window.set_audio_error("".into());
            select_tree_path(&window, &tree_state, &settings, &path);
            window.set_active_audio_path(path.to_string_lossy().into_owned().into());
            window.set_audio_file_name(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            );
            update_audio_rows(
                &audio_model,
                engine.path(),
                engine.is_playing(),
                engine.position(),
                engine.duration(),
            );
            if let Some(folder) = audio_folder.borrow().clone() {
                load_workflow_for_audio(&window, &folder, &path, &workflow_loading);
                save_playback_position(&folder, engine);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_close_about(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_about_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_close_settings(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_settings_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_close_tips(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_tips_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_tips_preference_changed(move |hide_tips| {
            settings.borrow_mut().hide_tips_of_the_day = hide_tips;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_hide_tips_of_the_day(hide_tips);
                if hide_tips {
                    window.set_tips_visible(false);
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        window.on_comfyui_sync_requested(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                window.set_comfyui_sync_error(
                    "Open a workspace before configuring ComfyUI sync".into(),
                );
                window.set_comfyui_sync_visible(true);
                return;
            };
            let config = sync::load_config(&folder).unwrap_or_default();
            window.set_comfyui_sync_url(config.url.into());
            window.set_comfyui_sync_remote_directory(config.remote_directory.into());
            window.set_comfyui_sync_local_directory(config.local_directory.into());
            window.set_comfyui_sync_interval(config.interval_ms.to_string().into());
            window.set_comfyui_sync_error("".into());
            window.set_comfyui_sync_test_message("".into());
            window.set_comfyui_sync_test_success(false);
            window.set_comfyui_sync_visible(true);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        window.on_comfyui_sync_choose_directory(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            if let Some(directory) = rfd::FileDialog::new()
                .set_title("Choose ComfyUI download directory")
                .set_directory(&folder)
                .pick_folder()
            {
                if let Ok(relative) = directory.strip_prefix(&folder) {
                    window.set_comfyui_sync_local_directory(
                        relative.to_string_lossy().into_owned().into(),
                    );
                    window.set_comfyui_sync_error("".into());
                    window.set_comfyui_sync_test_message("".into());
                } else {
                    window.set_comfyui_sync_error(
                        "Download directory must be inside the workspace".into(),
                    );
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        window.on_comfyui_sync_test(move |url, remote_directory, local_directory, interval| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                window.set_comfyui_sync_test_success(false);
                window.set_comfyui_sync_test_message(
                    "Open a workspace before testing ComfyUI sync".into(),
                );
                return;
            };
            let Ok(interval_ms) = interval.trim().parse::<u64>() else {
                window.set_comfyui_sync_test_success(false);
                window.set_comfyui_sync_test_message(
                    "Sync frequency must be a number of milliseconds".into(),
                );
                return;
            };
            let config = SyncConfig {
                url: url.to_string(),
                remote_directory: remote_directory.to_string(),
                local_directory: local_directory.to_string(),
                interval_ms,
            }
            .normalized();
            if let Err(error) = config.local_path(&folder) {
                window.set_comfyui_sync_test_success(false);
                window.set_comfyui_sync_test_message(error.to_string().into());
                return;
            }
            match ComfyUiClient::new().and_then(|client| client.list_files(&config)) {
                Ok(files) => {
                    window.set_comfyui_sync_error("".into());
                    window.set_comfyui_sync_test_success(true);
                    window.set_comfyui_sync_test_message(
                        format!("Connection successful: {} file(s) found", files.len()).into(),
                    );
                }
                Err(error) => {
                    window.set_comfyui_sync_test_success(false);
                    window.set_comfyui_sync_test_message(error.to_string().into());
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let sync_controller = Rc::clone(&sync_controller);
        let recreate_workflow_pending = Rc::clone(&recreate_workflow_pending);
        let edited_workflow = Rc::clone(&edited_workflow);
        window.on_comfyui_sync_save(move |url, remote_directory, local_directory, interval| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                window.set_comfyui_sync_error(
                    "Open a workspace before configuring ComfyUI sync".into(),
                );
                return;
            };
            let Ok(interval_ms) = interval.trim().parse::<u64>() else {
                window.set_comfyui_sync_error(
                    "Sync frequency must be a number of milliseconds".into(),
                );
                return;
            };
            let config = SyncConfig {
                url: url.to_string(),
                remote_directory: remote_directory.to_string(),
                local_directory: local_directory.to_string(),
                interval_ms,
            }
            .normalized();
            if let Err(error) = config.local_path(&folder) {
                window.set_comfyui_sync_error(error.to_string().into());
                return;
            }
            sync_controller.borrow_mut().stop();
            let test_result =
                ComfyUiClient::new().and_then(|client| client.list_files(&config).map(|_| ()));
            if let Err(error) = test_result {
                window.set_comfyui_sync_active(false);
                window.set_comfyui_sync_error_state(true);
                window.set_comfyui_sync_error(error.to_string().into());
                return;
            }
            if let Err(error) = sync::ensure_destination(&folder, &config) {
                window.set_comfyui_sync_error(error.to_string().into());
                return;
            }
            if let Err(error) = sync::save_config(&folder, &config) {
                window.set_comfyui_sync_error(error.to_string().into());
                return;
            }
            sync_controller.borrow_mut().start(folder, config);
            window.set_comfyui_sync_error("".into());
            window.set_comfyui_sync_error_state(false);
            window.set_comfyui_sync_visible(false);
            if *recreate_workflow_pending.borrow() {
                *recreate_workflow_pending.borrow_mut() = false;
                if let Some(folder) = audio_folder.borrow().clone() {
                    if let Some(config) = sync::load_config(&folder) {
                        recreate_workflow(&window, &folder, &config, &edited_workflow);
                    }
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_comfyui_sync_cancel(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_comfyui_sync_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_toggle_fullscreen(move || {
            if let Some(window) = weak_window.upgrade() {
                let fullscreen = window.window().is_fullscreen();
                window.window().set_fullscreen(!fullscreen);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_theme_selected(move |light_theme| {
            {
                settings.borrow_mut().light_theme = light_theme;
            }
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_theme_index(if light_theme { 1 } else { 0 });
                window.set_light_theme(light_theme);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        let tree_state = Rc::clone(&tree_state);
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        window.on_sort_order_selected(move |sort_order| {
            settings.borrow_mut().sort_order = sort_order;
            settings::save(&settings.borrow());
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            refresh_tree(&window, &tree_state);
            let Some(workspace) = audio_folder.borrow().clone() else {
                return;
            };
            let folder = settings
                .borrow()
                .last_selected_path
                .as_deref()
                .map(PathBuf::from)
                .filter(|path| path.exists() && path.strip_prefix(&workspace).is_ok())
                .map(|path| {
                    if path.is_dir() {
                        path
                    } else {
                        path.parent().unwrap_or(&workspace).to_path_buf()
                    }
                })
                .unwrap_or(workspace);
            refresh_audio(
                &window,
                &audio_folder,
                &audio_model,
                &audio_load_state,
                folder,
            );
        });
    }

    window.show()?;
    let window_weak = window.as_weak();
    let splash_timer = slint::Timer::default();
    splash_timer.start(
        slint::TimerMode::SingleShot,
        Duration::from_secs(2),
        move || {
            if let Some(window) = window_weak.upgrade() {
                window.set_splash_visible(false);
            }
        },
    );
    std::mem::forget(splash_timer);

    println!("Adb Studio {APP_VERSION} ({BUILD_NUMBER})");
    window.run()?;
    sync_controller.borrow_mut().stop();
    settings::save_window(&window, &mut settings.borrow_mut());
    settings::save(&settings.borrow());
    Ok(())
}

fn sync_cursor_environment() {
    if std::env::var_os("XCURSOR_THEME").is_none() {
        if let Some(theme) = gsettings_value("org.gnome.desktop.interface", "cursor-theme") {
            std::env::set_var("XCURSOR_THEME", theme);
        }
    }
    if std::env::var_os("XCURSOR_SIZE").is_none() {
        if let Some(size) = gsettings_value("org.gnome.desktop.interface", "cursor-size") {
            std::env::set_var("XCURSOR_SIZE", size);
        }
    }
}

fn gsettings_value(schema: &str, key: &str) -> Option<String> {
    let output = Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    Some(value.trim_matches('\'').to_owned())
}

fn set_workspace(
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
                .filter(|path| !is_internal_path(path))
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

    {
        settings.borrow_mut().last_folder = Some(folder.to_string_lossy().into_owned());
    }
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
    if let Some(selected_path) = settings_snapshot
        .last_selected_path
        .as_deref()
        .map(PathBuf::from)
    {
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
    refresh_tree(window, tree_state);
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
    refresh_audio(
        window,
        audio_folder,
        audio_model,
        audio_load_state,
        audio_view_folder,
    );
}

fn close_workspace(
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
) {
    *workspace_watcher.borrow_mut() = None;
    sync_controller.borrow_mut().stop();
    if let Some(engine) = playback.borrow_mut().as_mut() {
        engine.stop();
    }
    {
        let mut state = audio_load_state.lock().unwrap();
        state.folder = PathBuf::new();
        state.paths.clear();
        state.requested_range = None;
        state.generated.clear();
        state.loading.clear();
        state.generation += 1;
        state
            .cancellation_generation
            .store(state.generation, Ordering::Release);
        state.completed = 0;
        state.total = 0;
    }
    *tree_state.borrow_mut() = None;
    *audio_folder.borrow_mut() = None;
    *audio_model.borrow_mut() = None;
    workflow_files.borrow_mut().clear();

    {
        let mut settings = settings.borrow_mut();
        settings.last_folder = None;
        settings.last_selected_path = None;
        settings::save(&settings);
    }

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

fn refresh_workspace(
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
    refresh_tree(window, tree_state);

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
        path.parent() == Some(audio_view_folder.as_path())
            && file_system::FileKind::from_path(path) == file_system::FileKind::Audio
    }) {
        refresh_audio_for_changes(
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

fn track_differences(
    folder: &Path,
    pinned_path: Option<&Path>,
    track_path: &Path,
) -> ModelRc<TrackDifference> {
    let differences = pinned_path
        .map(|pinned_path| metadata::comfyui::compare_files(folder, pinned_path, track_path))
        .unwrap_or_default()
        .into_iter()
        .map(|difference| TrackDifference {
            label: difference.label.into(),
            value: difference.value.into(),
        })
        .collect::<Vec<_>>();
    ModelRc::new(VecModel::from(differences))
}

fn refresh_audio(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    folder: PathBuf,
) {
    refresh_audio_with_changes(
        window,
        audio_folder,
        audio_model,
        audio_load_state,
        folder,
        None,
    );
}

fn refresh_audio_for_changes(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    folder: PathBuf,
    changed_paths: &[PathBuf],
) {
    refresh_audio_with_changes(
        window,
        audio_folder,
        audio_model,
        audio_load_state,
        folder,
        Some(changed_paths),
    );
}

fn refresh_audio_with_changes(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    folder: PathBuf,
    changed_paths: Option<&[PathBuf]>,
) {
    if let Some(workspace) = audio_folder.borrow().clone() {
        set_audio_breadcrumbs(window, &workspace, &folder);
    }
    let filter = window.get_audio_filter().to_string();
    let reload_all = changed_paths.is_none();
    let changed_audio_paths = changed_paths
        .into_iter()
        .flat_map(|paths| paths.iter())
        .filter(|path| path.parent() == Some(folder.as_path()))
        .filter(|path| file_system::FileKind::from_path(path) == file_system::FileKind::Audio)
        .cloned()
        .collect::<HashSet<_>>();
    let existing_rows = audio_model
        .borrow()
        .clone()
        .map(|model| {
            (0..model.row_count())
                .filter_map(|index| model.row_data(index))
                .map(|row| (PathBuf::from(row.path.as_str()), row))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let pinned_path = audio_folder
        .borrow()
        .clone()
        .and_then(|workspace| workspace::preferences::pinned_track(&workspace, &folder));
    let workspace = audio_folder.borrow().clone().unwrap_or_default();
    let index = metadata::load_index(&folder);
    let mut rows = Vec::new();
    for entry in file_system::read_dir_sorted(
        &folder,
        file_system::SortOrder::from_i32(window.get_sort_order()),
    ) {
        if entry.kind != file_system::FileKind::Audio || !matches_audio_filter(&entry.name, &filter)
        {
            continue;
        }
        let should_reload = reload_all || changed_audio_paths.contains(&entry.path);
        if !should_reload {
            if let Some(row) = existing_rows.get(&entry.path) {
                rows.push(row.clone());
                continue;
            }
        }
        let modified_date = fs::metadata(&entry.path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_secs())
            .unwrap_or_default();
        let path_string = entry.path.to_string_lossy().into_owned();
        let stored_position = index
            .audio_files
            .iter()
            .find(|item| item.file_path == path_string);
        let comments = comment_rows(&metadata::load_audio_metadata(&workspace, &entry.path));
        let progress = stored_position
            .as_ref()
            .filter(|item| item.duration_seconds > 0.0)
            .map(|item| (item.last_position_seconds / item.duration_seconds).clamp(0.0, 1.0))
            .unwrap_or(0.0);
        rows.push(AudioRow {
            path: path_string.into(),
            name: entry.name.into(),
            modified_date: modified_date.to_string().into(),
            peaks: ModelRc::new(VecModel::from(vec![0.0; waveform::DISPLAY_PEAK_COUNT])),
            is_loading: false,
            comments,
            differences: track_differences(
                &audio_folder.borrow().clone().unwrap_or_default(),
                pinned_path.as_deref(),
                &entry.path,
            ),
            rating: stored_position
                .as_ref()
                .map(|item| item.normalized_rating() as i32)
                .unwrap_or(0),
            is_pinned: pinned_path.as_deref() == Some(entry.path.as_path()),
            is_active: false,
            is_selected: false,
            is_playing: false,
            progress,
            loop_enabled: false,
            selected_comment_start: -1.0,
            selected_comment_end: -1.0,
        });
    }
    let sort_order = file_system::SortOrder::from_i32(window.get_sort_order());
    rows.sort_by(|left, right| match sort_order {
        file_system::SortOrder::AlphabeticalAscending => left
            .name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase()),
        file_system::SortOrder::AlphabeticalDescending => right
            .name
            .to_ascii_lowercase()
            .cmp(&left.name.to_ascii_lowercase()),
        file_system::SortOrder::ModifiedAscending => left
            .modified_date
            .parse::<u64>()
            .unwrap_or_default()
            .cmp(&right.modified_date.parse::<u64>().unwrap_or_default())
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            }),
        file_system::SortOrder::ModifiedDescending => right
            .modified_date
            .parse::<u64>()
            .unwrap_or_default()
            .cmp(&left.modified_date.parse::<u64>().unwrap_or_default())
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            }),
    });
    let previous_selected_path = PathBuf::from(window.get_selected_audio_path().as_str());
    let selected_path = rows
        .iter()
        .find(|row| Path::new(row.path.as_str()) == previous_selected_path)
        .map(|row| row.path.clone())
        .or_else(|| rows.first().map(|row| row.path.clone()))
        .unwrap_or_default();
    for row in &mut rows {
        row.is_selected = row.path == selected_path;
    }
    window.set_selected_audio_path(selected_path);
    let paths: Vec<PathBuf> = rows
        .iter()
        .map(|row| PathBuf::from(row.path.as_str()))
        .collect();
    let total = paths.len();
    let model = Rc::new(VecModel::from(rows));
    window.set_audio_rows(ModelRc::new(model.clone()));
    *audio_model.borrow_mut() = Some(model);
    {
        let mut state = audio_load_state.lock().unwrap();
        state.folder = audio_folder.borrow().clone().unwrap_or_default();
        state.paths = paths;
        if reload_all {
            state.generated.clear();
            state.loading.clear();
        } else {
            let current_paths = state.paths.iter().cloned().collect::<HashSet<_>>();
            state
                .generated
                .retain(|path| current_paths.contains(path) && !changed_audio_paths.contains(path));
            state
                .loading
                .retain(|path| current_paths.contains(path) && !changed_audio_paths.contains(path));
        }
        state.generation += 1;
        state
            .cancellation_generation
            .store(state.generation, Ordering::Release);
        state.completed = state.generated.len();
        state.total = total;
        state.requested_range = None;
    }
    request_audio_generation(audio_load_state, 0, 1);
}

fn matches_audio_filter(name: &str, filter: &str) -> bool {
    let filter = filter.trim();
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::matches_audio_filter;

    #[test]
    fn audio_filter_matches_names_case_insensitively() {
        assert!(matches_audio_filter("My Voice.WAV", " voice "));
        assert!(matches_audio_filter("My Voice.WAV", ""));
        assert!(!matches_audio_filter("My Voice.WAV", "music"));
    }

}

fn comment_duration(
    folder: &Path,
    path: &Path,
    playback: &Rc<RefCell<Option<PlaybackEngine>>>,
) -> f32 {
    let path_string = path.to_string_lossy();
    let stored_duration = metadata::load_audio_metadata(folder, path).duration_seconds;
    if stored_duration > 0.0 {
        return stored_duration;
    }
    let mut playback_ref = playback.borrow_mut();
    let Some(engine) = playback_ref.as_mut() else {
        return 0.0;
    };
    if engine.path() != Some(path) && engine.play(path, Duration::ZERO).is_err() {
        return 0.0;
    }
    let duration = engine.duration().as_secs_f32();
    if duration > 0.0 {
        let mut metadata = metadata::load_audio_metadata(folder, path);
        metadata.file_path = path_string.into_owned();
        metadata.duration_seconds = duration;
        metadata::save_audio_metadata(folder, path, &metadata);
    }
    duration
}

fn save_playback_position(folder: &Path, engine: &PlaybackEngine) {
    let Some(path) = engine.path() else {
        return;
    };
    let path_string = path.to_string_lossy().into_owned();
    let mut index = metadata::load_index(folder);
    if let Some(stored) = index
        .audio_files
        .iter_mut()
        .find(|item| item.file_path == path_string)
    {
        stored.last_position_seconds = engine.position().as_secs_f32();
        stored.duration_seconds = engine.duration().as_secs_f32();
    } else {
        index.audio_files.push(metadata::AudioFileMetadata {
            file_path: path_string,
            last_position_seconds: engine.position().as_secs_f32(),
            duration_seconds: engine.duration().as_secs_f32(),
            ..Default::default()
        });
    }
    metadata::save_index(folder, &index);
}

fn select_tree_path(
    window: &MainWindow,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    settings: &Rc<RefCell<AppSettings>>,
    path: &Path,
) {
    let mut state_ref = tree_state.borrow_mut();
    let Some(state) = state_ref.as_mut() else {
        return;
    };
    state.select_and_expand(path);
    let tree_index = file_system::build_visible_rows(
        state,
        file_system::SortOrder::from_i32(window.get_sort_order()),
    )
    .iter()
    .position(|row| row.path == path)
    .map(|index| index as i32);
    settings.borrow_mut().last_selected_path = Some(path.to_string_lossy().into_owned());
    let settings_snapshot = settings.borrow().clone();
    settings::save(&settings_snapshot);
    window.set_selected_name(
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .into(),
    );
    drop(state_ref);
    refresh_tree(window, tree_state);
    window.set_tree_scroll_to_index(-1);
    window.set_tree_scroll_to_index(tree_index.unwrap_or(-1));
}

fn set_audio_breadcrumbs(window: &MainWindow, workspace: &Path, folder: &Path) {
    let mut rows = Vec::new();
    let mut path = workspace.to_path_buf();
    let workspace_name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| workspace.to_str().unwrap_or("Workspace"));
    rows.push(BreadcrumbRow {
        name: workspace_name.to_owned().into(),
        path: path.to_string_lossy().into_owned().into(),
    });

    if let Ok(relative) = folder.strip_prefix(workspace) {
        for component in relative.components() {
            path.push(component.as_os_str());
            rows.push(BreadcrumbRow {
                name: component.as_os_str().to_string_lossy().into_owned().into(),
                path: path.to_string_lossy().into_owned().into(),
            });
        }
    }

    window.set_audio_breadcrumbs(ModelRc::new(VecModel::from(rows)));
}

fn request_audio_generation(
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    start_index: usize,
    visible_rows: usize,
) {
    loader::request(audio_load_state, start_index, visible_rows);
}

fn refresh_tree(window: &MainWindow, tree_state: &Rc<RefCell<Option<TreeState>>>) {
    let state_ref = tree_state.borrow();
    let Some(state) = state_ref.as_ref() else {
        return;
    };
    window.set_tree_selection_count(state.selected_paths().len() as i32);

    let rows: Vec<TreeRow> = file_system::build_visible_rows(
        state,
        file_system::SortOrder::from_i32(window.get_sort_order()),
    )
    .into_iter()
    .map(|row| TreeRow {
        path: row.path.to_string_lossy().into_owned().into(),
        name: row.name.into(),
        depth: row.depth,
        is_dir: row.is_dir,
        is_expanded: row.is_expanded,
        is_selected: row.is_selected,
        kind: row.kind.as_str().into(),
    })
    .collect();

    window.set_tree_rows(ModelRc::new(VecModel::from(rows)));
}
