mod diff;
mod parser;
mod updater;

pub use diff::compare_files;
pub use parser::{
    clear_memory_cache, is_runnable_audio_workflow, parse_file_cached, parse_value,
    workflow_to_api_prompt,
};
pub use updater::{add_lora, remove_lora, reorder_loras, set_lora_strength, update_metadata};

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
    pub model: String,
    pub prompt: String,
    pub lyrics: String,
    pub loras: Vec<LoRAInfo>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LoRAInfo {
    pub node_id: String,
    pub filename: String,
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
