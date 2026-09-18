use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{atomic::Ordering, mpsc, Arc},
};

use slint::ComponentHandle;

use crate::{
    audio::stem_separation::{self, StemJob, StemUpdate},
    settings::{self, AppSettings},
    MainWindow,
};

pub fn register_callbacks(
    window: &MainWindow,
    settings: &Rc<RefCell<AppSettings>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    receiver: &Rc<RefCell<Option<mpsc::Receiver<StemUpdate>>>>,
    cancelled: &Rc<RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>>,
    job_state: &Rc<RefCell<Option<StemJob>>>,
) {
    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(settings);
        let audio_folder = Rc::clone(audio_folder);
        let job_state = Rc::clone(job_state);
        window.on_stem_separation_requested(move |path| {
            let Some(window) = weak_window.upgrade() else { return; };
            let Some(workspace) = audio_folder.borrow().clone() else {
                window.set_audio_error("Open a workspace before exporting stems".into());
                return;
            };
            let source = PathBuf::from(path.as_str());
            if !stem_separation::is_audio(&source) {
                window.set_audio_error("Stem separation requires an audio file".into());
                return;
            }
            let current = settings.borrow().clone();
            let job = StemJob {
                workspace,
                source,
                output_folder: current.stem_output_folder,
                format: current.stem_format,
                overwrite: false,
            };
            match stem_separation::is_exported(&job) {
                Ok(true) => {
                    *job_state.borrow_mut() = Some(job);
                    window.set_stem_regenerate_visible(true);
                }
                Ok(false) => {
                    *job_state.borrow_mut() = Some(job);
                    window.set_stem_install_visible(stem_separation::demucs_python().is_none());
                    if stem_separation::demucs_python().is_some() {
                        window.invoke_stem_install_started();
                    }
                }
                Err(error) => window.set_audio_error(format!("Stem separation: {error}").into()),
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let job_state = Rc::clone(job_state);
        window.on_stem_regenerate_requested(move || {
            let Some(window) = weak_window.upgrade() else { return; };
            if let Some(job) = job_state.borrow_mut().as_mut() {
                job.overwrite = true;
            }
            window.set_stem_regenerate_visible(false);
            if stem_separation::demucs_python().is_none() {
                window.set_stem_install_visible(true);
            } else {
                window.invoke_stem_install_started();
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let job_state = Rc::clone(job_state);
        window.on_stem_regenerate_cancelled(move || {
            *job_state.borrow_mut() = None;
            if let Some(window) = weak_window.upgrade() {
                window.set_stem_regenerate_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let job_state = Rc::clone(job_state);
        let receiver = Rc::clone(receiver);
        let cancelled = Rc::clone(cancelled);
        window.on_stem_install_started(move || {
            let Some(window) = weak_window.upgrade() else { return; };
            let Some(job) = job_state.borrow().clone() else { return; };
            let (sender, new_receiver) = mpsc::channel();
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            stem_separation::start(job, sender, Arc::clone(&stop));
            *receiver.borrow_mut() = Some(new_receiver);
            *cancelled.borrow_mut() = Some(stop);
            window.set_stem_install_visible(false);
            window.set_stem_progress(0.0);
            window.set_stem_status("Preparing Demucs".into());
            window.set_stem_progress_visible(true);
        });
    }

    {
        let weak_window = window.as_weak();
        let cancelled = Rc::clone(cancelled);
        window.on_stem_separation_cancelled(move || {
            if let Some(stop) = cancelled.borrow().as_ref() {
                stop.store(true, Ordering::Release);
            }
            if let Some(window) = weak_window.upgrade() {
                window.set_stem_status("Cancelling...".into());
            }
        });
    }

    let weak_window = window.as_weak();
    window.on_stem_install_cancelled(move || {
        if let Some(window) = weak_window.upgrade() {
            window.set_stem_install_visible(false);
        }
    });

    let settings = Rc::clone(settings);
    let weak_window = window.as_weak();
    window.on_stem_settings_changed(move |format, folder| {
        if let Err(error) = save_settings(&settings, format.as_str(), folder.as_str()) {
            if let Some(window) = weak_window.upgrade() {
                window.set_audio_error(format!("Stem settings: {error}").into());
            }
        }
    });
}

pub fn tick(
    window: &MainWindow,
    receiver: &Rc<RefCell<Option<mpsc::Receiver<StemUpdate>>>>,
    cancelled: &Rc<RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>>,
    job_state: &Rc<RefCell<Option<StemJob>>>,
) {
    let mut finished = None;
    if let Some(receiver) = receiver.borrow_mut().as_mut() {
        while let Ok(update) = receiver.try_recv() {
            match update {
                StemUpdate::Progress { progress, status } => {
                    window.set_stem_progress(progress);
                    window.set_stem_status(status.into());
                }
                StemUpdate::Complete => finished = Some(None),
                StemUpdate::Cancelled => finished = Some(Some("Stem export cancelled".to_string())),
                StemUpdate::Error(error) => finished = Some(Some(error)),
            }
        }
    }
    let Some(message) = finished else { return; };
    *receiver.borrow_mut() = None;
    *cancelled.borrow_mut() = None;
    *job_state.borrow_mut() = None;
    window.set_stem_progress_visible(false);
    if let Some(message) = message {
        window.set_audio_error(message.into());
    } else {
        window.set_audio_error("Stem export complete".into());
    }
}

pub fn save_settings(settings: &Rc<RefCell<AppSettings>>, format: &str, folder: &str) -> Result<(), String> {
    if !matches!(format, "flac" | "wav" | "mp3") {
        return Err("Stem format must be FLAC, WAV, or MP3".into());
    }
    stem_separation::normalized_output_folder(folder)?;
    let mut settings = settings.borrow_mut();
    settings.stem_format = format.to_owned();
    settings.stem_output_folder = folder.trim().to_owned();
    settings::save(&settings);
    Ok(())
}
