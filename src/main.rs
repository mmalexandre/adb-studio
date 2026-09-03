use std::{
    cell::RefCell,
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    sync::mpsc::{self, Sender},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use slint::{Model, ModelRc, VecModel};

mod file_system;
mod metadata;
mod waveform;
use file_system::TreeState;

slint::include_modules!();

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_NUMBER: &str = env!("ADB_BUILD_NUMBER");
const AUDIO_PREFETCH_ROWS: usize = 32;
const AUDIO_PREFETCH_BEFORE: usize = 4;
struct AudioLoadState {
    folder: PathBuf,
    paths: Vec<PathBuf>,
    requested_range: Option<(usize, usize)>,
    generated: HashSet<PathBuf>,
    generation: u64,
    running: bool,
    completed: usize,
    total: usize,
    result_sender: Sender<AudioResult>,
}

struct AudioResult {
    generation: u64,
    index: usize,
    path: String,
    peaks: Vec<f32>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct AppSettings {
    last_folder: Option<String>,
    last_selected_path: Option<String>,
    light_theme: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MainWindow::new()?;
    let settings = Rc::new(RefCell::new(load_settings()));
    let tree_state: Rc<RefCell<Option<TreeState>>> = Rc::new(RefCell::new(None));
    let audio_folder: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let audio_model: Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>> = Rc::new(RefCell::new(None));
    let (audio_result_sender, audio_result_receiver) = mpsc::channel();
    let audio_load_state = Arc::new(Mutex::new(AudioLoadState {
        folder: PathBuf::new(),
        paths: Vec::new(),
        requested_range: None,
        generated: HashSet::new(),
        generation: 0,
        running: false,
        completed: 0,
        total: 0,
        result_sender: audio_result_sender,
    }));
    window.set_build_number(BUILD_NUMBER.into());
    window.set_light_theme(settings.borrow().light_theme);

    let last_folder = settings.borrow().last_folder.clone();
    if let Some(last_folder) = last_folder {
        let folder = PathBuf::from(last_folder);
        if folder.is_dir() {
            set_workspace(&window, folder, &settings, &tree_state, &audio_folder, &audio_model, &audio_load_state);
        }
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let audio_result_receiver = Rc::new(RefCell::new(audio_result_receiver));
        let mut spinner_frame = 0usize;
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(40), move || {
            if let Some(window) = weak_window.upgrade() {
                let state = audio_load_state.lock().unwrap();
                window.set_audio_loading(state.running);
                window.set_audio_completed(state.completed as i32);
                window.set_audio_total(state.total as i32);
                if state.running {
                    window.set_audio_spinner(["|", "/", "-", "\\"][spinner_frame].into());
                    spinner_frame = (spinner_frame + 1) % 4;
                }
            }
            let Some(model) = audio_model.borrow().clone() else {
                return;
            };
            for result in audio_result_receiver.borrow_mut().try_iter().take(3) {
                let current_generation = audio_load_state.lock().unwrap().generation;
                if result.generation != current_generation || result.index >= model.row_count() {
                    continue;
                }
                let Some(row) = model.row_data(result.index) else {
                    continue;
                };
                model.set_row_data(result.index, AudioRow {
                    path: result.path.into(),
                    name: row.name,
                    modified_date: row.modified_date,
                    peaks: ModelRc::new(VecModel::from(waveform::aggregate_peaks(&result.peaks))),
                });
            }
        });
        std::mem::forget(timer);
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        let tree_state = Rc::clone(&tree_state);
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        window.on_open_folder(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };

            if let Some(folder) = rfd::FileDialog::new().set_title("Open Adb Studio Workspace").pick_folder() {
                set_workspace(&window, folder, &settings, &tree_state, &audio_folder, &audio_model, &audio_load_state);
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
        window.on_row_clicked(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());

            let mut state_ref = tree_state.borrow_mut();
            let Some(state) = state_ref.as_mut() else {
                return;
            };
            if path.is_dir() {
                state.toggle(&path);
            }
            state.select(&path);
            let selected_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            settings.borrow_mut().last_selected_path = Some(path.to_string_lossy().into_owned());
            let settings_snapshot = settings.borrow().clone();
            save_settings(&settings_snapshot);
            drop(state_ref);

            window.set_selected_name(selected_name.into());
            refresh_tree(&window, &tree_state);
            if path.is_dir() {
                refresh_audio(&window, &audio_folder, &audio_model, &audio_load_state, path);
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
            if let Some(folder) = audio_folder.borrow().clone() {
                refresh_audio(&window, &audio_folder, &audio_model, &audio_load_state, folder);
            }
        });
    }

    {
        let audio_load_state = Arc::clone(&audio_load_state);
        window.on_audio_viewport_changed(move |start_index| {
            request_audio_generation(&audio_load_state, start_index as usize);
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
        let settings = Rc::clone(&settings);
        window.on_theme_selected(move |light_theme| {
            {
                settings.borrow_mut().light_theme = light_theme;
            }
            let settings_snapshot = settings.borrow().clone();
            save_settings(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_light_theme(light_theme);
            }
        });
    }

    println!("Adb Studio {APP_VERSION} ({BUILD_NUMBER})");
    window.run()?;
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
) {
    let folder_name = folder
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| folder.to_str().unwrap_or("Workspace"))
        .to_string();

    window.set_folder_name(folder_name.into());
    window.set_has_folder(true);

    {
        settings.borrow_mut().last_folder = Some(folder.to_string_lossy().into_owned());
    }
    let settings_snapshot = settings.borrow().clone();
    save_settings(&settings_snapshot);

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
    refresh_audio(window, audio_folder, audio_model, audio_load_state, audio_view_folder);
}

fn refresh_audio(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    folder: PathBuf,
) {
    let filter = window.get_audio_filter().to_string().to_ascii_lowercase();
    let mut rows = Vec::new();
    for entry in file_system::read_dir_sorted(&folder) {
        if entry.kind != file_system::FileKind::Audio
            || !entry.name.to_ascii_lowercase().contains(&filter)
        {
            continue;
        }
        let modified_date = fs::metadata(&entry.path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_secs())
            .unwrap_or_default();
        let path_string = entry.path.to_string_lossy().into_owned();
        rows.push(AudioRow {
            path: path_string.into(),
            name: entry.name.into(),
            modified_date: modified_date.to_string().into(),
            peaks: ModelRc::new(VecModel::from(vec![0.0; waveform::DISPLAY_PEAK_COUNT])),
        });
    }
    rows.sort_by(|left, right| right.modified_date.cmp(&left.modified_date));
    let paths: Vec<PathBuf> = rows.iter().map(|row| PathBuf::from(row.path.as_str())).collect();
    let total = paths.len();
    *audio_folder.borrow_mut() = Some(folder);
    let model = Rc::new(VecModel::from(rows));
    window.set_audio_rows(ModelRc::new(model.clone()));
    *audio_model.borrow_mut() = Some(model);
    {
        let mut state = audio_load_state.lock().unwrap();
        state.folder = audio_folder.borrow().clone().unwrap_or_default();
        state.paths = paths;
        state.generated.clear();
        state.generation += 1;
        state.completed = 0;
        state.total = total;
        state.requested_range = None;
    }
    request_audio_generation(audio_load_state, 0);
}

fn request_audio_generation(
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    start_index: usize,
) {
    let mut state = audio_load_state.lock().unwrap();
    let start = start_index.saturating_sub(AUDIO_PREFETCH_BEFORE);
    let end = start_index
        .saturating_add(AUDIO_PREFETCH_ROWS)
        .min(state.paths.len());
    state.requested_range = Some((start, end));
    if state.running {
        return;
    }
    state.running = true;
    let shared_state = Arc::clone(audio_load_state);
    thread::spawn(move || generate_audio_ranges(shared_state));
}

fn generate_audio_ranges(audio_load_state: Arc<Mutex<AudioLoadState>>) {
    loop {
        let (folder, paths, range, generation) = {
            let mut state = audio_load_state.lock().unwrap();
            let Some(range) = state.requested_range.take() else {
                state.running = false;
                return;
            };
            (state.folder.clone(), state.paths.clone(), range, state.generation)
        };
        let end = range.1.min(paths.len());
        for index in range.0.min(end)..end {
            let path = paths[index].clone();
            {
                let state = audio_load_state.lock().unwrap();
                if state.generation != generation || state.generated.contains(&path) {
                    continue;
                }
            }
            let (cache_key, peaks) = waveform::load_or_generate(&path, &folder);
            let mut metadata_index = metadata::load_index(&folder);
            let path_string = path.to_string_lossy().into_owned();
            if let Some(stored) = metadata_index.audio_files.iter_mut().find(|item| item.file_path == path_string) {
                stored.waveform_cache_key = cache_key;
            } else {
                metadata_index.audio_files.push(metadata::AudioFileMetadata {
                    file_path: path_string.clone(),
                    waveform_cache_key: cache_key,
                    ..Default::default()
                });
            }
            metadata::save_index(&folder, &metadata_index);
            {
                let mut state = audio_load_state.lock().unwrap();
                if state.generation != generation {
                    continue;
                }
                state.generated.insert(path);
                state.completed += 1;
            }
            let state = audio_load_state.lock().unwrap();
            let _ = state.result_sender.send(AudioResult {
                generation,
                index,
                path: path_string,
                peaks,
            });
        }
    }
}

fn refresh_tree(window: &MainWindow, tree_state: &Rc<RefCell<Option<TreeState>>>) {
    let state_ref = tree_state.borrow();
    let Some(state) = state_ref.as_ref() else {
        return;
    };

    let rows: Vec<TreeRow> = file_system::build_visible_rows(state)
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


fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|directory| directory.join("adb-studio").join("settings.json"))
}

fn load_settings() -> AppSettings {
    let Some(path) = settings_path() else {
        return AppSettings::default();
    };

    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &AppSettings) {
    let Some(path) = settings_path() else {
        return;
    };
    let Some(directory) = path.parent() else {
        return;
    };

    if fs::create_dir_all(directory).is_err() {
        return;
    }

    let Ok(contents) = serde_json::to_string_pretty(settings) else {
        return;
    };

    let _ = fs::write(path, contents);
}
