mod diff;
mod parser;
mod updater;

pub use diff::compare_files;
pub use parser::parse_file;
pub use updater::update_metadata;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComfyUIWorkflow {
    pub bpm: String,
    pub key: String,
    pub seed: String,
    pub model: String,
    pub prompt: String,
    pub lyrics: String,
    pub loras: Vec<LoRAInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoRAInfo {
    pub filename: String,
    pub strength: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackDifference {
    pub label: String,
    pub value: String,
}
