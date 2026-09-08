use std::{
    cell::RefCell,
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{atomic::Ordering, mpsc, Arc, Mutex},
};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{
    audio::{conversion::{self, ConversionJob, ConversionUpdate}, loader::State as AudioLoadState, playback::PlaybackEngine},
    workspace::{file_system::TreeState, library::refresh_audio, tree_nav::refresh_tree},
    ConversionRow, MainWindow,
};

/// Wires the conversion dialog callbacks (target selection, start, cancel, close).
#[allow(clippy::too_many_arguments)]
pub fn register_conversion_callbacks(
    window: &MainWindow,
    conversion_target: &Rc<RefCell<Option<PathBuf>>>,
    conversion_jobs: &Rc<RefCell<Vec<ConversionJob>>>,
    conversion_model: &Rc<RefCell<Option<Rc<VecModel<ConversionRow>>>>>,
    conversion_receiver: &Rc<RefCell<Option<mpsc::Receiver<ConversionUpdate>>>>,
    conversion_cancelled: &Rc<RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>>,
    conversion_temp_root: &Rc<RefCell<Option<PathBuf>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    playback: &Rc<RefCell<Option<PlaybackEngine>>>,
) {
    {
        let target = Rc::clone(conversion_target);
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
        let target = Rc::clone(conversion_target);
        let jobs_state = Rc::clone(conversion_jobs);
        let model_state = Rc::clone(conversion_model);
        let receiver_state = Rc::clone(conversion_receiver);
        let cancelled_state = Rc::clone(conversion_cancelled);
        let temp_state = Rc::clone(conversion_temp_root);
        let playback = Rc::clone(playback);
        let audio_folder = Rc::clone(audio_folder);
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
        let cancelled_state = Rc::clone(conversion_cancelled);
        window.on_conversion_cancelled(move || {
            if let Some(cancelled) = cancelled_state.borrow().as_ref() {
                cancelled.store(true, Ordering::Release);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let jobs_state = Rc::clone(conversion_jobs);
        let temp_state = Rc::clone(conversion_temp_root);
        let receiver_state = Rc::clone(conversion_receiver);
        let cancelled_state = Rc::clone(conversion_cancelled);
        let model_state = Rc::clone(conversion_model);
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
}

/// Drains conversion progress updates and finalizes the job batch once complete.
/// Returns true if a batch just finished (caller may want to refresh other UI).
#[allow(clippy::too_many_arguments)]
pub fn tick(
    window: &MainWindow,
    conversion_receiver: &Rc<RefCell<Option<mpsc::Receiver<ConversionUpdate>>>>,
    conversion_jobs: &Rc<RefCell<Vec<ConversionJob>>>,
    conversion_model: &Rc<RefCell<Option<Rc<VecModel<ConversionRow>>>>>,
    conversion_temp_root: &Rc<RefCell<Option<PathBuf>>>,
    conversion_cancelled: &Rc<RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>>,
    conversion_updates: &mut usize,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<crate::AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
) {
    let mut conversion_finished = false;
    if let Some(receiver) = conversion_receiver.borrow_mut().as_mut() {
        while let Ok(update) = receiver.try_recv() {
            *conversion_updates += 1;
            if let Some(model) = conversion_model.borrow().as_ref() {
                if let Some(mut row) = model.row_data(update.index) {
                    row.progress = update.progress;
                    row.status = update.status.into();
                    model.set_row_data(update.index, row);
                }
            }
        }
        conversion_finished =
            *conversion_updates >= conversion_jobs.borrow().len() && !conversion_jobs.borrow().is_empty();
    }
    if !conversion_finished {
        return;
    }
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
            if let Err(error) =
                crate::metadata::rename_associated_workflow(folder, &job.source, &job.destination)
            {
                errors.push(error.to_string());
            }
            crate::metadata::rename_audio_metadata(folder, &job.source, &job.destination);
        }
    }
    if let Some(root) = conversion_temp_root.borrow_mut().take() {
        let _ = fs::remove_dir_all(root);
    }
    *conversion_receiver.borrow_mut() = None;
    *conversion_cancelled.borrow_mut() = None;
    *conversion_updates = 0;
    window.set_conversion_complete(true);
    refresh_tree(window, tree_state);
    let audio_view_folder = {
        let state_ref = tree_state.borrow();
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
            window,
            audio_folder,
            audio_model,
            audio_load_state,
            audio_view_folder,
        );
    }
    if !errors.is_empty() {
        window.set_audio_error(format!("Conversion: {}", errors.join("; ")).into());
    }
}
