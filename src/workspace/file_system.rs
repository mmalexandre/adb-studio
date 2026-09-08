use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Directory,
    Audio,
    Safetensors,
    Json,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    AlphabeticalAscending,
    AlphabeticalDescending,
    ModifiedAscending,
    ModifiedDescending,
}

impl SortOrder {
    pub fn from_i32(value: i32) -> Self {
        match value {
            0 => Self::AlphabeticalAscending,
            1 => Self::AlphabeticalDescending,
            2 => Self::ModifiedAscending,
            _ => Self::ModifiedDescending,
        }
    }
}

impl FileKind {
    pub fn from_path(path: &Path) -> Self {
        if path.is_dir() {
            return FileKind::Directory;
        }
        match path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref()
        {
            Some("flac" | "mp3" | "ogg" | "opus" | "wav") => FileKind::Audio,
            Some("safetensors") => FileKind::Safetensors,
            Some("json") => FileKind::Json,
            _ => FileKind::Other,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FileKind::Directory => "dir",
            FileKind::Audio => "audio",
            FileKind::Safetensors => "safetensors",
            FileKind::Json => "json",
            FileKind::Other => "other",
        }
    }
}

pub struct DirEntryInfo {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub kind: FileKind,
}

/// Reads one directory level; returns alphabetically sorted dirs first, then sorted files.
/// Unreadable directories yield an empty list rather than an error.
pub fn read_dir_sorted(path: &Path, sort_order: SortOrder) -> Vec<DirEntryInfo> {
    let Ok(read_dir) = fs::read_dir(path) else {
        return Vec::new();
    };

    let mut entries: Vec<DirEntryInfo> = read_dir
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = FileKind::from_path(&path);
            DirEntryInfo {
                is_dir: kind == FileKind::Directory,
                path,
                name,
                kind,
            }
        })
        .filter(|entry| !entry.name.starts_with('.') && entry.name != "/" && entry.name != "\\")
        .collect();

    entries.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            if a.is_dir {
                return compare_names(a, b);
            }
            match sort_order {
                SortOrder::AlphabeticalAscending => compare_names(a, b),
                SortOrder::AlphabeticalDescending => compare_names(b, a),
                SortOrder::ModifiedAscending => compare_modified(a, b),
                SortOrder::ModifiedDescending => compare_modified(b, a),
            }
        })
    });

    entries
}

fn compare_names(left: &DirEntryInfo, right: &DirEntryInfo) -> std::cmp::Ordering {
    left.name
        .to_ascii_lowercase()
        .cmp(&right.name.to_ascii_lowercase())
}

fn compare_modified(left: &DirEntryInfo, right: &DirEntryInfo) -> std::cmp::Ordering {
    let left_modified = fs::metadata(&left.path)
        .and_then(|metadata| metadata.modified())
        .ok();
    let right_modified = fs::metadata(&right.path)
        .and_then(|metadata| metadata.modified())
        .ok();
    left_modified
        .cmp(&right_modified)
        .then_with(|| compare_names(left, right))
}

pub struct TreeState {
    pub root: PathBuf,
    expanded: HashSet<PathBuf>,
    pub selected: Option<PathBuf>,
    selected_paths: HashSet<PathBuf>,
    selection_anchor: Option<PathBuf>,
}

impl TreeState {
    pub fn new(root: PathBuf) -> Self {
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        Self {
            root,
            expanded,
            selected: None,
            selected_paths: HashSet::new(),
            selection_anchor: None,
        }
    }

