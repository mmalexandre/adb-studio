use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::UNIX_EPOCH,
};

use super::{ComfyUIWorkflow, LoRAInfo};

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct CachedWorkflow {
    relative_path: String,
    size: u64,
    modified_nanos: u128,
    json_hash: String,
    workflow: ComfyUIWorkflow,
}

static MEMORY_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedWorkflow>>> = OnceLock::new();

pub fn clear_memory_cache() {
    if let Some(cache) = MEMORY_CACHE.get() {
        cache.lock().expect("workflow cache mutex poisoned").clear();
    }
}

pub fn parse_file_cached(folder: &Path, path: &Path) -> Result<ComfyUIWorkflow, String> {
    parse_file_cached_with_hash(folder, path).map(|(workflow, _)| workflow)
}

pub fn parse_file_cached_with_hash(
    folder: &Path,
    path: &Path,
) -> Result<(ComfyUIWorkflow, String), String> {
    let relative_path = path
        .strip_prefix(folder)
        .map_err(|error| format!("workflow path is outside workspace: {error}"))?
        .to_string_lossy()
        .replace('\\', "/");
    let file_metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    let modified_nanos = file_metadata
        .modified()
        .map_err(|error| error.to_string())?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let path_key = path.to_path_buf();

    if let Some(cached) = memory_cache()
        .lock()
        .expect("workflow cache mutex poisoned")
        .get(&path_key)
        .filter(|cached| {
            cached.relative_path == relative_path
                && cached.size == file_metadata.len()
                && cached.modified_nanos == modified_nanos
        })
        .cloned()
    {
        return Ok((cached.workflow, cached.json_hash));
    }

    let cache_path = cache_path(folder, &relative_path);
    if let Some(cached) = load_cache_entry(&cache_path).filter(|cached| {
        cached.relative_path == relative_path
            && cached.size == file_metadata.len()
            && cached.modified_nanos == modified_nanos
    }) {
        memory_cache()
            .lock()
            .expect("workflow cache mutex poisoned")
            .insert(path_key, cached.clone());
        return Ok((cached.workflow, cached.json_hash));
    }

    let (workflow, json_hash) = parse_file_with_hash(path)?;
    let cached = CachedWorkflow {
        relative_path,
        size: file_metadata.len(),
        modified_nanos,
        json_hash: json_hash.clone(),
        workflow: workflow.clone(),
    };
    memory_cache()
        .lock()
        .expect("workflow cache mutex poisoned")
        .insert(path_key, cached.clone());
    save_cache_entry(&cache_path, &cached);
    Ok((workflow, json_hash))
}

fn parse_file_with_hash(path: &Path) -> Result<(ComfyUIWorkflow, String), String> {
    eprintln!("[metadata] parsing workflow: {}", path.display());
    let contents = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_str(&contents).map_err(|error| error.to_string())?;
    let workflow = parse_value(&value);
    let json_hash = canonical_json_hash(&value);
    eprintln!(
        "[metadata] parsed workflow: bpm={:?}, key={:?}, prompt_chars={}, lyrics_chars={}, loras={}",
        workflow.bpm,
        workflow.key,
        workflow.prompt.chars().count(),
        workflow.lyrics.chars().count(),
        workflow.loras.len()
    );
    Ok((workflow, json_hash))
}

fn memory_cache() -> &'static Mutex<HashMap<PathBuf, CachedWorkflow>> {
    MEMORY_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_path(folder: &Path, relative_path: &str) -> PathBuf {
    let mut digest = Sha256::new();
    digest.update(relative_path.as_bytes());
    folder
        .join(".adbstudio")
        .join("workflow-cache")
        .join(format!("{:x}.json", digest.finalize()))
}

fn load_cache_entry(path: &Path) -> Option<CachedWorkflow> {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
}

fn save_cache_entry(path: &Path, entry: &CachedWorkflow) {
    let Some(directory) = path.parent() else {
        return;
    };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    let Ok(contents) = serde_json::to_string(entry) else {
        return;
    };
    let _ = fs::write(path, contents);
}

fn canonical_json_hash(value: &Value) -> String {
    let canonical = serde_json::to_vec(value).expect("JSON values should be serializable");
    let mut digest = Sha256::new();
    digest.update(canonical);
    format!("{:x}", digest.finalize())
}

