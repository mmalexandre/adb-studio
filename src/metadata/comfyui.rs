use serde_json::Value;
use std::{fs, path::Path};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComfyUIWorkflow {
    pub bpm: String,
    pub key: String,
    pub seed: String,
    pub prompt: String,
    pub lyrics: String,
    pub loras: Vec<LoRAInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoRAInfo {
    pub filename: String,
    pub strength: String,
}

pub fn parse_file(path: &Path) -> Result<ComfyUIWorkflow, String> {
    eprintln!("[metadata] parsing workflow: {}", path.display());
    let contents = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_str(&contents).map_err(|error| error.to_string())?;
    let workflow = parse_value(&value);
    eprintln!(
        "[metadata] parsed workflow: bpm={:?}, key={:?}, prompt_chars={}, lyrics_chars={}, loras={}",
        workflow.bpm,
        workflow.key,
        workflow.prompt.chars().count(),
        workflow.lyrics.chars().count(),
        workflow.loras.len()
    );
    Ok(workflow)
}

pub fn parse_value(value: &Value) -> ComfyUIWorkflow {
    let mut workflow = ComfyUIWorkflow::default();
    visit(value, &mut workflow, false);
    workflow
}

fn visit(value: &Value, workflow: &mut ComfyUIWorkflow, lyrics_context: bool) {
    match value {
        Value::Object(object) => {
            if let Some(nodes) = object.get("nodes").and_then(Value::as_array) {
                for node in nodes {
                    parse_visual_node(node, workflow);
                }
            }
            let class_type = object
                .get("class_type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let class_lower = class_type.to_ascii_lowercase();
            let ace_context = class_lower.contains("textencodeacestep")
                || class_lower.contains("acestep") && class_lower.contains("textencode");
            let lora_context = class_lower.contains("load lora")
                || class_lower.contains("loadlora")
                || class_lower.contains("loraloader");

            if lora_context {
                let inputs = object
                    .get("inputs")
                    .and_then(Value::as_object)
                    .unwrap_or(object);
                if let Some(filename) =
                    find_string(inputs, &["lora_name", "filename", "file_name", "name"])
                {
                    let strength = find_scalar(inputs, &["strength_model", "strength", "weight"])
                        .unwrap_or_else(|| "".to_string());
                    workflow.loras.push(LoRAInfo { filename, strength });
                }
            }

            if ace_context {
                if let Some(inputs) = object.get("inputs").and_then(Value::as_object) {
                    for (key, child) in inputs {
                        let key_lower = key.to_ascii_lowercase();
                        if workflow.bpm.is_empty() && key_lower == "bpm" {
                            workflow.bpm = scalar_text(child);
                        } else if workflow.key.is_empty()
                            && (key_lower == "key" || key_lower == "tonality")
                        {
                            workflow.key = scalar_text(child);
                        } else if workflow.seed.is_empty() && key_lower == "seed" {
                            workflow.seed = scalar_text(child);
                        } else if workflow.prompt.is_empty()
                            && (key_lower.contains("prompt") || key_lower == "text")
                        {
                            workflow.prompt = scalar_text(child);
                        } else if workflow.lyrics.is_empty() && key_lower.contains("lyric") {
                            workflow.lyrics = scalar_text(child);
                        }
                    }
                }
            }

            for (key, child) in object {
                let key_lower = key.to_ascii_lowercase();
                let child_lyrics = lyrics_context || key_lower.contains("lyric");
                if ace_context {
                    if workflow.bpm.is_empty() && key_lower == "bpm" {
                        workflow.bpm = scalar_text(child);
                    } else if workflow.key.is_empty()
                        && (key_lower == "key" || key_lower == "tonality")
                    {
                        workflow.key = scalar_text(child);
                    } else if workflow.seed.is_empty() && key_lower == "seed" {
                        workflow.seed = scalar_text(child);
                    } else if workflow.prompt.is_empty()
                        && (key_lower.contains("prompt") || key_lower == "text")
                    {
                        workflow.prompt = scalar_text(child);
                    } else if workflow.lyrics.is_empty() && key_lower.contains("lyric") {
                        workflow.lyrics = scalar_text(child);
                    }
                }
                if workflow.lyrics.is_empty() && child_lyrics && child.is_string() {
                    workflow.lyrics = scalar_text(child);
                }
                visit(child, workflow, child_lyrics);
            }
        }
        Value::Array(array) => {
            for child in array {
                visit(child, workflow, lyrics_context);
            }
        }
        _ => {}
    }
}

fn parse_visual_node(node: &Value, workflow: &mut ComfyUIWorkflow) {
    let Some(object) = node.as_object() else {
        return;
    };
    let node_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let node_lower = node_type.to_ascii_lowercase();
    let widget_values = object
        .get("widgets_values")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let inputs = object.get("inputs").and_then(Value::as_array);
    let mut values = std::collections::HashMap::new();

    if let Some(inputs) = inputs {
        let mut value_index = 0;
        for input in inputs {
            let Some(input_object) = input.as_object() else {
                continue;
            };
            let Some(name) = input_object.get("name").and_then(Value::as_str) else {
                continue;
            };
            if input_object.get("widget").is_none() {
                continue;
            }
            if let Some(value) = widget_values.get(value_index) {
                values.insert(name.to_ascii_lowercase(), value.clone());
            }
            value_index += 1;
            if name.eq_ignore_ascii_case("seed")
                && widget_values
                    .get(value_index)
                    .and_then(Value::as_str)
                    .is_some_and(|value| matches!(value, "fixed" | "randomize"))
            {
                value_index += 1;
            }
        }
    }

    if node_lower.contains("textencodeacestep")
        || (node_lower.contains("acestep") && node_lower.contains("textencode"))
    {
        let uses_positional_metadata = widget_values.len() >= 9
            && !values.contains_key("bpm")
            && !values.contains_key("keyscale");
        if workflow.prompt.is_empty() {
            workflow.prompt = values
                .get("tags")
                .or_else(|| values.get("prompt"))
                .map(scalar_text)
                .unwrap_or_default();
        }
        if workflow.lyrics.is_empty() {
            workflow.lyrics = values.get("lyrics").map(scalar_text).unwrap_or_default();
        }
        if workflow.bpm.is_empty() {
            workflow.bpm = values.get("bpm").map(scalar_text).unwrap_or_default();
        }
        if workflow.seed.is_empty() {
            workflow.seed = values.get("seed").map(scalar_text).unwrap_or_default();
        }
        if workflow.key.is_empty() {
            workflow.key = values
                .get("keyscale")
                .or_else(|| values.get("key"))
                .or_else(|| values.get("tonality"))
                .map(scalar_text)
                .unwrap_or_default();
        }
        if uses_positional_metadata {
            let positional = |index| {
                widget_values
                    .get(index)
                    .map(scalar_text)
                    .unwrap_or_default()
            };
            if workflow.prompt.is_empty() {
                workflow.prompt = positional(0);
            }
            if workflow.lyrics.is_empty() {
                workflow.lyrics = positional(1);
            }
            workflow.seed = positional(2);
            if workflow.bpm.is_empty() {
                workflow.bpm = positional(4);
            }
            if workflow.key.is_empty() {
                workflow.key = positional(8);
            }
        }
    }

    if node_lower.contains("loraloader") || node_lower.contains("loadlora") {
        if let Some(filename) = values
            .get("lora_name")
            .or_else(|| values.get("filename"))
            .or_else(|| values.get("file_name"))
            .or_else(|| values.get("name"))
            .map(scalar_text)
        {
            let strength = values
                .get("strength_model")
                .or_else(|| values.get("strength"))
                .or_else(|| values.get("weight"))
                .map(scalar_text)
                .unwrap_or_default();
            workflow.loras.push(LoRAInfo { filename, strength });
        }
    }
}

fn find_string(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
    })
}

