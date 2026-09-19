use std::{
    cell::RefCell,
    collections::{HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{mpsc, Arc, Mutex, OnceLock},
    thread,
};

use slint::{Image, Model, ModelRc, VecModel};

use crate::{
    audio::{
        loader,
        view::{
            available_tag_rows, comment_rows_with_labels, label_row_fields, tag_rows,
            user_comment_subtitle,
        },
    },
    metadata::{self, AudioFileMetadata},
    settings,
    workspace::{file_system, pinned_track_sort, preferences},
    AudioLoadState, AudioRow, MainWindow, TrackDifference,
};

use super::tree_nav::set_audio_breadcrumbs;

struct RefreshQueue {
    next_generation: std::sync::atomic::AtomicU64,
    sender: mpsc::Sender<RefreshResult>,
    receiver: Mutex<mpsc::Receiver<RefreshResult>>,
}

static QUEUE: OnceLock<RefreshQueue> = OnceLock::new();

fn queue() -> &'static RefreshQueue {
    QUEUE.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        RefreshQueue {
            next_generation: std::sync::atomic::AtomicU64::new(0),
            sender,
            receiver: Mutex::new(receiver),
        }
    })
}

struct RefreshResult {
    generation: u64,
    folder: PathBuf,
    workspace: PathBuf,
    expanded: HashSet<PathBuf>,
    cut_paths: HashSet<PathBuf>,
    pinned_path: Option<PathBuf>,
    reload_all: bool,
    changed_audio_paths: HashSet<PathBuf>,
    entries: Vec<PreparedEntry>,
}

struct PreparedEntry {
    path: PathBuf,
    name: String,
    depth: i32,
    is_directory: bool,
    is_lora: bool,
    modified_date: String,
    metadata: Option<AudioFileMetadata>,
    differences: Vec<metadata::comfyui::TrackDifference>,
}

pub fn enqueue(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    _audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    _audio_load_state: &Arc<Mutex<AudioLoadState>>,
    folder: PathBuf,
    changed_paths: Option<&[PathBuf]>,
) {
    if let Some(workspace) = audio_folder.borrow().clone() {
        set_audio_breadcrumbs(window, &workspace, &folder);
    }
    let changed_audio_paths = changed_paths
        .into_iter()
        .flat_map(|paths| paths.iter())
        .filter(|path| path.starts_with(&folder))
        .filter(|path| {
            matches!(
                file_system::FileKind::from_path(path),
                file_system::FileKind::Audio | file_system::FileKind::Safetensors
            )
        })
        .cloned()
        .collect::<HashSet<_>>();
    let expanded = window
        .get_tree_rows()
        .iter()
        .filter(|row| row.is_dir && row.is_expanded)
        .map(|row| PathBuf::from(row.path.as_str()))
        .collect::<HashSet<_>>();
    let cut_paths = window
        .get_cut_paths()
        .iter()
        .map(|path| PathBuf::from(path.as_str()))
        .collect::<HashSet<_>>();
    let workspace = audio_folder.borrow().clone().unwrap_or_default();
    let pinned_path = audio_folder
        .borrow()
        .clone()
        .and_then(|workspace| preferences::pinned_track(&workspace, &folder));
    let filter = window.get_audio_filter().to_string();
    let label_filter = window.get_audio_label_filter();
    let tag_filters = window.get_audio_tag_filters().iter().collect::<Vec<_>>();
    let sort_order = file_system::SortOrder::from_i32(window.get_sort_order());
    let generation = queue()
        .next_generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    let sender = queue().sender.clone();
    let reload_all = changed_paths.is_none();
    thread::spawn(move || {
        let entries = prepare_entries(
            &workspace,
            &folder,
            &expanded,
            sort_order,
            pinned_path.as_deref(),
            &filter,
            label_filter,
            &tag_filters,
        );
        let _ = sender.send(RefreshResult {
            generation,
            folder,
            workspace,
            expanded,
            cut_paths,
            pinned_path,
            reload_all,
            changed_audio_paths,
            entries,
        });
    });
}

