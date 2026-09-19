use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{atomic::Ordering, Arc, Mutex},
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
    metadata, settings,
    workspace::{file_system, preferences},
    AudioLoadState, AudioRow, MainWindow, TrackDifference,
};

use super::library_refresh;
use super::{pinned_track_sort, tree_nav::set_audio_breadcrumbs};

fn collect_visible_entries(
    folder: &Path,
    depth: i32,
    expanded: &HashSet<PathBuf>,
    sort_order: file_system::SortOrder,
    pinned_path: Option<&Path>,
    entries: &mut Vec<(file_system::DirEntryInfo, i32)>,
) {
    let mut directory_entries = file_system::read_dir_sorted(folder, sort_order);
    pinned_track_sort::sort_tracks(folder, pinned_path, sort_order, &mut directory_entries);
    for entry in directory_entries {
        let is_directory = entry.kind == file_system::FileKind::Directory;
        let child_path = entry.path.clone();
        entries.push((entry, depth));
        if is_directory && expanded.contains(&child_path) {
            collect_visible_entries(&child_path, depth + 1, expanded, sort_order, None, entries);
        }
    }
}

#[cfg(test)]
mod visible_entry_tests {
    use std::{
        collections::HashSet,
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::collect_visible_entries;
    use crate::workspace::file_system::SortOrder;

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("adb-studio-library-{suffix}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn expanded_folder_includes_audio_files_from_its_own_root() {
        let temp = TempDirectory::new();
        let folder = temp.0.join("folder");
        fs::create_dir(&folder).unwrap();
        let song = folder.join("song.wav");
        fs::write(&song, []).unwrap();

        let mut expanded = HashSet::new();
        expanded.insert(folder.clone());
        let mut entries = Vec::new();
        collect_visible_entries(
            &temp.0,
            1,
            &expanded,
            SortOrder::AlphabeticalAscending,
            None,
            &mut entries,
        );

        assert!(entries.iter().any(|(entry, _)| entry.path == folder));
        assert!(entries.iter().any(|(entry, _)| entry.path == song));
        assert!(Path::new(&entries[1].0.path).ends_with("song.wav"));
    }
}

pub fn refresh_audio(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    folder: PathBuf,
) {
    library_refresh::enqueue(
        window,
        audio_folder,
        audio_model,
        audio_load_state,
        folder,
        None,
    );
}

pub fn refresh_audio_for_changes(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    folder: PathBuf,
    changed_paths: &[PathBuf],
) {
    library_refresh::enqueue(
        window,
        audio_folder,
        audio_model,
        audio_load_state,
        folder,
        Some(changed_paths),
    );
}

#[allow(dead_code)]
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
        .filter(|path| path.starts_with(&folder))
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
    let generated_paths = audio_load_state.lock().unwrap().generated.clone();
    let pinned_path = audio_folder
        .borrow()
        .clone()
        .and_then(|workspace| preferences::pinned_track(&workspace, &folder));
    let workspace = audio_folder.borrow().clone().unwrap_or_default();
    let definition_settings = settings::load();
    let sort_order = file_system::SortOrder::from_i32(window.get_sort_order());
    let cut_paths = window.get_cut_paths();
    let expanded = window
        .get_tree_rows()
        .iter()
        .filter(|row| row.is_dir && row.is_expanded)
        .map(|row| PathBuf::from(row.path.as_str()))
        .collect::<HashSet<_>>();
    let mut entries = Vec::new();
    collect_visible_entries(
        &folder,
        1,
        &expanded,
        sort_order,
        pinned_path.as_deref(),
        &mut entries,
    );
    let folder_name = folder
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| folder.to_str().unwrap_or("Workspace"));
    let mut rows = Vec::new();
    let mut preserved_waveform_paths = HashSet::new();
    rows.push(AudioRow {
        path: folder.to_string_lossy().into_owned().into(),
        name: folder_name.into(),
        subtitle: "".into(),
        custom_tag: "".into(),
        is_folder: true,
        is_lora: false,
        depth: 0,
        is_expanded: expanded.contains(&folder),
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
        is_cut: cut_paths
            .iter()
            .any(|cut_path| cut_path.as_str() == folder.to_string_lossy()),
    });
    for (entry, depth) in entries {
        if entry.kind == file_system::FileKind::Directory {
            rows.push(AudioRow {
                path: entry.path.to_string_lossy().into_owned().into(),
                name: entry.name.into(),
                subtitle: "".into(),
                custom_tag: "".into(),
                is_folder: true,
                is_lora: false,
                depth,
                is_expanded: expanded.contains(&entry.path),
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
                is_cut: cut_paths
                    .iter()
                    .any(|cut_path| cut_path.as_str() == entry.path.to_string_lossy()),
            });
            continue;
        }
        if entry.kind != file_system::FileKind::Audio
            || !file_system::matches_audio_filter(&entry.name, &filter)
        {
            continue;
        }
        let modified_date = fs::metadata(&entry.path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_secs())
            .unwrap_or_default();
        let existing_row = existing_rows.get(&entry.path);
        if !changed_audio_paths.contains(&entry.path)
            && existing_row.is_some_and(|row| row.modified_date == modified_date.to_string())
        {
            let mut row = existing_row.unwrap().clone();
            let audio_metadata = metadata::load_audio_metadata(&workspace, &entry.path);
            let (label_id, label_name, label_color, label_known) = label_row_fields(
                audio_metadata.label_id,
                &definition_settings.label_definitions,
            );
            row.name = entry.name.clone().into();
            row.subtitle = user_comment_subtitle(&audio_metadata.user_comments).into();
            row.comments =
                comment_rows_with_labels(&audio_metadata, &definition_settings.label_definitions);
            row.label_id = label_id;
            row.label_name = label_name;
            row.label_color = label_color;
            row.label_known = label_known;
            row.tags = tag_rows(
                &audio_metadata.tag_ids,
                &definition_settings.tag_definitions,
            );
            row.available_tags = available_tag_rows(
                &audio_metadata.tag_ids,
                &definition_settings.tag_definitions,
            );
            row.depth = depth;
            row.is_pinned = pinned_path.as_deref() == Some(entry.path.as_path());
            row.is_cut = cut_paths
                .iter()
                .any(|cut_path| cut_path.as_str() == entry.path.to_string_lossy());
            if generated_paths.contains(&entry.path) {
                preserved_waveform_paths.insert(entry.path.clone());
            }
            rows.push(row);
            continue;
        }
        let can_reuse_waveform = existing_row.is_some_and(|row| {
            generated_paths.contains(&entry.path) && row.modified_date == modified_date.to_string()
        });
        let should_reload =
            changed_audio_paths.contains(&entry.path) || (reload_all && !can_reuse_waveform);
        if !should_reload && !reload_all {
            if let Some(row) = existing_rows.get(&entry.path) {
                rows.push(row.clone());
                continue;
            }
        }
        let path_string = entry.path.to_string_lossy().into_owned();
        let audio_metadata = metadata::load_audio_metadata(&workspace, &entry.path);
        let comments =
            comment_rows_with_labels(&audio_metadata, &definition_settings.label_definitions);
        let (label_id, label_name, label_color, label_known) = label_row_fields(
            audio_metadata.label_id,
            &definition_settings.label_definitions,
        );
        let progress = (audio_metadata.duration_seconds > 0.0)
            .then_some(audio_metadata.last_position_seconds / audio_metadata.duration_seconds)
            .map(|value| value.clamp(0.0, 1.0))
            .unwrap_or(0.0);
        let differences = pinned_path
            .as_deref()
            .map(|pinned| metadata::comfyui::compare_files(&workspace, pinned, &entry.path))
            .unwrap_or_default();
        let similarity = pinned_path
            .as_deref()
            .map(|_| pinned_track_sort::similarity_from_differences(&differences))
            .unwrap_or(-1.0);
        rows.push(AudioRow {
            path: path_string.into(),
            name: entry.name.into(),
            subtitle: user_comment_subtitle(&audio_metadata.user_comments).into(),
            custom_tag: audio_metadata.custom_tag.clone().into(),
            is_folder: false,
            is_lora: false,
            depth,
            is_expanded: false,
            modified_date: modified_date.to_string().into(),
            waveform: if can_reuse_waveform {
                preserved_waveform_paths.insert(entry.path.clone());
                existing_row
                    .map(|row| row.waveform.clone())
                    .unwrap_or_default()
            } else {
                Image::default()
            },
            is_loading: false,
            comments,
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
            is_pinned: pinned_path.as_deref() == Some(entry.path.as_path()),
            is_active: false,
            is_selected: false,
            is_primary: false,
            is_playing: false,
            progress,
            duration_seconds: audio_metadata.duration_seconds,
            loop_enabled: false,
            selected_comment_start: -1.0,
            selected_comment_end: -1.0,
            label_id,
            label_name,
            label_color,
            label_known,
            tags: tag_rows(
                &audio_metadata.tag_ids,
                &definition_settings.tag_definitions,
            ),
            available_tags: available_tag_rows(
                &audio_metadata.tag_ids,
                &definition_settings.tag_definitions,
            ),
            detected_bpm: 0.0,
            is_cut: cut_paths
                .iter()
                .any(|cut_path| cut_path.as_str() == entry.path.to_string_lossy()),
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
        .filter(|row| !row.is_folder)
        .find(|row| Path::new(row.path.as_str()) == previous_selected_path)
        .map(|row| row.path.clone())
        .or_else(|| {
            rows.iter()
                .find(|row| !row.is_folder)
                .map(|row| row.path.clone())
        })
        .unwrap_or_default();
    for row in &mut rows {
        let path = Path::new(row.path.as_str());
        row.is_selected = selected_paths.contains(path) || row.path == selected_path;
        row.is_primary = row.path == selected_path;
    }
    window.set_selected_audio_path(selected_path);
    let paths: Vec<PathBuf> = rows
        .iter()
        .map(|row| PathBuf::from(row.path.as_str()))
        .collect();
    let total = paths
        .iter()
        .filter(|path| file_system::FileKind::from_path(path) == file_system::FileKind::Audio)
        .count();
    let model = Rc::new(VecModel::from(rows));
    window.set_audio_rows(ModelRc::new(model.clone()));
    crate::audio::view::set_audio_row_index(&model);
    *audio_model.borrow_mut() = Some(model);
    {
        let mut state = audio_load_state.lock().unwrap();
        state.folder = audio_folder.borrow().clone().unwrap_or_default();
        state.paths = paths;
        if reload_all {
            state
                .generated
                .retain(|path| preserved_waveform_paths.contains(path));
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
}
