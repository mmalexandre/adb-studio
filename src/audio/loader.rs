use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{mpsc::Sender, Arc, Mutex},
    thread,
};

use crate::metadata;

use super::waveform;

pub const PREFETCH_ROWS: usize = 32;
pub const PREFETCH_BEFORE: usize = 4;

pub struct State {
    pub folder: PathBuf,
    pub paths: Vec<PathBuf>,
    pub requested_range: Option<(usize, usize)>,
    pub generated: HashSet<PathBuf>,
    pub generation: u64,
    pub running: bool,
    pub completed: usize,
    pub total: usize,
    pub result_sender: Sender<Result>,
}

pub struct Result {
    pub generation: u64,
    pub index: usize,
    pub path: String,
    pub peaks: Vec<f32>,
}

pub fn request(state: &Arc<Mutex<State>>, start_index: usize) {
    let mut state_ref = state.lock().unwrap();
    let start = start_index.saturating_sub(PREFETCH_BEFORE);
    let end = start_index
        .saturating_add(PREFETCH_ROWS)
        .min(state_ref.paths.len());
    state_ref.requested_range = Some((start, end));
    if state_ref.running {
        return;
    }
    state_ref.running = true;
    let shared_state = Arc::clone(state);
    thread::spawn(move || generate(shared_state));
}

fn generate(state: Arc<Mutex<State>>) {
    loop {
        let (folder, paths, range, generation) = {
            let mut state_ref = state.lock().unwrap();
            let Some(range) = state_ref.requested_range.take() else {
                state_ref.running = false;
                return;
            };
            (
                state_ref.folder.clone(),
                state_ref.paths.clone(),
                range,
                state_ref.generation,
            )
        };
        let end = range.1.min(paths.len());
        for index in range.0.min(end)..end {
            let path = paths[index].clone();
            {
                let state_ref = state.lock().unwrap();
                if state_ref.generation != generation || state_ref.generated.contains(&path) {
                    continue;
                }
            }
            let (cache_key, peaks) = waveform::load_or_generate(&path, &folder);
            let mut index_data = metadata::load_index(&folder);
            let path_string = path.to_string_lossy().into_owned();
            if let Some(stored) = index_data
                .audio_files
                .iter_mut()
                .find(|item| item.file_path == path_string)
            {
                stored.waveform_cache_key = cache_key;
            } else {
                index_data.audio_files.push(metadata::AudioFileMetadata {
                    file_path: path_string.clone(),
                    waveform_cache_key: cache_key,
                    ..Default::default()
                });
            }
            metadata::save_index(&folder, &index_data);
            let mut state_ref = state.lock().unwrap();
            if state_ref.generation != generation {
                continue;
            }
            state_ref.generated.insert(path);
            state_ref.completed += 1;
            let _ = state_ref.result_sender.send(Result {
                generation,
                index,
                path: path_string,
                peaks,
            });
        }
    }
}