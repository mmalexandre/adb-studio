mod diff;
mod parser;
mod updater;

pub use diff::compare_files;
pub use parser::{
    clear_memory_cache, is_runnable_audio_workflow, parse_file_cached, parse_value,
    workflow_to_api_prompt,
};
pub use updater::{
    add_lora, remove_lora, reorder_loras, set_lora_path, set_lora_strength, update_metadata,
};

use serde::{Deserialize, Serialize};

pub fn display_lora_name(filename: &str) -> String {
    filename
        .strip_suffix(".safetensors")
        .unwrap_or(filename)
        .to_string()
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ComfyUIWorkflow {
    pub bpm: String,
    pub duration: String,
    pub key: String,
    pub seed: String,
    pub ksampler_cfg: String,
    pub ksampler_steps: String,
    pub reference_audio: String,
    pub reference_audio_hash: String,
    pub reference_audio_guess_attempted: bool,
    pub reference_audio_ambiguous: bool,
    pub model: String,
    pub prompt: String,
    pub lyrics: String,
    pub loras: Vec<LoRAInfo>,
}

pub fn set_reference_audio_metadata(
    workflow: &mut serde_json::Value,
    hash: &str,
    guess_attempted: bool,
    ambiguous: bool,
) {
    let Some(object) = workflow.as_object_mut() else {
        return;
    };
    let metadata = object
        .entry("_adb_studio")
        .or_insert_with(|| serde_json::json!({}));
    let Some(metadata) = metadata.as_object_mut() else {
        return;
    };
    metadata.insert("reference_audio_hash".into(), hash.into());
    metadata.insert(
        "reference_audio_guess_attempted".into(),
        guess_attempted.into(),
    );
    metadata.insert("reference_audio_ambiguous".into(), ambiguous.into());
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LoRAInfo {
    pub node_id: String,
    pub filename: String,
    pub custom_path: String,
    pub strength: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackDifference {
    pub label: String,
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::display_lora_name;

    #[test]
    fn display_lora_name_strips_only_safetensors_suffix() {
        assert_eq!(display_lora_name("voice.safetensors"), "voice");
        assert_eq!(display_lora_name("voice.ckpt"), "voice.ckpt");
    }
}
