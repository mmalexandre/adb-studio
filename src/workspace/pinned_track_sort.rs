use std::{cmp::Ordering, path::Path};

use crate::metadata::{
    self,
    comfyui::{ComfyUIWorkflow, LoRAInfo},
};

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
            let workflow = workflow_for(folder, &entry.path);
            let distance = if entry.path == pinned_path {
                Distance {
                    tier: 0,
                    values: Vec::new(),
                }
            } else if pinned_workflow.is_none() {
                Distance {
                    tier: if workflow.is_some() { 1 } else { 2 },
                    values: Vec::new(),
                }
            } else if let (Some(pinned), Some(track)) = (&pinned_workflow, &workflow) {
                distance_between(pinned, track)
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
    crate::metadata::comfyui::parse_file(&workflow_path).ok()
}

fn distance_between(pinned: &ComfyUIWorkflow, track: &ComfyUIWorkflow) -> Distance {
    let lora_distance = lora_distance(&pinned.loras, &track.loras);
    if lora_distance.iter().any(|value| *value != 0) {
        return Distance {
            tier: 1,
            values: lora_distance,
        };
    }

    let seed = integer_difference(&pinned.seed, &track.seed);
    let bpm = numeric_difference(&pinned.bpm, &track.bpm);
    let key = key_distance(&pinned.key, &track.key);
    if seed != 0 || bpm != 0 || key != 0 {
        return Distance {
            tier: 2,
            values: vec![seed, bpm, key],
        };
    }

    let lyrics = edit_distance(&pinned.lyrics, &track.lyrics) as i64;
    let prompt = edit_distance(&pinned.prompt, &track.prompt) as i64;
    Distance {
        tier: if lyrics != 0 || prompt != 0 { 3 } else { 0 },
        values: vec![lyrics, prompt],
    }
}

fn lora_distance(pinned: &[LoRAInfo], track: &[LoRAInfo]) -> Vec<i64> {
    let strength_differences = pinned
        .iter()
        .zip(track)
        .filter(|(left, right)| left.filename == right.filename && left.strength != right.strength)
        .map(|(left, right)| numeric_difference(&left.strength, &right.strength))
        .collect::<Vec<_>>();
    let shared_order_difference = pinned
        .iter()
        .zip(track)
        .filter(|(left, right)| left.filename != right.filename)
        .count() as i64;
    let identity_difference = pinned
        .iter()
        .zip(track)
        .filter(|(left, right)| left.filename != right.filename)
        .count() as i64
        + (pinned.len() as i64 - track.len() as i64).abs();
    vec![
        strength_differences.len() as i64,
        strength_differences.into_iter().sum(),
        shared_order_difference,
        identity_difference,
    ]
}

fn numeric_difference(left: &str, right: &str) -> i64 {
    match (left.parse::<f64>(), right.parse::<f64>()) {
        (Ok(left), Ok(right)) => ((left - right).abs() * 1_000_000.0).round() as i64,
        _ if left == right => 0,
        _ => i64::MAX / 4,
    }
}

fn integer_difference(left: &str, right: &str) -> i64 {
    match (left.parse::<i128>(), right.parse::<i128>()) {
        (Ok(left), Ok(right)) => (left - right).abs().min(i64::MAX as i128) as i64,
        _ if left == right => 0,
        _ => i64::MAX / 4,
    }
}

fn key_distance(left: &str, right: &str) -> i64 {
    if left == right {
        return 0;
    }
    let left = key_index(left);
    let right = key_index(right);
    match (left, right) {
        (Some(left), Some(right)) => (left - right).abs().min(24 - (left - right).abs()),
        _ => 1,
    }
}

fn key_index(key: &str) -> Option<i64> {
    [
        "C major", "C minor", "C# major", "C# minor", "D major", "D minor", "Eb major", "Eb minor",
        "E major", "E minor", "F major", "F minor", "F# major", "F# minor", "G major", "G minor",
        "Ab major", "Ab minor", "A major", "A minor", "Bb major", "Bb minor", "B major", "B minor",
    ]
    .iter()
    .position(|candidate| candidate.eq_ignore_ascii_case(key))
    .map(|index| index as i64)
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right = right.chars().collect::<Vec<_>>();
    let mut distances = (0..=right.len()).collect::<Vec<_>>();
    for (left_index, left_char) in left.chars().enumerate() {
        let mut diagonal = distances[0];
        distances[0] = left_index + 1;
        for (right_index, right_char) in right.iter().enumerate() {
            let previous = distances[right_index + 1];
            distances[right_index + 1] = if left_char == *right_char {
                diagonal
            } else {
                1 + diagonal.min(distances[right_index]).min(previous)
            };
            diagonal = previous;
        }
    }
    distances[right.len()]
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{distance_between, sort_tracks, ComfyUIWorkflow, LoRAInfo};
    use crate::workspace::file_system::{DirEntryInfo, FileKind, SortOrder};

    fn workflow() -> ComfyUIWorkflow {
        ComfyUIWorkflow {
            bpm: "120".into(),
            key: "C major".into(),
            seed: "10".into(),
            ..Default::default()
        }
    }

    #[test]
    fn lower_tier_always_wins() {
        let pinned = workflow();
        let mut lora = workflow();
        lora.loras = vec![LoRAInfo {
            filename: "a".into(),
            strength: "0.1".into(),
        }];
        let mut seed = workflow();
        seed.seed = "11".into();
        assert!(distance_between(&pinned, &lora) < distance_between(&pinned, &seed));
    }

    #[test]
    fn content_changes_are_further_than_generation_changes() {
        let pinned = workflow();
        let mut generation = workflow();
        generation.bpm = "121".into();
        let mut content = workflow();
        content.prompt = "changed".into();
        assert!(distance_between(&pinned, &generation) < distance_between(&pinned, &content));
    }

    #[test]
    fn known_workflows_precede_missing_workflows_without_pinned_metadata() {
        let folder = std::env::temp_dir().join(format!(
            "adb-studio-pinned-sort-{}",
            std::process::id()
        ));
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
        let mut entries = vec![
            entry(&missing),
            entry(&known),
            entry(&pinned),
        ];

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