pub fn parse_value(value: &Value) -> ComfyUIWorkflow {
    let mut workflow = ComfyUIWorkflow::default();
    visit(value, &mut workflow, false, None);
    if value.get("nodes").is_some() {
        if let Ok(prompt) = workflow_to_api_prompt(value) {
            visit(&prompt, &mut workflow, false, None);
        }
    }
    let active_loras = active_lora_ids(value);
    let mut seen_loras = std::collections::HashSet::new();
    workflow
        .loras
        .retain(|lora| {
            active_loras.contains(&lora.node_id) && seen_loras.insert(lora.node_id.clone())
        });
    workflow
}

pub fn workflow_to_api_prompt(value: &Value) -> Result<Value, String> {
    let Some(object) = value.as_object() else {
        return Err("workflow must be a JSON object".into());
    };
    if object.values().all(|node| node.get("class_type").is_some()) {
        return Ok(value.clone());
    }
    let Some(nodes) = object.get("nodes").and_then(Value::as_array) else {
        return Err("workflow is neither an API prompt nor a visual graph".into());
    };
    let links = object
        .get("links")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|link| {
            let link = link.as_array()?;
            Some((
                link.first()?.as_i64()?,
                (link.get(1)?.as_i64()?.to_string(), link.get(2)?.as_i64()?),
            ))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut primitive_values = nodes
        .iter()
        .filter(|node| node.get("type").and_then(Value::as_str) == Some("PrimitiveNode"))
        .filter_map(|node| {
            let value = node
                .get("widgets_values")
                .and_then(Value::as_array)
                .and_then(|values| values.first())?
                .clone();
            let links = node
                .get("outputs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(|output| output.get("links").and_then(Value::as_array))
                .flatten()
                .filter_map(Value::as_i64)
                .map(|link_id| (link_id, value.clone()));
            Some(links.collect::<Vec<_>>())
        })
        .flatten()
        .collect::<std::collections::HashMap<_, _>>();
    for link in object
        .get("links")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(link) = link.as_array() else {
            continue;
        };
        let Some(link_id) = link.first().and_then(Value::as_i64) else {
            continue;
        };
        let Some(source_id) = link.get(1).map(scalar_text) else {
            continue;
        };
        let Some(source) = nodes.iter().find(|node| {
            node.get("id").map(scalar_text).as_deref() == Some(source_id.as_str())
                && node.get("type").and_then(Value::as_str) == Some("PrimitiveNode")
        }) else {
            continue;
        };
        if let Some(value) = source
            .get("widgets_values")
            .and_then(Value::as_array)
            .and_then(|values| values.first())
        {
            primitive_values.insert(link_id, value.clone());
        }
    }
    let mut prompt = serde_json::Map::new();
    for node in nodes {
        let id = node
            .get("id")
            .map(scalar_text)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "visual workflow node has no id".to_string())?;
        let class_type = node
            .get("type")
            .and_then(Value::as_str)
            .filter(|node_type| !node_type.is_empty())
            .ok_or_else(|| format!("workflow node {id} has no type"))?;
        if class_type == "PrimitiveNode" || class_type == "MarkdownNote" {
            continue;
        }
        let input_nodes = node.get("inputs").and_then(Value::as_array);
        let widget_values = node.get("widgets_values").and_then(Value::as_array);
        let widget_names = visual_widget_names(class_type);
        let mut widget_index = 0;
        let mut inputs = serde_json::Map::new();
        if let Some(input_nodes) = input_nodes {
            for input in input_nodes {
                let name = input
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("workflow node {id} has an unnamed input"))?;
                if let Some(link_id) = input.get("link").and_then(Value::as_i64) {
                    if let Some(value) = primitive_values.get(&link_id) {
                        inputs.insert(name.into(), value.clone());
                        continue;
                    }
                    let Some((source_id, output_index)) = links.get(&link_id) else {
                        return Err(format!(
                            "workflow node {id} references missing link {link_id}"
                        ));
                    };
                    inputs.insert(name.into(), serde_json::json!([source_id, output_index]));
                    if input.get("widget").is_some() {
                        widget_index = widget_index.max(
                            widget_names
                                .iter()
                                .position(|widget_name| *widget_name == name)
                                .map(|index| index + 1)
                                .unwrap_or(widget_index),
                        );
                    }
                } else if input.get("widget").is_some() {
                    let value_index = widget_names
                        .iter()
                        .position(|widget_name| *widget_name == name)
                        .unwrap_or(widget_index);
                    if let Some(value) = widget_values.and_then(|values| values.get(value_index)) {
                        inputs.insert(name.into(), value.clone());
                    }
                    widget_index = widget_index.max(value_index + 1);
                }
            }
        }
        for (index, name) in widget_names.iter().enumerate() {
            if !inputs.contains_key(*name)
                && !input_nodes.is_some_and(|inputs| {
                    inputs.iter().any(|input| {
                        input.get("name").and_then(Value::as_str) == Some(name)
                            && input.get("link").is_some()
                    })
                })
            {
                if let Some(value) = widget_values.and_then(|values| values.get(index)) {
                    inputs.insert((*name).into(), value.clone());
                }
            }
        }
        if class_type == "PrimitiveNode" {
            if let Some(value) = widget_values.and_then(|values| values.first()) {
                inputs.insert("value".into(), value.clone());
            }
        }
        prompt.insert(
            id,
            serde_json::json!({"class_type": class_type, "inputs": inputs}),
        );
    }
    if prompt.is_empty() {
        return Err("visual workflow has no nodes".into());
    }
    Ok(Value::Object(prompt))
}

