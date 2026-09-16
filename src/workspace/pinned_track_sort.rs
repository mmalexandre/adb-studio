use std::{cmp::Ordering, path::Path};

use crate::metadata::{self, comfyui::ComfyUIWorkflow};

use super::file_system::{DirEntryInfo, SortOrder};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Distance {
    tier: u8,
    values: Vec<i64>,
}

impl Ord for Distance {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tier
            .cmp(&other.tier)
            .then_with(|| self.values.cmp(&other.values))
    }
}

impl PartialOrd for Distance {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone)]
struct RankedEntry {
    path: std::path::PathBuf,
    name: String,
    distance: Distance,
}

pub fn sort_tracks(
    folder: &Path,
    pinned_path: Option<&Path>,
    sort_order: SortOrder,
    entries: &mut [DirEntryInfo],
) {
    let Some(pinned_path) = pinned_path else {
        entries.sort_by(|left, right| compare_existing(left, right, sort_order));
        return;
    };

    let pinned_workflow = workflow_for(folder, pinned_path);
    let mut ranked = entries
        .iter()
        .map(|entry| {
            let distance = if entry.path == pinned_path {
                Distance {
                    tier: 0,
                    values: Vec::new(),
                }
            } else if pinned_workflow.is_none() {
                let workflow = workflow_for(folder, &entry.path);
                Distance {
                    tier: if workflow.is_some() { 1 } else { 2 },
                    values: Vec::new(),
                }
            } else if pinned_workflow.is_some() {
                Distance {
                    tier: comparison_tier(&crate::metadata::comfyui::compare_files(
                        folder,
                        pinned_path,
                        &entry.path,
                    )),
                    values: Vec::new(),
                }
            } else {
                Distance {
                    tier: 4,
                    values: Vec::new(),
                }
            };
            RankedEntry {
                path: entry.path.clone(),
                name: entry.name.clone(),
                distance,
            }
        })
        .collect::<Vec<_>>();

    ranked.sort_by(|left, right| {
        left.distance
            .cmp(&right.distance)
            .then_with(|| compare_names(&left.name, &right.name))
    });
    let order = ranked
        .into_iter()
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| order.iter().position(|path| path == &entry.path));
}

fn comparison_tier(differences: &[crate::metadata::comfyui::TrackDifference]) -> u8 {
    if differences.is_empty() {
        return 0;
    }
    if differences
        .iter()
        .any(|difference| difference.value == "missing workflow file")
    {
        return 4;
    }
    if differences
        .iter()
        .any(|difference| difference.label.starts_with("Lora") || difference.label == "Loras: ")
    {
        return 1;
    }
    if differences
        .iter()
        .any(|difference| matches!(difference.label.as_str(), "BPM: " | "Seed: " | "Key: "))
    {
        return 2;
    }
    3
}

pub fn similarity_from_differences(
    differences: &[crate::metadata::comfyui::TrackDifference],
) -> f32 {
    if differences.is_empty() {
        return 1.0;
    }
    if differences
        .iter()
        .any(|difference| difference.value == "missing workflow file")
    {
        return 0.0;
    }
    if differences
        .iter()
        .any(|difference| difference.label.starts_with("Lora") || difference.label == "Loras: ")
    {
        return 0.7;
    }
    if differences
        .iter()
        .any(|difference| matches!(difference.label.as_str(), "BPM: " | "Seed: " | "Key: "))
    {
        return 0.4;
    }
    0.1
}

fn compare_existing(left: &DirEntryInfo, right: &DirEntryInfo, sort_order: SortOrder) -> Ordering {
    right.is_dir.cmp(&left.is_dir).then_with(|| {
        if left.is_dir {
            return compare_names(&left.name, &right.name);
        }
        match sort_order {
            SortOrder::AlphabeticalAscending => compare_names(&left.name, &right.name),
            SortOrder::AlphabeticalDescending => compare_names(&right.name, &left.name),
            SortOrder::ModifiedAscending => compare_modified(left, right),
            SortOrder::ModifiedDescending => compare_modified(right, left),
        }
    })
}

fn compare_names(left: &str, right: &str) -> Ordering {
    left.to_ascii_lowercase()
        .cmp(&right.to_ascii_lowercase())
        .then_with(|| left.cmp(right))
}

fn compare_modified(left: &DirEntryInfo, right: &DirEntryInfo) -> Ordering {
    std::fs::metadata(&left.path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .cmp(
            &std::fs::metadata(&right.path)
                .and_then(|metadata| metadata.modified())
                .ok(),
        )
        .then_with(|| compare_names(&left.name, &right.name))
}

fn workflow_for(folder: &Path, track_path: &Path) -> Option<ComfyUIWorkflow> {
    let workflow_path = metadata::workflow_path(folder, track_path)?;
    if !workflow_path.is_file() {
        return None;
    }
    crate::metadata::comfyui::parse_file_cached(folder, &workflow_path).ok()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{similarity_from_differences, sort_tracks};
    use crate::metadata::comfyui::TrackDifference;
    use crate::workspace::file_system::{DirEntryInfo, FileKind, SortOrder};

    #[test]
    fn bpm_difference_gets_generation_similarity() {
        let differences = vec![TrackDifference {
            label: "BPM: ".into(),
            value: "136 (-16)".into(),
        }];

        assert_eq!(similarity_from_differences(&differences), 0.4);
    }

    #[test]
    fn known_workflows_precede_missing_workflows_without_pinned_metadata() {
        let folder =
            std::env::temp_dir().join(format!("adb-studio-pinned-sort-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(folder.join(".adbstudio/workflows")).unwrap();
        let pinned = folder.join("pinned.wav");
        let known = folder.join("known.wav");
        let missing = folder.join("missing.wav");
        fs::write(
            crate::metadata::workflow_path(&folder, &known).unwrap(),
            "{}",
        )
        .unwrap();
        let mut entries = vec![entry(&missing), entry(&known), entry(&pinned)];

        sort_tracks(
            &folder,
            Some(&pinned),
            SortOrder::AlphabeticalAscending,
            &mut entries,
        );

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>(),
            vec![pinned, known, missing]
        );
        let _ = fs::remove_dir_all(folder);
    }

    fn entry(path: &PathBuf) -> DirEntryInfo {
        DirEntryInfo {
            path: path.clone(),
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            is_dir: false,
            kind: FileKind::Audio,
        }
    }
}
