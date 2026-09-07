use std::{
    cell::RefCell,
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::mpsc,
    sync::{
        atomic::Ordering,
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use audio::conversion::{self, ConversionJob};
use audio::playback::PlaybackEngine;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

mod app;
mod audio;
mod metadata;
mod settings;
mod sync;
mod workspace;
use audio::waveform;
use audio::session::{comment_duration, request_audio_generation, save_playback_position};
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
use workspace::library::{refresh_audio, refresh_audio_for_changes, track_differences};
use workspace::lifecycle;
use workspace::tree_nav::{refresh_tree, select_tree_path};
use workspace::workflow::{clear_workflow, load_workflow_for_audio, scan_json_files};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    app::env::sync_cursor_environment();
    let window = MainWindow::new()?;
    slint::set_xdg_app_id("com.adbstudio.AdbStudio")?;
    let state = app::AppState::new();
    let settings = state.settings.clone();
    *settings.borrow_mut() = settings::load();
    settings::restore_window(&window, &mut settings.borrow_mut());
    let tree_state = state.tree_state.clone();
    let audio_folder = state.audio_folder.clone();
    let workflow_files = state.workflow_files.clone();
    let edited_workflow = state.edited_workflow.clone();
    let workflow_loading = state.workflow_loading.clone();
    let recreate_workflow_pending = state.recreate_workflow_pending.clone();
    let workspace_watcher = state.workspace_watcher.clone();
    let (workspace_change_sender, workspace_change_receiver) = mpsc::channel::<Vec<PathBuf>>();
    let audio_model = state.audio_model.clone();
    let sync_controller = state.sync_controller.clone();
    let comment_editor_original = state.comment_editor_original.clone();
    let comment_editor_duration = state.comment_editor_duration.clone();
    let last_button_click = state.last_button_click.clone();
    let conversion_receiver = state.conversion_receiver.clone();
    let conversion_cancelled = state.conversion_cancelled.clone();
    let conversion_jobs = state.conversion_jobs.clone();
    let conversion_temp_root = state.conversion_temp_root.clone();
    let conversion_target = state.conversion_target.clone();
    let conversion_model = state.conversion_model.clone();
    let workflow_run_sender = state.workflow_run_sender.clone();
    let workflow_run_receiver = state.workflow_run_receiver.clone();
    let workflow_run_cancelled = state.workflow_run_cancelled.clone();
    let audio_result_receiver = state.audio_result_receiver.clone();
    let (playback, playback_error) = match PlaybackEngine::new() {
        Ok(engine) => (Rc::new(RefCell::new(Some(engine))), None),
        Err(error) => (Rc::new(RefCell::new(None)), Some(error)),
    };
    if let Some(error) = playback_error {
        window.set_audio_error(error.into());
    }
    let audio_load_state = state.audio_load_state.clone();
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

    app::playback_controller::register_playback_callbacks(
        &window,
        &playback,
        &audio_model,
        &audio_folder,
        &settings,
        &tree_state,
        &workflow_loading,
        &comment_editor_original,
        &comment_editor_duration,
        &last_button_click,
    );

    app::workflow_controller::register_workflow_callbacks(
        &window,
        &settings,
        &tree_state,
        &audio_folder,
        &audio_model,
        &audio_load_state,
        &workflow_loading,
        &edited_workflow,
        &workflow_run_cancelled,
        &workflow_run_sender,
        &playback,
        &recreate_workflow_pending,
        &comment_editor_original,
        &comment_editor_duration,
    );

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

    let last_folder = settings.borrow().last_folder.clone();
    if let Some(last_folder) = last_folder {
        let folder = PathBuf::from(last_folder);
        if folder.is_dir() {
            lifecycle::set_workspace(
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
                        lifecycle::refresh_workspace(
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
                if let Some(receiver) = audio_result_receiver.borrow_mut().as_mut() {
                    for result in receiver.try_iter().take(3) {
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
                }
            },
        );
        std::mem::forget(timer);
    }

    app::window_controller::register_window_callbacks(
        &window,
        &settings,
        &tree_state,
        &audio_folder,
        &audio_model,
        &audio_load_state,
        &sync_controller,
        &workflow_files,
        &workspace_watcher,
        &workspace_change_sender,
        &playback,
        &workflow_loading,
    );

    app::sync_ui_controller::register_sync_ui_callbacks(
        &window,
        &settings,
        &tree_state,
        &audio_folder,
        &audio_model,
        &audio_load_state,
        &sync_controller,
        &recreate_workflow_pending,
        &edited_workflow,
    );

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