fn active_lora_ids(value: &Value) -> std::collections::HashSet<String> {
    let mut active = std::collections::HashSet::new();
    let mut pending = workflow_nodes(value)
        .into_iter()
        .flatten()
        .filter(|(_, node)| is_ksampler_node(node))
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    let mut visited = std::collections::HashSet::new();

    while let Some(node_id) = pending.pop() {
        if !visited.insert(node_id.clone()) {
            continue;
        }
        let Some(node) = find_workflow_node(value, &node_id) else {
            continue;
        };
        if is_lora_node(node) && !is_bypassed_node(node) {
            active.insert(node_id);
        }
        if let Some(upstream_id) = upstream_model_id(value, node) {
            pending.push(upstream_id);
        }
    }

    active
}

fn workflow_nodes<'a>(value: &'a Value) -> Option<Vec<(String, &'a Value)>> {
    let object = value.as_object()?;
    if let Some(nodes) = object.get("nodes").and_then(Value::as_array) {
        return Some(
            nodes
                .iter()
                .filter_map(|node| {
                    let id = node
                        .get("id")
                        .map(scalar_text)
                        .filter(|id| !id.is_empty())?;
                    Some((id, node))
                })
                .collect(),
        );
    }
    Some(
        object
            .iter()
            .filter_map(|(id, node)| node.get("class_type").is_some().then(|| (id.clone(), node)))
            .collect(),
    )
}

fn find_workflow_node<'a>(value: &'a Value, node_id: &str) -> Option<&'a Value> {
    workflow_nodes(value)?
        .into_iter()
        .find_map(|(id, node)| (id == node_id).then_some(node))
}

fn is_ksampler_node(node: &Value) -> bool {
    node.get("class_type")
        .or_else(|| node.get("type"))
        .and_then(Value::as_str)
        .map(|node_type| node_type.to_ascii_lowercase().contains("ksampler"))
        .unwrap_or(false)
}

fn is_lora_node(node: &Value) -> bool {
    node.get("class_type")
        .or_else(|| node.get("type"))
        .and_then(Value::as_str)
        .map(|node_type| {
            node_type
                .to_ascii_lowercase()
                .replace([' ', '_', '-'], "")
                .contains("loraloader")
                || node_type
                    .to_ascii_lowercase()
                    .replace([' ', '_', '-'], "")
                    .contains("loadlora")
        })
        .unwrap_or(false)
}

fn is_bypassed_node(node: &Value) -> bool {
    node.get("mode").and_then(Value::as_i64) == Some(4)
}