fn find_scalar(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key).map(scalar_text))
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_value, LoRAInfo};
    use serde_json::json;

    #[test]
    fn extracts_ace_step_metadata_and_loras() {
        let workflow = parse_value(&json!({
            "1": {"class_type": "TextEncodeAceStepAudio1.5", "inputs": {
                "bpm": 128, "key": "Bb minor", "prompt": "bright synthwave", "lyrics": "verse"
            }},
            "2": {"class_type": "Load LoRA", "inputs": {
                "lora_name": "style.safetensors", "strength_model": 0.8
            }}
        }));

        assert_eq!(workflow.bpm, "128");
        assert_eq!(workflow.key, "Bb minor");
        assert_eq!(workflow.prompt, "bright synthwave");
        assert_eq!(workflow.lyrics, "verse");
        assert_eq!(
            workflow.loras,
            vec![LoRAInfo {
                filename: "style.safetensors".into(),
                strength: "0.8".into()
            }]
        );
    }

    #[test]
    fn accepts_lyrics_from_a_separate_text_node() {
        let workflow = parse_value(&json!({
            "1": {"class_type": "TextEncodeAceStepAudio1.5", "inputs": {"bpm": 100}},
            "2": {"class_type": "ShowText", "inputs": {"lyrics": "la la la"}}
        }));
        assert_eq!(workflow.lyrics, "la la la");
    }

    #[test]
    fn extracts_metadata_from_visual_workflow_nodes() {
        let workflow = parse_value(&json!({
            "nodes": [
                {
                    "type": "TextEncodeAceStepAudio1.5",
                    "inputs": [
                        {"name": "tags", "widget": {"name": "tags"}, "link": null},
                        {"name": "lyrics", "widget": {"name": "lyrics"}, "link": null},
                        {"name": "seed", "widget": {"name": "seed"}, "link": 1},
                        {"name": "bpm", "widget": {"name": "bpm"}, "link": null},
                        {"name": "keyscale", "widget": {"name": "keyscale"}, "link": null}
                    ],
                    "widgets_values": ["prompt", "lyrics", 31, 130, "E minor"]
                },
                {
                    "type": "LoraLoaderModelOnly",
                    "inputs": [
                        {"name": "model", "link": 2},
                        {"name": "lora_name", "widget": {"name": "lora_name"}, "link": null},
                        {"name": "strength_model", "widget": {"name": "strength_model"}, "link": null}
                    ],
                    "widgets_values": ["style.safetensors", 0.8]
                }
            ]
        }));

        assert_eq!(workflow.bpm, "130");
        assert_eq!(workflow.key, "E minor");
        assert_eq!(workflow.seed, "31");
        assert_eq!(workflow.prompt, "prompt");
        assert_eq!(workflow.lyrics, "lyrics");
        assert_eq!(workflow.loras[0].filename, "style.safetensors");
        assert_eq!(workflow.loras[0].strength, "0.8");
    }

    #[test]
    fn skips_seed_control_value_before_bpm() {
        let workflow = parse_value(&json!({
            "nodes": [{
                "type": "TextEncodeAceStepAudio1.5",
                "inputs": [
                    {"name": "tags", "widget": {}},
                    {"name": "lyrics", "widget": {}},
                    {"name": "seed", "widget": {}},
                    {"name": "bpm", "widget": {}}
                ],
                "widgets_values": ["prompt", "lyrics", 31, "fixed", 130]
            }]
        }));

        assert_eq!(workflow.bpm, "130");
    }

    #[test]
    fn extracts_metadata_from_positional_acestep_export() {
        let workflow = parse_value(&json!({
            "nodes": [{
                "type": "TextEncodeAceStepAudio1.5",
                "inputs": [
                    {"name": "clip"},
                    {"name": "seed", "widget": {"name": "seed"}},
                    {"name": "duration", "widget": {"name": "duration"}}
                ],
                "widgets_values": [
                    "prompt text", "[Verse] lyrics", 32, "fixed", 125, 30,
                    "4", "en", "E minor", true, 2, 0.85, 0.9, 0, 0
                ]
            }]
        }));

        assert_eq!(workflow.bpm, "125");
        assert_eq!(workflow.key, "E minor");
        assert_eq!(workflow.seed, "32");
        assert_eq!(workflow.prompt, "prompt text");
        assert_eq!(workflow.lyrics, "[Verse] lyrics");
    }
}