    pub fn toggle(&mut self, path: &Path) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
    }

    pub fn select(&mut self, path: &Path) {
        self.selected = Some(path.to_path_buf());
        self.selected_paths.clear();
        self.selected_paths.insert(path.to_path_buf());
        self.selection_anchor = Some(path.to_path_buf());
    }

    pub fn select_with_shift(&mut self, path: &Path, shift: bool, sort_order: SortOrder) {
        if !shift {
            self.select(path);
            return;
        }

        let Some(parent) = path.parent() else {
            self.select(path);
            return;
        };
        let siblings = read_dir_sorted(parent, sort_order)
            .into_iter()
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        self.select_with_shift_in_order(path, true, &siblings);
    }

    pub fn select_with_shift_in_order(
        &mut self,
        path: &Path,
        shift: bool,
        ordered_paths: &[PathBuf],
    ) {
        if !shift {
            self.select(path);
            return;
        }

        let Some(anchor) = self.selection_anchor.as_ref() else {
            self.select(path);
            return;
        };
        let Some(parent) = path.parent() else {
            self.select(path);
            return;
        };
        if anchor.parent() != Some(parent) {
            self.select(path);
            return;
        }

        let Some(anchor_index) = ordered_paths.iter().position(|candidate| candidate == anchor) else {
            self.select(path);
            return;
        };
        let Some(path_index) = ordered_paths.iter().position(|candidate| candidate == path) else {
            self.select(path);
            return;
        };
        let (start, end) = if anchor_index <= path_index {
            (anchor_index, path_index)
        } else {
            (path_index, anchor_index)
        };
        self.selected_paths = ordered_paths[start..=end].iter().cloned().collect();
        self.selected = Some(path.to_path_buf());
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.selected_paths.iter().cloned().collect()
    }

    pub fn select_paths(&mut self, paths: Vec<PathBuf>, primary: PathBuf) {
        self.selected_paths = paths.into_iter().collect();
        self.selected = Some(primary);
        self.selection_anchor = self.selected.clone();
    }

    pub fn remove_path(&mut self, path: &Path) -> bool {
        let selection_removed = self
            .selected
            .as_ref()
            .is_some_and(|selected| selected == path || selected.starts_with(path));
        self.selected_paths
            .retain(|selected| selected != path && !selected.starts_with(path));
        self.expanded
            .retain(|expanded| expanded != path && !expanded.starts_with(path));
        if selection_removed {
            self.selected = None;
            self.selection_anchor = None;
        }
        selection_removed
    }

    pub fn select_and_expand(&mut self, path: &Path) {
        self.select(path);
        self.expand_to(path);
    }

    pub fn expand_to(&mut self, path: &Path) {
        let mut ancestor = path.parent();
        while let Some(path) = ancestor {
            self.expanded.insert(path.to_path_buf());
            if path == self.root {
                break;
            }
            ancestor = path.parent();
        }
    }
}

pub struct VisibleRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: i32,
    pub is_dir: bool,
    pub is_expanded: bool,
    pub is_selected: bool,
    pub kind: FileKind,
}

pub fn build_visible_rows(state: &TreeState, sort_order: SortOrder) -> Vec<VisibleRow> {
    let mut rows = Vec::new();
    let is_selected = state.selected_paths.contains(&state.root);
    rows.push(VisibleRow {
        path: state.root.clone(),
        name: state
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| state.root.to_str().unwrap_or("Workspace"))
            .to_string(),
        depth: 0,
        is_dir: true,
        is_expanded: state.expanded.contains(&state.root),
        is_selected,
        kind: FileKind::Directory,
    });
    if state.expanded.contains(&state.root) {
        push_children(&state.root, 1, state, sort_order, &mut rows);
    }
    rows
}

