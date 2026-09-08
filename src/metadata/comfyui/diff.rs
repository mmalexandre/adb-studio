use std::path::Path;

use super::{parser, ComfyUIWorkflow, LoRAInfo, TrackDifference};

pub fn compare_files(folder: &Path, pinned_path: &Path, track_path: &Path) -> Vec<TrackDifference> {
    let Some(track_workflow_path) = crate::metadata::workflow_path(folder, track_path) else {
        return vec![TrackDifference {
            label: String::new(),
            value: "missing workflow file".to_string(),
        }];
    };
    let Ok(track_workflow) = parser::parse_file(&track_workflow_path) else {
        return vec![TrackDifference {
            label: String::new(),
            value: "missing workflow file".to_string(),
        }];
    };
    if pinned_path == track_path {
        return Vec::new();
    }
    let Some(pinned_workflow_path) = crate::metadata::workflow_path(folder, pinned_path) else {
        return Vec::new();
    };
    let Ok(pinned_workflow) = parser::parse_file(&pinned_workflow_path) else {
        return Vec::new();
    };
    compare_workflows(&pinned_workflow, &track_workflow)
}

fn compare_workflows(
    pinned_workflow: &ComfyUIWorkflow,
    track_workflow: &ComfyUIWorkflow,
) -> Vec<TrackDifference> {
    let mut differences = Vec::new();
    add_numeric_difference(
        &mut differences,
        "BPM: ",
        &pinned_workflow.bpm,
        &track_workflow.bpm,
    );
    add_numeric_difference(
        &mut differences,
        "Seed: ",
        &pinned_workflow.seed,
        &track_workflow.seed,
    );
    add_text_difference(
        &mut differences,
        "Key: ",
        &pinned_workflow.key,
        &track_workflow.key,
    );
    add_text_difference(
        &mut differences,
        "Model: ",
        &pinned_workflow.model,
        &track_workflow.model,
    );
    add_count_difference(
        &mut differences,
        "Prompt: ",
        &pinned_workflow.prompt,
        &track_workflow.prompt,
    );
    add_count_difference(
        &mut differences,
        "Lyrics: ",
        &pinned_workflow.lyrics,
        &track_workflow.lyrics,
    );
    add_lora_differences(
        &mut differences,
        &pinned_workflow.loras,
        &track_workflow.loras,
    );
    differences
}

fn add_text_difference(
    differences: &mut Vec<TrackDifference>,
    label: &str,
    pinned: &str,
    track: &str,
) {
    if pinned != track {
        differences.push(TrackDifference {
            label: label.to_string(),
            value: track.to_string(),
        });
    }
}

fn add_numeric_difference(
    differences: &mut Vec<TrackDifference>,
    label: &str,
    pinned: &str,
    track: &str,
) {
    if pinned == track {
        return;
    }
    let value = match (pinned.parse::<f64>(), track.parse::<f64>()) {
        (Ok(pinned), Ok(track)) => format_number(track, Some(track - pinned)),
        _ => track.to_string(),
    };
    differences.push(TrackDifference {
        label: label.to_string(),
        value,
    });
}

fn add_count_difference(
    differences: &mut Vec<TrackDifference>,
    label: &str,
    pinned: &str,
    track: &str,
) {
    if pinned != track {
        differences.push(TrackDifference {
            label: label.to_string(),
            value: edit_distance(pinned, track).to_string(),
        });
    }
}

fn add_lora_differences(
    differences: &mut Vec<TrackDifference>,
    pinned: &[LoRAInfo],
    track: &[LoRAInfo],
) {
    if pinned.len() != track.len() {
        differences.push(TrackDifference {
            label: "Loras: ".to_string(),
            value: format_number(
                track.len() as f64,
                Some(track.len() as f64 - pinned.len() as f64),
            ),
        });
    }
    for lora in track {
        let Some(pinned_lora) = pinned
            .iter()
            .find(|candidate| candidate.filename == lora.filename)
        else {
            continue;
        };
        if pinned_lora.strength == lora.strength {
            continue;
        }
        let value = match (
            pinned_lora.strength.parse::<f64>(),
            lora.strength.parse::<f64>(),
        ) {
            (Ok(pinned), Ok(track)) => format_number(track, Some(track - pinned)),
            _ => lora.strength.clone(),
        };
        differences.push(TrackDifference {
            label: format!("Lora {} strength: ", lora.filename),
            value,
        });
    }
}

