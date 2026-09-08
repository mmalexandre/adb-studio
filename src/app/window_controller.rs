use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};

use notify::RecommendedWatcher;
use slint::ComponentHandle;

use crate::{
    audio::{
        loader::State as AudioLoadState,
        playback::PlaybackEngine,
        view::{format_duration, scroll_to_path as scroll_audio_to_path, update_audio_rows},
    },
    settings::{self, AppSettings},
    workspace::{
        file_system::{self, TreeState},
        lifecycle::{close_workspace, set_workspace},
        tree_nav::refresh_tree,
    },
    MainWindow,
};

pub fn register_window_callbacks(
    window: &MainWindow,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<slint::VecModel<crate::AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    sync_controller: &Rc<RefCell<crate::sync::SyncController>>,
    workflow_files: &Rc<RefCell<Vec<(String, String)>>>,
    workspace_watcher: &Rc<RefCell<Option<RecommendedWatcher>>>,
    workspace_change_sender: &mpsc::Sender<Vec<PathBuf>>,
    playback: &Rc<RefCell<Option<PlaybackEngine>>>,
    workflow_loading: &Rc<RefCell<bool>>,
    edited_workflow: &Rc<RefCell<Option<serde_json::Value>>>,
    edited_workflow_path: &Rc<RefCell<Option<PathBuf>>>,
    loaded_workflow_path: &Rc<RefCell<Option<PathBuf>>>,
) {
    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(settings);
        let tree_state = Rc::clone(tree_state);
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let audio_load_state = Arc::clone(audio_load_state);
        let sync_controller = Rc::clone(sync_controller);
        let workflow_files = Rc::clone(workflow_files);
        let workspace_watcher = Rc::clone(workspace_watcher);
        let workspace_change_sender = workspace_change_sender.clone();
        let edited_workflow = Rc::clone(edited_workflow);
        let edited_workflow_path = Rc::clone(edited_workflow_path);
        let loaded_workflow_path = Rc::clone(loaded_workflow_path);
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
                    &edited_workflow,
                    &edited_workflow_path,
                    &loaded_workflow_path,
                );
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(settings);
        let tree_state = Rc::clone(tree_state);
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let audio_load_state = Arc::clone(audio_load_state);
        let workflow_files = Rc::clone(workflow_files);
        let sync_controller = Rc::clone(sync_controller);
        let playback = Rc::clone(playback);
        let workspace_watcher = Rc::clone(workspace_watcher);
        let edited_workflow = Rc::clone(edited_workflow);
        let edited_workflow_path = Rc::clone(edited_workflow_path);
        let loaded_workflow_path = Rc::clone(loaded_workflow_path);
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
                &edited_workflow,
                &edited_workflow_path,
                &loaded_workflow_path,
            );
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(settings);
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
        let settings = Rc::clone(settings);
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
        let settings = Rc::clone(settings);
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
        let settings = Rc::clone(settings);
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
        let settings = Rc::clone(settings);
        let playback = Rc::clone(playback);
        let audio_model = Rc::clone(audio_model);
        let audio_folder = Rc::clone(audio_folder);
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
                    crate::audio::session::save_playback_position(&folder, engine);
                }
                window.set_audio_current_time(format_duration(engine.position()).into());
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(settings);
        let tree_state = Rc::clone(tree_state);
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let audio_load_state = Arc::clone(audio_load_state);
        let playback = Rc::clone(playback);
        let workflow_loading = Rc::clone(workflow_loading);
        let loaded_workflow_path = Rc::clone(loaded_workflow_path);
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
                crate::workspace::library::refresh_audio(
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
                crate::workspace::library::refresh_audio(
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
                    crate::workspace::workflow::load_workflow_for_audio(
                        &window,
                        &folder,
                        &path,
                        &workflow_loading,
                        &loaded_workflow_path,
                        false,
                    );
                    crate::audio::session::save_playback_position(&folder, engine);
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let audio_load_state = Arc::clone(audio_load_state);
        window.on_filter_changed(move |filter| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            window.set_audio_filter(filter);
            let folder = audio_folder.borrow().clone();
            if let Some(folder) = folder {
                crate::workspace::library::refresh_audio(
                    &window,
                    &audio_folder,
                    &audio_model,
                    &audio_load_state,
                    folder,
                );
            }
        });
    }
}