pub fn matches_audio_filter(name: &str, filter: &str) -> bool {
    let filter = filter.trim();
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

fn push_children(
    dir: &Path,
    depth: i32,
    state: &TreeState,
    sort_order: SortOrder,
    rows: &mut Vec<VisibleRow>,
) {
    for entry in read_dir_sorted(dir, sort_order) {
        let is_expanded = entry.is_dir && state.expanded.contains(&entry.path);
        let is_selected = state.selected_paths.contains(&entry.path);
        rows.push(VisibleRow {
            path: entry.path.clone(),
            name: entry.name,
            depth,
            is_dir: entry.is_dir,
            is_expanded,
            is_selected,
            kind: entry.kind,
        });
        if is_expanded {
            push_children(&entry.path, depth + 1, state, sort_order, rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_visible_rows, matches_audio_filter, read_dir_sorted, FileKind, SortOrder, TreeState};
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("adb-studio-file-system-{suffix}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn file_kind_is_case_insensitive_and_directories_win() {
        let temp = TempDirectory::new();
        let directory = temp.path().join("song.WAV");
        fs::create_dir(&directory).unwrap();

        assert_eq!(FileKind::from_path(&directory), FileKind::Directory);
        assert_eq!(FileKind::from_path(Path::new("song.WAV")), FileKind::Audio);
        assert_eq!(FileKind::from_path(Path::new("model.SAFETENSORS")), FileKind::Safetensors);
        assert_eq!(FileKind::from_path(Path::new("notes.JSON")), FileKind::Json);
        assert_eq!(FileKind::from_path(Path::new("notes.txt")), FileKind::Other);
    }

    #[test]
    fn read_dir_sorted_hides_dotfiles_and_keeps_directories_first() {
        let temp = TempDirectory::new();
        fs::create_dir(temp.path().join("Bravo")).unwrap();
        fs::write(temp.path().join("alpha.wav"), []).unwrap();
        fs::write(temp.path().join("charlie.wav"), []).unwrap();
        fs::write(temp.path().join(".hidden.wav"), []).unwrap();

        let ascending = read_dir_sorted(temp.path(), SortOrder::AlphabeticalAscending);
        assert_eq!(
            ascending.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
            vec!["Bravo", "alpha.wav", "charlie.wav"]
        );

        let descending = read_dir_sorted(temp.path(), SortOrder::AlphabeticalDescending);
        assert_eq!(
            descending.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
            vec!["Bravo", "charlie.wav", "alpha.wav"]
        );
    }

    #[test]
    fn shift_selection_selects_the_range_in_directory_order() {
        let temp = TempDirectory::new();
        for name in ["one.wav", "two.wav", "three.wav"] {
            fs::write(temp.path().join(name), []).unwrap();
        }
        let first = temp.path().join("one.wav");
        let last = temp.path().join("three.wav");
        let mut state = TreeState::new(temp.path().to_path_buf());

        state.select(&first);
        state.select_with_shift(&last, true, SortOrder::AlphabeticalAscending);

        let mut selected = state.selected_paths();
        selected.sort();
        assert_eq!(
            selected,
            vec![
                temp.path().join("one.wav"),
                temp.path().join("three.wav"),
            ]
        );
        assert_eq!(state.selected, Some(last));
    }

    #[test]
    fn shift_selection_uses_the_supplied_visible_order() {
        let temp = TempDirectory::new();
        for name in ["one.wav", "two.wav", "three.wav"] {
            fs::write(temp.path().join(name), []).unwrap();
        }
        let first = temp.path().join("one.wav");
        let last = temp.path().join("two.wav");
        let visible_order = vec![
            first.clone(),
            temp.path().join("three.wav"),
            last.clone(),
        ];
        let mut state = TreeState::new(temp.path().to_path_buf());

        state.select(&first);
        state.select_with_shift_in_order(&last, true, &visible_order);

        assert_eq!(state.selected_paths.len(), 3);
        assert!(visible_order
            .iter()
            .all(|path| state.selected_paths.contains(path)));
        assert_eq!(state.selected, Some(last));
    }

    #[test]
    fn visible_rows_follow_expanded_state() {
        let temp = TempDirectory::new();
        let child = temp.path().join("child");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("song.wav"), []).unwrap();
        let state = TreeState::new(temp.path().to_path_buf());

        assert_eq!(build_visible_rows(&state, SortOrder::AlphabeticalAscending).len(), 2);
    }

    #[test]
    fn audio_filter_matches_names_case_insensitively() {
        assert!(matches_audio_filter("My Voice.WAV", " voice "));
        assert!(matches_audio_filter("My Voice.WAV", ""));
        assert!(!matches_audio_filter("My Voice.WAV", "music"));
    }
}
