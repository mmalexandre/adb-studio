use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use slint::{ComponentHandle, Model};

use crate::{
    audio::{
        playback::PlaybackEngine,
        session::{comment_duration, save_playback_position},
        view::{
            format_duration, scroll_to_path_if_needed as scroll_audio_to_path, select_audio_path,
            select_comment, update_audio_rows,
        },
    },
    metadata::AudioComment,
    workspace::{file_system::TreeState, tree_nav::select_tree_path},
    MainWindow,
};

pub fn register_playback_callbacks(
    window: &MainWindow,
    playback: &Rc<RefCell<Option<PlaybackEngine>>>,
    audio_model: &Rc<RefCell<Option<Rc<slint::VecModel<crate::AudioRow>>>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    settings: &Rc<RefCell<crate::settings::AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    workflow_loading: &Rc<RefCell<bool>>,
    loaded_workflow_path: &Rc<RefCell<Option<PathBuf>>>,
    comment_editor_original: &Rc<RefCell<Option<AudioComment>>>,
    comment_editor_duration: &Rc<RefCell<f32>>,
    last_button_click: &Rc<RefCell<Option<(PathBuf, Instant)>>>,
) {
    {
        let playback = Rc::clone(playback);
        window.on_volume_changed(move |volume| {
            if let Some(engine) = playback.borrow_mut().as_mut() {
                engine.set_volume(volume);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(audio_model);
        let audio_folder = Rc::clone(audio_folder);
        let playback = Rc::clone(playback);
        let comment_editor_original = Rc::clone(comment_editor_original);
        let comment_editor_duration = Rc::clone(comment_editor_duration);
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
            _ = comment_editor_original;
            _ = comment_editor_duration;
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(audio_model);
        let audio_folder = Rc::clone(audio_folder);
        let playback = Rc::clone(playback);
        let comment_editor_original = Rc::clone(comment_editor_original);
        let comment_editor_duration = Rc::clone(comment_editor_duration);
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
        let audio_model = Rc::clone(audio_model);
        let audio_folder = Rc::clone(audio_folder);
        let playback = Rc::clone(playback);
        let last_button_click = Rc::clone(last_button_click);
        let tree_state = Rc::clone(tree_state);
        let settings = Rc::clone(settings);
        let workflow_loading = Rc::clone(workflow_loading);
        let loaded_workflow_path = Rc::clone(loaded_workflow_path);
        window.on_audio_play(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let now = Instant::now();
            let restart = last_button_click
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
                crate::workspace::workflow::load_workflow_for_audio(
                    &window,
                    &folder,
                    &path,
                    &workflow_loading,
                    &loaded_workflow_path,
                    false,
                );
                save_playback_position(&folder, engine);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let playback = Rc::clone(playback);
        let audio_model = Rc::clone(audio_model);
        let audio_folder = Rc::clone(audio_folder);
        let tree_state = Rc::clone(tree_state);
        let settings = Rc::clone(settings);
        let workflow_loading = Rc::clone(workflow_loading);
        let loaded_workflow_path = Rc::clone(loaded_workflow_path);
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
                crate::workspace::workflow::load_workflow_for_audio(
                    &window,
                    &folder,
                    &path,
                    &workflow_loading,
                    &loaded_workflow_path,
                    false,
                );
                save_playback_position(&folder, engine);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(audio_model);
        let audio_folder = Rc::clone(audio_folder);
        let playback = Rc::clone(playback);
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
        let audio_model = Rc::clone(audio_model);
        let audio_folder = Rc::clone(audio_folder);
        let _playback = Rc::clone(playback);
        let last_button_click = Rc::clone(last_button_click);
        let tree_state = Rc::clone(tree_state);
        let settings = Rc::clone(settings);
        let workflow_loading = Rc::clone(workflow_loading);
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
            let _ = (&last_button_click, &tree_state, &settings, &workflow_loading, &audio_folder);
        });
    }
}

fn format_seconds(seconds: f32) -> String {
    let total = seconds.max(0.0) as i64;
    let mins = total / 60;
    let secs = total % 60;
    format!("{mins}:{secs:02}")
}

/// Advances playback position, handles loop/auto-advance, and persists position periodically.
pub fn tick(
    window: &MainWindow,
    playback: &Rc<RefCell<Option<PlaybackEngine>>>,
    audio_model: &Rc<RefCell<Option<Rc<slint::VecModel<crate::AudioRow>>>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    settings: &Rc<RefCell<crate::settings::AppSettings>>,
    last_persisted_position: &Rc<RefCell<Instant>>,
) {
    let mut folder_next_path = None;
    if let Some(engine) = playback.borrow_mut().as_mut() {
        engine.update_position();
        let comment_loop = engine.comment_loop();
        let should_loop = comment_loop
            .map(|(_, end)| engine.position() >= end)
            .unwrap_or_else(|| {
                engine.has_finished() || (!engine.is_playing() && engine.position() >= engine.duration())
            });
        let loop_mode = settings.borrow().loop_mode;
        if (loop_mode == 1 || (loop_mode == 2 && comment_loop.is_some()))
            && !engine.duration().is_zero()
            && should_loop
        {
            if let Some(path) = engine.path().map(Path::to_path_buf) {
                let loop_start = comment_loop.map(|(start, _)| start).unwrap_or(Duration::ZERO);
                let _ = engine.play(&path, loop_start);
            }
        } else if loop_mode == 2 && !engine.duration().is_zero() && should_loop {
            if let Some(path) = engine.path() {
                if let Some(model) = audio_model.borrow().clone() {
                    let row_count = model.row_count();
                    let current_index = (0..row_count).find(|index| {
                        model
                            .row_data(*index)
                            .is_some_and(|row| row.path.as_str() == path.to_string_lossy().as_ref())
                    });
                    if let Some(current_index) = current_index {
                        let next_index = (current_index + 1) % row_count;
                        folder_next_path = model.row_data(next_index).map(|row| row.path);
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
        update_audio_rows(audio_model, engine.path(), playing, position, duration);
        if last_persisted_position.borrow().elapsed() >= Duration::from_millis(500) {
            if let Some(folder) = audio_folder.borrow().clone() {
                save_playback_position(&folder, engine);
            }
            *last_persisted_position.borrow_mut() = Instant::now();
        }
    }
    if let Some(path) = folder_next_path {
        window.invoke_audio_play(path);
    }
}