fn format_number(value: f64, delta: Option<f64>) -> String {
    let value = if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    };
    match delta {
        Some(delta) if delta != 0.0 => {
            let delta = if delta.fract() == 0.0 {
                format!("{delta:+.0}")
            } else {
                format!("{delta:+.2}")
            };
            format!("{value} ({delta})")
        }
        _ => value,
    }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut distances: Vec<usize> = (0..=right.len()).collect();
    for (left_index, left_char) in left.chars().enumerate() {
        let mut diagonal = distances[0];
        distances[0] = left_index + 1;
        for right_index in 0..right.len() {
            let previous = distances[right_index + 1];
            distances[right_index + 1] = if left_char == right[right_index] {
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
    use super::{compare_workflows, edit_distance, format_number};
    use crate::metadata::comfyui::{ComfyUIWorkflow, LoRAInfo};

    #[test]
    fn counts_prompt_edits_and_formats_deltas() {
        assert_eq!(edit_distance("kit", "kat"), 1);
        assert_eq!(edit_distance("same", "same"), 0);
        assert_eq!(format_number(130.0, Some(5.0)), "130 (+5)");
        assert_eq!(format_number(34.0, Some(2.0)), "34 (+2)");
        assert_eq!(format_number(2.0, Some(2.0)), "2 (+2)");
        assert_eq!(format_number(2.0, Some(0.5)), "2 (+0.50)");
    }

    #[test]
    fn reports_workflow_field_and_lora_differences() {
        let pinned = ComfyUIWorkflow {
            bpm: "120".into(),
            seed: "10".into(),
            key: "C major".into(),
            model: "old.ckpt".into(),
            prompt: "old prompt".into(),
            lyrics: "old lyrics".into(),
            loras: vec![LoRAInfo {
                filename: "voice.safetensors".into(),
                strength: "0.50".into(),
            }],
        };
        let track = ComfyUIWorkflow {
            bpm: "121.5".into(),
            seed: "11".into(),
            key: "D major".into(),
            model: "new.ckpt".into(),
            prompt: "new prompt".into(),
            lyrics: "new lyrics".into(),
            loras: vec![
                LoRAInfo {
                    filename: "voice.safetensors".into(),
                    strength: "0.75".into(),
                },
                LoRAInfo {
                    filename: "drums.safetensors".into(),
                    strength: "0.25".into(),
                },
            ],
        };

        let differences = compare_workflows(&pinned, &track);

        assert!(differences.iter().any(|difference| {
            difference.label == "BPM: " && difference.value == "121.50 (+1.50)"
        }));
        assert!(differences.iter().any(|difference| {
            difference.label == "Seed: " && difference.value == "11 (+1)"
        }));
        assert!(differences.iter().any(|difference| {
            difference.label == "Key: " && difference.value == "D major"
        }));
        assert!(differences.iter().any(|difference| {
            difference.label == "Model: " && difference.value == "new.ckpt"
        }));
        assert!(differences.iter().any(|difference| difference.label == "Prompt: "));
        assert!(differences.iter().any(|difference| difference.label == "Lyrics: "));
        assert!(differences.iter().any(|difference| {
            difference.label == "Loras: " && difference.value == "2 (+1)"
        }));
        assert!(differences.iter().any(|difference| {
            difference.label == "Lora voice.safetensors strength: "
                && difference.value == "0.75 (+0.25)"
        }));
    }

    #[test]
    fn omits_equal_fields_and_handles_non_numeric_values() {
        let workflow = ComfyUIWorkflow {
            bpm: "unknown".into(),
            seed: "seed".into(),
            key: "C major".into(),
            ..Default::default()
        };
        let mut changed = workflow.clone();
        changed.bpm = "fast".into();
        changed.seed = "new seed".into();

        let differences = compare_workflows(&workflow, &changed);

        assert_eq!(differences.len(), 2);
        assert_eq!(differences[0].value, "fast");
        assert_eq!(differences[1].value, "new seed");
    }
}
