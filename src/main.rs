use std::{
    cell::RefCell,
    env,
    path::PathBuf,
    rc::Rc,
    sync::mpsc,
    sync::Arc,
    time::{Duration, Instant},
};

use audio::playback::PlaybackEngine;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

mod app;
mod audio;
mod metadata;
mod settings;
mod sync;
mod workspace;
use audio::waveform;
use audio::session::request_audio_generation;

slint::include_modules!();

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_NUMBER: &str = env!("ADB_BUILD_NUMBER");
use audio::loader::State as AudioLoadState;
use audio::view::update_audio_loading_rows;
use sync::WorkflowRunUpdate;
use workspace::lifecycle;
use workspace::tree_nav::refresh_tree;

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
        &audio_folder,
        &workflow_loading,
        &edited_workflow,
        &workflow_run_cancelled,
        &workflow_run_sender,
        &recreate_workflow_pending,
    );

    app::metadata_pane_controller::register_metadata_pane_callbacks(
        &window,
        &settings,
        &tree_state,
        &audio_folder,
        &audio_model,
        &audio_load_state,
        &playback,
        &comment_editor_original,
        &comment_editor_duration,
    );

    app::tree_controller::register_tree_callbacks(
        &window,
        &settings,
        &tree_state,
        &audio_folder,
        &audio_model,
        &audio_load_state,
        &edited_workflow,
        &workflow_loading,
    );

    app::conversion_controller::register_conversion_callbacks(
        &window,
        &conversion_target,
        &conversion_jobs,
        &conversion_model,
        &conversion_receiver,
        &conversion_cancelled,
        &conversion_temp_root,
        &audio_folder,
        &playback,
    );

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
                        app::conversion_controller::tick(
                            &window,
                            &conversion_receiver,
                            &conversion_jobs,
                            &conversion_model,
                            &conversion_temp_root,
                            &conversion_cancelled,
                            &mut conversion_updates,
                            &audio_folder,
                            &audio_model,
                            &audio_load_state,
                            &tree_state_for_conversion,
                        );
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
                    app::playback_controller::tick(
                        &window,
                        &playback,
                        &audio_model,
                        &audio_folder,
                        &settings,
                        &last_persisted_position_for_timer,
                    );
                    let state = audio_load_state.lock().unwrap();
                    window.set_audio_loading(state.running);
                    window.set_audio_completed(state.completed as i32);
                    window.set_audio_total(state.total as i32);
                    if state.running {
                        window.set_audio_spinner(["|", "/", "-", "\\"][spinner_frame].into());
                        spinner_frame = (spinner_frame + 1) % 4;
                    }
                    drop(state);
                    app::sync_ui_controller::tick(
                        &window,
                        &settings,
                        &tree_state,
                        &audio_folder,
                        &audio_model,
                        &audio_load_state,
                        &sync_controller,
                        &playback,
                        &workflow_loading,
                        &mut sync_spinner_frame,
                    );
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