fn prepare_entries(
    workspace: &Path,
    folder: &Path,
    expanded: &HashSet<PathBuf>,
    sort_order: file_system::SortOrder,
    pinned_path: Option<&Path>,
    filter: &str,
    label_filter: i32,
    tag_filters: &[i32],
) -> Vec<PreparedEntry> {
    let mut folders = VecDeque::from([(folder.to_path_buf(), 1)]);
    let mut entries = Vec::new();
    while let Some((current, depth)) = folders.pop_front() {
        let mut directory_entries = file_system::read_dir_sorted(&current, sort_order);
        pinned_track_sort::sort_tracks(&current, pinned_path, sort_order, &mut directory_entries);
        for entry in directory_entries {
            let is_directory = entry.kind == file_system::FileKind::Directory;
            let is_audio = entry.kind == file_system::FileKind::Audio;
            let is_lora = entry.kind == file_system::FileKind::Safetensors;
            let path = entry.path.clone();
            let name = entry.name;
            let metadata =
                if (is_audio && file_system::matches_audio_filter(&name, filter)) || is_lora {
                    let audio_metadata = metadata::load_audio_metadata(workspace, &path);
                    ((!is_audio
                        || label_filter < 0
                        || audio_metadata.label_id == Some(label_filter as u64))
                        && tag_filters.iter().all(|tag_id| {
                            !is_audio || audio_metadata.tag_ids.contains(&(*tag_id as u64))
                        }))
                    .then_some(audio_metadata)
                } else {
                    None
                };
            entries.push(PreparedEntry {
                modified_date: fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|time| time.as_secs().to_string())
                    .unwrap_or_default(),
                path: path.clone(),
                name,
                depth,
                is_directory,
                is_lora,
                metadata,
                differences: Vec::new(),
            });
            if is_directory && expanded.contains(&path) {
                folders.push_back((path, depth + 1));
            }
        }
    }
    for entry in &mut entries {
        if let (Some(pinned), Some(_)) = (pinned_path, entry.metadata.as_ref()) {
            entry.differences = metadata::comfyui::compare_files(workspace, pinned, &entry.path);
        }
    }
    entries
}

pub fn tick(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
) {
    let latest = {
        let receiver = queue().receiver.lock().unwrap();
        receiver.try_iter().max_by_key(|result| result.generation)
    };
    let Some(result) = latest else { return };
    let latest_requested = queue()
        .next_generation
        .load(std::sync::atomic::Ordering::Relaxed);
    if result.generation != latest_requested {
        return;
    }
    if audio_folder.borrow().as_ref() != Some(&result.workspace) {
        return;
    }
    apply(window, audio_folder, audio_model, audio_load_state, result);
}

