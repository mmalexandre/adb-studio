use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Directory,
    Audio,
    Safetensors,
    Json,
    Other,
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
            Some("flac" | "mp3" | "opus" | "wav") => FileKind::Audio,
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

/// Reads one directory level; returns dirs first, then files, both name-sorted.
/// Unreadable directories yield an empty list rather than an error.
pub fn read_dir_sorted(path: &Path) -> Vec<DirEntryInfo> {
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
        .filter(|entry| {
            !entry.name.starts_with('.') && entry.name != "/" && entry.name != "\\"
        })
        .collect();

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });

    entries
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

    pub fn select_with_shift(&mut self, path: &Path, shift: bool) {
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

        let siblings = read_dir_sorted(parent);
        let Some(anchor_index) = siblings.iter().position(|entry| entry.path == *anchor) else {
            self.select(path);
            return;
        };
        let Some(path_index) = siblings.iter().position(|entry| entry.path == path) else {
            self.select(path);
            return;
        };
        let (start, end) = if anchor_index <= path_index {
            (anchor_index, path_index)
        } else {
            (path_index, anchor_index)
        };
        self.selected_paths = siblings[start..=end]
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
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

pub fn build_visible_rows(state: &TreeState) -> Vec<VisibleRow> {
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
        push_children(&state.root, 1, state, &mut rows);
    }
    rows
}

fn push_children(dir: &Path, depth: i32, state: &TreeState, rows: &mut Vec<VisibleRow>) {
    for entry in read_dir_sorted(dir) {
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
            push_children(&entry.path, depth + 1, state, rows);
        }
    }
}