pub fn is_runnable_audio_workflow(value: &Value) -> bool {
    let Some(nodes) = workflow_nodes(value) else {
        return false;
    };
    let active_nodes = nodes
        .into_iter()
        .filter(|(_, node)| !is_bypassed_node(node))
        .map(|(_, node)| node);
    let mut has_sampler = false;
    let mut has_audio_node = false;
    for node in active_nodes {
        let node_type = node
            .get("class_type")
            .or_else(|| node.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if node_type.contains("trainer")
            || node_type.contains("preprocess")
            || node_type.contains("llmloader")
        {
            return false;
        }
        has_sampler |= node_type.contains("ksampler");
        has_audio_node |= node_type.contains("acestep")
            && (node_type.contains("audio") || node_type.contains("latent"));
    }
    has_sampler && has_audio_node
}

fn upstream_model_id(value: &Value, node: &Value) -> Option<String> {
    if let Some(inputs) = node.get("inputs").and_then(Value::as_object) {
        return inputs.iter().find_map(|(name, reference)| {
            (name.eq_ignore_ascii_case("model") || name.to_ascii_lowercase().starts_with("model"))
                .then(|| {
                    reference.as_array().and_then(|reference| {
                        reference
                            .first()
                            .map(scalar_text)
                            .filter(|id| !id.is_empty())
                    })
                })
                .flatten()
        });
    }

    let link_id = node
        .get("inputs")
        .and_then(Value::as_array)
        .and_then(|inputs| {
            inputs.iter().find_map(|input| {
                let name = input.get("name").and_then(Value::as_str)?;
                (name.eq_ignore_ascii_case("model")
                    || name.to_ascii_lowercase().starts_with("model"))
                .then(|| input.get("link").map(scalar_text))
                .flatten()
            })
        })?;
    value
        .get("links")
        .and_then(Value::as_array)
        .and_then(|links| {
            links.iter().find_map(|link| {
                let link = link.as_array()?;
                (link.first().map(scalar_text).as_deref() == Some(link_id.as_str()))
                    .then(|| link.get(1).map(scalar_text))
                    .flatten()
            })
        })
}

fn visit(
    value: &Value,
    workflow: &mut ComfyUIWorkflow,
    lyrics_context: bool,
    node_id: Option<&str>,
) {
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
                    let filename_basename = basename(&filename);
                    let strength = find_scalar(inputs, &["strength_model", "strength", "weight"])
                        .unwrap_or_else(|| "".to_string());
                    workflow.loras.push(LoRAInfo {
                        node_id: node_id.unwrap_or_default().to_string(),
                        filename: filename_basename.clone(),
                        custom_path: (filename != filename_basename)
                            .then_some(filename)
                            .unwrap_or_default(),
                        strength,
                    });
                }
            }

            if class_lower.contains("ksampler") {
                let inputs = object
                    .get("inputs")
                    .and_then(Value::as_object)
                    .unwrap_or(object);
                if workflow.ksampler_cfg.is_empty() {
                    workflow.ksampler_cfg = find_scalar(inputs, &["cfg"]).unwrap_or_default();
                }
                if workflow.ksampler_steps.is_empty() {
                    workflow.ksampler_steps =
                        find_scalar(inputs, &["steps"]).unwrap_or_default();
                }
            }

            if class_lower.replace([' ', '_', '-'], "").contains("loadaudio") {
                let inputs = object
                    .get("inputs")
                    .and_then(Value::as_object)
                    .unwrap_or(object);
                if workflow.reference_audio.is_empty() {
                    workflow.reference_audio = find_string(
                        inputs,
                        &["audio", "audio_name", "filename", "file_name", "name"],
                    )
                    .or_else(|| {
                        inputs
                            .values()
                            .find(|value| value.is_string())
                            .map(scalar_text)
                    })
                    .map(|value| basename(&value))
                    .unwrap_or_default();
                }
            }

            if workflow.model.is_empty() {
                let inputs = object
                    .get("inputs")
                    .and_then(Value::as_object)
                    .unwrap_or(object);
                workflow.model = find_string(
                    inputs,
                    &[
                        "ckpt_name",
                        "checkpoint",
                        "checkpoint_name",
                        "model_name",
                        "unet_name",
                    ],
                )
                .unwrap_or_default();
            }

            if ace_context {
                if let Some(inputs) = object.get("inputs").and_then(Value::as_object) {
                    for (key, child) in inputs {
                        let key_lower = key.to_ascii_lowercase();
                        if workflow.bpm.is_empty() && key_lower == "bpm" {
                            workflow.bpm = scalar_text(child);
                        } else if workflow.duration.is_empty() && key_lower == "duration" {
                            workflow.duration = scalar_text(child);
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
                    } else if workflow.duration.is_empty() && key_lower == "duration" {
                        workflow.duration = scalar_text(child);
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
                visit(child, workflow, child_lyrics, node_id.or(Some(key)));
            }
        }
        Value::Array(array) => {
            for child in array {
                visit(child, workflow, lyrics_context, node_id);
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
    let node_lower = format!(
        "{} {}",
        node_type,
        object
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| properties.get("Node name for S&R"))
            .map(scalar_text)
            .unwrap_or_default()
    )
    .to_ascii_lowercase();
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
        if workflow.duration.is_empty() {
            workflow.duration = values.get("duration").map(scalar_text).unwrap_or_default();
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
            workflow.duration = positional(5);
            if workflow.key.is_empty() {
                workflow.key = positional(8);
            }
        }
    }

    if node_lower.contains("ksampler") {
        if workflow.ksampler_steps.is_empty() {
            workflow.ksampler_steps = values
                .get("steps")
                .or_else(|| widget_values.get(2))
                .map(scalar_text)
                .unwrap_or_default();
        }
        if workflow.ksampler_cfg.is_empty() {
            workflow.ksampler_cfg = values
                .get("cfg")
                .or_else(|| widget_values.get(3))
                .map(scalar_text)
                .unwrap_or_default();
        }
    }

    if node_lower.replace([' ', '_', '-'], "").contains("loadaudio")
        && workflow.reference_audio.is_empty()
    {
        workflow.reference_audio = values
            .get("audio")
            .or_else(|| values.get("audio_name"))
            .or_else(|| values.get("filename"))
            .or_else(|| widget_values.first())
            .map(scalar_text)
            .map(|value| basename(&value))
            .unwrap_or_default();
    }

    if workflow.model.is_empty()
        && (node_lower.contains("checkpointloader")
            || node_lower.contains("unetloader")
            || node_lower.contains("model loader"))
    {
        workflow.model = values
            .get("ckpt_name")
            .or_else(|| values.get("checkpoint"))
            .or_else(|| values.get("checkpoint_name"))
            .or_else(|| values.get("model_name"))
            .or_else(|| values.get("unet_name"))
            .map(scalar_text)
            .unwrap_or_default();
        if workflow.model.is_empty() {
            workflow.model = object
                .get("properties")
                .and_then(Value::as_object)
                .and_then(|properties| properties.get("models"))
                .and_then(Value::as_array)
                .and_then(|models| models.first())
                .and_then(Value::as_object)
                .and_then(|model| model.get("name"))
                .map(scalar_text)
                .unwrap_or_default();
        }
    }

    if node_lower.contains("loraloader") || node_lower.contains("loadlora") {
        let filename = values
            .get("lora_name")
            .or_else(|| values.get("filename"))
            .or_else(|| values.get("file_name"))
            .or_else(|| values.get("name"))
            .map(scalar_text)
            .or_else(|| widget_values.first().map(scalar_text));
        if let Some(filename) = filename {
            let filename_basename = basename(&filename);
            let strength = values
                .get("strength_model")
                .or_else(|| values.get("strength"))
                .or_else(|| values.get("weight"))
                .map(scalar_text)
                .or_else(|| widget_values.get(1).map(scalar_text))
                .unwrap_or_default();
            workflow.loras.push(LoRAInfo {
                node_id: object.get("id").map(scalar_text).unwrap_or_default(),
                filename: filename_basename.clone(),
                custom_path: (filename != filename_basename)
                    .then_some(filename)
                    .unwrap_or_default(),
                strength,
            });
        }
    }
}

fn basename(value: &str) -> String {
    value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value)
        .to_string()
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

fn visual_widget_names(node_type: &str) -> &'static [&'static str] {
    match node_type {
        "CheckpointLoaderSimple" => &["ckpt_name"],
        "LoraLoaderModelOnly" => &["lora_name", "strength_model"],
        "ModelSamplingAuraFlow" => &["shift"],
        "EmptyAceStep1.5LatentAudio" => &["seconds", "batch_size"],
        "KSampler" => &[
            "seed",
            "control_after_generate",
            "steps",
            "cfg",
            "sampler_name",
            "scheduler",
            "denoise",
        ],
        "TextEncodeAceStepAudio1.5" => &[
            "tags",
            "lyrics",
            "seed",
            "control_after_generate",
            "bpm",
            "duration",
            "timesignature",
            "language",
            "keyscale",
            "generate_audio_codes",
            "cfg_scale",
            "temperature",
            "top_p",
            "top_k",
            "min_p",
        ],
        "ADBDMusicPlayer" | "ADBMusicPlayer" => {
            &["filename_prefix", "format", "quality", "audio_format"]
        }
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_runnable_audio_workflow, memory_cache, parse_file_cached_with_hash, parse_value,
    };
    use crate::metadata::comfyui::LoRAInfo;
    use serde_json::json;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("adb-studio-workflow-cache-{suffix}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn extracts_ace_step_metadata_and_loras() {
        let workflow = parse_value(&json!({
            "1": {"class_type": "TextEncodeAceStepAudio1.5", "inputs": {
                "bpm": 128, "key": "Bb minor", "prompt": "bright synthwave", "lyrics": "verse"
            }},
            "2": {"class_type": "CheckpointLoaderSimple", "inputs": {
                "ckpt_name": "model.safetensors"
            }},
            "3": {"class_type": "Load LoRA", "inputs": {
                "lora_name": "style.safetensors", "strength_model": 0.8
            }},
            "4": {"class_type": "KSampler", "inputs": {
                "model": ["3", 0]
            }}
        }));

        assert_eq!(workflow.bpm, "128");
        assert_eq!(workflow.duration, "");
        assert_eq!(workflow.key, "Bb minor");
        assert_eq!(workflow.model, "model.safetensors");
        assert_eq!(workflow.prompt, "bright synthwave");
        assert_eq!(workflow.lyrics, "verse");
        assert_eq!(
            workflow.loras,
            vec![LoRAInfo {
                node_id: "3".into(),
                filename: "style.safetensors".into(),
                custom_path: String::new(),
                strength: "0.8".into()
            }]
        );
    }

    #[test]
    fn identifies_api_audio_generation_workflow() {
        assert!(is_runnable_audio_workflow(&json!({
            "1": {"class_type": "TextEncodeAceStepAudio1.5", "inputs": {}},
            "2": {"class_type": "KSampler", "inputs": {}}
        })));
    }

    #[test]
    fn converts_visual_workflow_to_api_prompt() {
        let prompt = super::workflow_to_api_prompt(&json!({
            "nodes": [
                {"id": 1, "type": "Source", "inputs": [], "widgets_values": []},
                {"id": 2, "type": "KSampler", "inputs": [
                    {"name": "model", "link": 7},
                    {"name": "seed", "widget": {"name": "seed"}, "link": null}
                ], "widgets_values": [42]}
            ],
            "links": [[7, 1, 0, 2, 0, "MODEL"]]
        }))
        .unwrap();
        assert_eq!(prompt["2"]["class_type"], "KSampler");
        assert_eq!(prompt["2"]["inputs"]["model"], json!(["1", 0]));
        assert_eq!(prompt["2"]["inputs"]["seed"], 42);
    }

    #[test]
    fn omits_frontend_nodes_and_inlines_primitive_values() {
        let prompt = super::workflow_to_api_prompt(&json!({
            "nodes": [
                {"id": 1, "type": "PrimitiveNode", "widgets_values": [120],
                 "outputs": [{"links": [7]}]},
                {"id": 2, "type": "MarkdownNote", "widgets_values": ["note"]},
                {"id": 3, "type": "EmptyAceStep1.5LatentAudio", "inputs": [
                    {"name": "seconds", "link": 7}
                ]}
            ],
            "links": [[7, 1, 0, 3, 0, "FLOAT"]]
        }))
        .unwrap();
        assert!(prompt.get("1").is_none());
        assert!(prompt.get("2").is_none());
        assert_eq!(prompt["3"]["inputs"]["seconds"], 120);
    }

    #[test]
    fn identifies_visual_audio_generation_workflow() {
        assert!(is_runnable_audio_workflow(&json!({
            "nodes": [
                {"id": 1, "type": "EmptyAceStep1.5LatentAudio", "mode": 0},
                {"id": 2, "type": "KSamplerAceStepAura", "mode": 0}
            ]
        })));
    }

    #[test]
    fn rejects_training_workflow_even_when_it_contains_acestep_nodes() {
        assert!(!is_runnable_audio_workflow(&json!({
            "nodes": [
                {"id": 1, "type": "FL_AceStep_PreprocessDataset", "mode": 0},
                {"id": 2, "type": "FL_AceStep_LoRATrainer", "mode": 0},
                {"id": 3, "type": "KSampler", "mode": 0}
            ]
        })));
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
                    "id": 1,
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
                    "id": 2,
                    "type": "CheckpointLoaderSimple",
                    "inputs": [
                        {"name": "ckpt_name", "widget": {"name": "ckpt_name"}, "link": null}
                    ],
                    "widgets_values": ["model.safetensors"]
                },
                {
                    "id": 3,
                    "type": "LoraLoaderModelOnly",
                    "inputs": [
                        {"name": "model", "link": 2},
                        {"name": "lora_name", "widget": {"name": "lora_name"}, "link": null},
                        {"name": "strength_model", "widget": {"name": "strength_model"}, "link": null}
                    ],
                    "widgets_values": ["style.safetensors", 0.8],
                    "outputs": [{"name": "MODEL", "links": [10]}]
                },
                {
                    "id": 4,
                    "type": "KSampler",
                    "inputs": [{"name": "model", "link": 10}]
                }
            ],
            "links": [[10, 3, 0, 4, 0, "MODEL"]]
        }));

        assert_eq!(workflow.bpm, "130");
        assert_eq!(workflow.key, "E minor");
        assert_eq!(workflow.seed, "31");
        assert_eq!(workflow.prompt, "prompt");
        assert_eq!(workflow.lyrics, "lyrics");
        assert_eq!(workflow.model, "model.safetensors");
        assert_eq!(workflow.loras[0].filename, "style.safetensors");
        assert_eq!(workflow.loras[0].strength, "0.8");
    }

    #[test]
    fn extracts_ksampler_values_from_corrected_example_template() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../comfyui_templates/acestep-1-5-generate-with-lora.json"
        ))
        .unwrap();
        let workflow = parse_value(&value);

        assert_eq!(workflow.ksampler_steps, "8");
        assert_eq!(workflow.ksampler_cfg, "1");
        assert_eq!(workflow.seed, "33");
        assert_eq!(workflow.bpm, "140");
        assert_eq!(workflow.key, "D minor");
    }

    #[test]
    fn extracts_lora_from_descriptorless_model_only_node() {
        let workflow = parse_value(&json!({
            "nodes": [{
                "id": 106,
                "type": "LoraLoaderModelOnly",
                "inputs": [{"name": "model", "link": 291}],
                "widgets_values": ["/remote/models/lora9_rondoveneziano_500.safetensors", 1.2],
                "outputs": [{"name": "MODEL", "links": [292]}]
            }, {
                "id": 107,
                "type": "KSampler",
                "inputs": [{"name": "model", "link": 292}]
            }],
            "links": [[292, 106, 0, 107, 0, "MODEL"]]
        }));

        assert_eq!(workflow.loras[0].node_id, "106");
        assert_eq!(
            workflow.loras[0].filename,
            "lora9_rondoveneziano_500.safetensors"
        );
        assert_eq!(workflow.loras[0].strength, "1.2");
    }

    #[test]
    fn extracts_only_loras_on_the_chain_to_a_ksampler() {
        let workflow = parse_value(&json!({
            "1": {"class_type": "CheckpointLoaderSimple", "inputs": {
                "ckpt_name": "model.safetensors"
            }},
            "2": {"class_type": "Load LoRA", "inputs": {
                "model": ["1", 0], "lora_name": "connected.safetensors"
            }},
            "3": {"class_type": "Load LoRA", "inputs": {
                "model": ["1", 0], "lora_name": "bypassed.safetensors"
            }},
            "4": {"class_type": "KSampler", "inputs": {
                "model": ["2", 0]
            }}
        }));

        assert_eq!(
            workflow
                .loras
                .iter()
                .map(|lora| lora.filename.as_str())
                .collect::<Vec<_>>(),
            vec!["connected.safetensors"]
        );
    }

    #[test]
    fn excludes_bypassed_loras_but_keeps_enabled_upstream_loras() {
        let workflow = parse_value(&json!({
            "nodes": [
                {
                    "id": 1,
                    "type": "LoraLoaderModelOnly",
                    "inputs": [{"name": "model", "link": null}],
                    "widgets_values": ["enabled.safetensors", 1.0]
                },
                {
                    "id": 2,
                    "type": "LoraLoaderModelOnly",
                    "mode": 4,
                    "inputs": [{"name": "model", "link": 10}],
                    "widgets_values": ["bypassed.safetensors", 1.0],
                    "outputs": [{"name": "MODEL", "links": [11]}]
                },
                {
                    "id": 3,
                    "type": "KSampler",
                    "inputs": [{"name": "model", "link": 11}]
                }
            ],
            "links": [
                [10, 1, 0, 2, 0, "MODEL"],
                [11, 2, 0, 3, 0, "MODEL"]
            ]
        }));

        assert_eq!(
            workflow
                .loras
                .iter()
                .map(|lora| lora.filename.as_str())
                .collect::<Vec<_>>(),
            vec!["enabled.safetensors"]
        );
    }

    #[test]
    fn extracts_model_from_unet_loader_visual_node() {
        let workflow = parse_value(&json!({
            "nodes": [{
                "type": "UNETLoader",
                "inputs": [{
                    "name": "unet_name",
                    "widget": {"name": "unet_name"},
                    "link": 55
                }],
                "properties": {
                    "models": [{"name": "minimax_music3_dit_fp16.safetensors"}]
                },
                "widgets_values": ["minimax_music3_dit_fp16.safetensors", "default"]
            }]
        }));

        assert_eq!(workflow.model, "minimax_music3_dit_fp16.safetensors");
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
        assert_eq!(workflow.duration, "30");
        assert_eq!(workflow.key, "E minor");
        assert_eq!(workflow.seed, "32");
        assert_eq!(workflow.prompt, "prompt text");
        assert_eq!(workflow.lyrics, "[Verse] lyrics");
    }

    #[test]
    fn preserves_acestep_sampling_widget_order() {
        let prompt = super::workflow_to_api_prompt(&json!({
            "nodes": [{
                "id": 1,
                "type": "TextEncodeAceStepAudio1.5",
                "inputs": [
                    {"name": "clip", "link": 2},
                    {"name": "tags", "widget": {}, "link": null},
                    {"name": "lyrics", "widget": {}, "link": null},
                    {"name": "seed", "widget": {}, "link": null},
                    {"name": "bpm", "widget": {}, "link": null},
                    {"name": "duration", "widget": {}, "link": null},
                    {"name": "timesignature", "widget": {}, "link": null},
                    {"name": "language", "widget": {}, "link": null},
                    {"name": "keyscale", "widget": {}, "link": null},
                    {"name": "generate_audio_codes", "widget": {}, "link": null},
                    {"name": "cfg_scale", "widget": {}, "link": null},
                    {"name": "temperature", "widget": {}, "link": null},
                    {"name": "top_p", "widget": {}, "link": null},
                    {"name": "top_k", "widget": {}, "link": null},
                    {"name": "min_p", "widget": {}, "link": null}
                ],
                "widgets_values": [
                    "tags", "lyrics", "", "fixed", 117, 240, "4", "en",
                    "E minor", true, 2, 0.85, 0.9, 0, 0
                ]
            }, {
                "id": 2,
                "type": "CLIPLoader",
                "widgets_values": []
            }],
            "links": [[2, 2, 0, 1, 0, "CLIP"]]
        }))
        .unwrap();
        let inputs = &prompt["1"]["inputs"];
        assert_eq!(inputs["cfg_scale"], json!(2));
        assert_eq!(inputs["temperature"], json!(0.85));
        assert_eq!(inputs["top_p"], json!(0.9));
        assert_eq!(inputs["top_k"], json!(0));
        assert_eq!(inputs["min_p"], json!(0));
    }

    #[test]
    fn cached_parse_survives_memory_clear_and_invalidates_after_file_change() {
        let temp = TempDirectory::new();
        let path = temp.0.join("song.workflow.json");
        fs::write(
            &path,
            r#"{"1":{"class_type":"TextEncodeAceStepAudio1.5","inputs":{"bpm":100}}}"#,
        )
        .unwrap();

        let (first, first_hash) = parse_file_cached_with_hash(&temp.0, &path).unwrap();
        assert_eq!(first.bpm, "100");
        assert!(temp.0.join(".adbstudio/workflow-cache").is_dir());

        memory_cache().lock().unwrap().clear();
        let (from_disk, from_disk_hash) = parse_file_cached_with_hash(&temp.0, &path).unwrap();
        assert_eq!(from_disk, first);
        assert_eq!(from_disk_hash, first_hash);

        fs::write(
            &path,
            r#"{"1":{"class_type":"TextEncodeAceStepAudio1.5","inputs":{"bpm":1000}}}"#,
        )
        .unwrap();
        memory_cache().lock().unwrap().clear();
        let (changed, changed_hash) = parse_file_cached_with_hash(&temp.0, &path).unwrap();
        assert_eq!(changed.bpm, "1000");
        assert_ne!(changed_hash, first_hash);
    }
}