fn apply(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    result: RefreshResult,
) {
    let existing_rows = audio_model
        .borrow()
        .clone()
        .map(|model| {
            (0..model.row_count())
                .filter_map(|index| model.row_data(index))
                .map(|row| (PathBuf::from(row.path.as_str()), row))
                .collect::<std::collections::HashMap<_, _>>()
        })
        .unwrap_or_default();
    let generated_paths = audio_load_state.lock().unwrap().generated.clone();
    let folder_name = result
        .folder
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Workspace");
    let mut rows = vec![folder_row(
        &result.folder,
        folder_name,
        &result.expanded,
        &result.cut_paths,
    )];
    let mut preserved_waveform_paths = HashSet::new();
    let definitions = settings::load();
    for entry in result.entries {
        if entry.is_directory {
            let mut row = folder_row(
                &entry.path,
                &entry.name,
                &result.expanded,
                &result.cut_paths,
            );
            row.depth = entry.depth;
            rows.push(row);
            continue;
        }
        let Some(audio_metadata) = entry.metadata else {
            continue;
        };
        let existing_row = existing_rows.get(&entry.path);
        let can_reuse_waveform = !result.changed_audio_paths.contains(&entry.path)
            && existing_row.is_some_and(|row| {
                generated_paths.contains(&entry.path) && row.modified_date == entry.modified_date
            });
        let waveform = if can_reuse_waveform {
            preserved_waveform_paths.insert(entry.path.clone());
            existing_row
                .map(|row| row.waveform.clone())
                .unwrap_or_default()
        } else {
            Image::default()
        };
        let (label_id, label_name, label_color, label_known) =
            label_row_fields(audio_metadata.label_id, &definitions.label_definitions);
        let similarity = result
            .pinned_path
            .as_ref()
            .map(|_| pinned_track_sort::similarity_from_differences(&entry.differences))
            .unwrap_or(-1.0);
        let differences = entry.differences;
        rows.push(AudioRow {
            path: entry.path.to_string_lossy().into_owned().into(),
            name: entry.name.into(),
            subtitle: user_comment_subtitle(&audio_metadata.user_comments).into(),
            custom_tag: audio_metadata.custom_tag.clone().into(),
            is_folder: false,
            is_lora: entry.is_lora,
            depth: entry.depth,
            is_expanded: false,
            modified_date: entry.modified_date.into(),
            waveform,
            is_loading: false,
            comments: comment_rows_with_labels(&audio_metadata, &definitions.label_definitions),
            differences: ModelRc::new(VecModel::from(
                differences
                    .into_iter()
                    .map(|difference| TrackDifference {
                        label: difference.label.into(),
                        value: difference.value.into(),
                    })
                    .collect::<Vec<_>>(),
            )),
            similarity,
            rating: audio_metadata.normalized_rating() as i32,
            is_pinned: result.pinned_path.as_deref() == Some(entry.path.as_path()),
            is_active: existing_row.is_some_and(|row| row.is_active),
            is_selected: existing_row.is_some_and(|row| row.is_selected),
            is_primary: existing_row.is_some_and(|row| row.is_primary),
            is_playing: existing_row.is_some_and(|row| row.is_playing),
            progress: existing_row.map(|row| row.progress).unwrap_or_else(|| {
                (audio_metadata.duration_seconds > 0.0)
                    .then_some(
                        audio_metadata.last_position_seconds / audio_metadata.duration_seconds,
                    )
                    .map(|value| value.clamp(0.0, 1.0))
                    .unwrap_or(0.0)
            }),
            duration_seconds: audio_metadata.duration_seconds,
            loop_enabled: existing_row.is_some_and(|row| row.loop_enabled),
            selected_comment_start: existing_row
                .map(|row| row.selected_comment_start)
                .unwrap_or(-1.0),
            selected_comment_end: existing_row
                .map(|row| row.selected_comment_end)
                .unwrap_or(-1.0),
            label_id,
            label_name,
            label_color,
            label_known,
            tags: tag_rows(&audio_metadata.tag_ids, &definitions.tag_definitions),
            available_tags: available_tag_rows(
                &audio_metadata.tag_ids,
                &definitions.tag_definitions,
            ),
            detected_bpm: existing_row.map(|row| row.detected_bpm).unwrap_or(0.0),
            is_cut: result.cut_paths.contains(&entry.path),
        });
    }
    let previous_selected_path = PathBuf::from(window.get_selected_audio_path().as_str());
    let selected_paths = window
        .get_tree_rows()
        .iter()
        .filter(|row| row.is_selected)
        .map(|row| PathBuf::from(row.path.as_str()))
        .collect::<HashSet<_>>();
    let selected_path = rows
        .iter()
        .find(|row| !row.is_folder && Path::new(row.path.as_str()) == previous_selected_path)
        .map(|row| row.path.clone())
        .or_else(|| {
            rows.iter()
                .find(|row| !row.is_folder)
                .map(|row| row.path.clone())
        })
        .unwrap_or_default();
    for row in &mut rows {
        row.is_selected =
            selected_paths.contains(Path::new(row.path.as_str())) || row.path == selected_path;
        row.is_primary = row.path == selected_path;
    }
    window.set_selected_audio_path(selected_path);
    let paths = rows
        .iter()
        .filter(|row| !row.is_folder && !row.is_lora)
        .map(|row| PathBuf::from(row.path.as_str()))
        .collect::<Vec<_>>();
    let total = paths
        .iter()
        .filter(|path| file_system::FileKind::from_path(path) == file_system::FileKind::Audio)
        .count();
    let model = Rc::new(VecModel::from(rows));
    window.set_audio_rows(ModelRc::new(model.clone()));
    crate::audio::view::set_audio_row_index(&model);
    *audio_model.borrow_mut() = Some(model);
    if !previous_selected_path.as_os_str().is_empty() {
        crate::audio::view::scroll_to_path(window, audio_model, &previous_selected_path);
    }
    let mut state = audio_load_state.lock().unwrap();
    state.folder = audio_folder.borrow().clone().unwrap_or_default();
    state.paths = paths;
    if result.reload_all {
        state
            .generated
            .retain(|path| preserved_waveform_paths.contains(path));
        state.loading.clear();
    } else {
        let current_paths = state.paths.iter().cloned().collect::<HashSet<_>>();
        state.generated.retain(|path| {
            current_paths.contains(path) && !result.changed_audio_paths.contains(path)
        });
        state.loading.retain(|path| {
            current_paths.contains(path) && !result.changed_audio_paths.contains(path)
        });
    }
    state.generation += 1;
    state
        .cancellation_generation
        .store(state.generation, std::sync::atomic::Ordering::Release);
    state.completed = state.generated.len();
    state.total = total;
    state.requested_range = None;
    drop(state);
}

fn folder_row(
    path: &Path,
    name: &str,
    expanded: &HashSet<PathBuf>,
    cut_paths: &HashSet<PathBuf>,
) -> AudioRow {
    AudioRow {
        path: path.to_string_lossy().into_owned().into(),
        name: name.into(),
        subtitle: "".into(),
        custom_tag: "".into(),
        is_folder: true,
        is_lora: false,
        depth: 0,
        is_expanded: expanded.contains(path),
        modified_date: "".into(),
        waveform: Image::default(),
        is_loading: false,
        comments: ModelRc::new(VecModel::from(Vec::new())),
        differences: ModelRc::new(VecModel::from(Vec::new())),
        similarity: -1.0,
        rating: 0,
        is_pinned: false,
        is_active: false,
        is_selected: false,
        is_primary: false,
        is_playing: false,
        progress: 0.0,
        duration_seconds: 0.0,
        loop_enabled: false,
        selected_comment_start: -1.0,
        selected_comment_end: -1.0,
        label_id: -1,
        label_name: "".into(),
        label_color: slint::Color::from_argb_u8(0, 0, 0, 0),
        label_known: false,
        tags: ModelRc::new(VecModel::from(Vec::new())),
        available_tags: ModelRc::new(VecModel::from(Vec::new())),
        detected_bpm: 0.0,
        is_cut: cut_paths.contains(path),
    }
}
