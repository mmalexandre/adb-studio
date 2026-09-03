use serde_json::Value;
use std::{fs, path::Path};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComfyUIWorkflow {
    pub bpm: String,
    pub key: String,
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
    let contents = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_str(&contents).map_err(|error| error.to_string())?;
    Ok(parse_value(&value))
}

pub fn parse_value(value: &Value) -> ComfyUIWorkflow {
    let mut workflow = ComfyUIWorkflow::default();
    visit(value, &mut workflow, false);
    workflow
}

fn visit(value: &Value, workflow: &mut ComfyUIWorkflow, lyrics_context: bool) {
    match value {
        Value::Object(object) => {
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
}
