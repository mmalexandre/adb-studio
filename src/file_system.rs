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
}

impl TreeState {
    pub fn new(root: PathBuf) -> Self {
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        Self {
            root,
            expanded,
            selected: None,
        }
    }

    pub fn toggle(&mut self, path: &Path) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
    }

    pub fn select(&mut self, path: &Path) {
        self.selected = Some(path.to_path_buf());
    }

    pub fn select_and_expand(&mut self, path: &Path) {
        self.select(path);
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
    push_children(&state.root, 0, state, &mut rows);
    rows
}

fn push_children(dir: &Path, depth: i32, state: &TreeState, rows: &mut Vec<VisibleRow>) {
    for entry in read_dir_sorted(dir) {
        let is_expanded = entry.is_dir && state.expanded.contains(&entry.path);
        let is_selected = state.selected.as_deref() == Some(entry.path.as_path());
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
