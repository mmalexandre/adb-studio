use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
        mpsc::Sender,
        Arc, Mutex,
    },
    thread,
};

use crate::metadata;

use super::waveform;

pub struct State {
    pub folder: PathBuf,
    pub paths: Vec<PathBuf>,
    pub requested_range: Option<(usize, usize)>,
    pub generated: HashSet<PathBuf>,
    pub loading: HashSet<PathBuf>,
    pub generation: u64,
    pub cancellation_generation: Arc<AtomicU64>,
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

pub fn request(state: &Arc<Mutex<State>>, start_index: usize, visible_rows: usize) {
    let mut state_ref = state.lock().unwrap();
    let start = start_index;
    let end = start_index
        .saturating_add(visible_rows)
        .min(state_ref.paths.len());
    let requested_range = (start, end);
    if state_ref.requested_range == Some(requested_range) {
        return;
    }
    state_ref.generation += 1;
    state_ref
        .cancellation_generation
        .store(state_ref.generation, Ordering::Release);
    state_ref.requested_range = Some(requested_range);
    let paths_to_load = state_ref.paths[start..end].to_vec();
    let generated = state_ref.generated.clone();
    state_ref
        .loading
        .retain(|path| paths_to_load.contains(path));
    state_ref.loading.extend(
        paths_to_load
            .into_iter()
            .filter(|path| !generated.contains(path)),
    );
    if state_ref.running {
        return;
    }
    state_ref.running = true;
    let shared_state = Arc::clone(state);
    thread::spawn(move || generate(shared_state));
}

fn generate(state: Arc<Mutex<State>>) {
    loop {
        let (folder, paths, requested_range, idle_jobs, generation) = {
            let mut state_ref = state.lock().unwrap();
            let paths = state_ref.paths.clone();
            let requested_range = state_ref.requested_range.take();
            let idle_jobs = if requested_range.is_none() {
                let idle_jobs = paths
                    .iter()
                    .enumerate()
                    .filter(|(_, path)| {
                        !state_ref.generated.contains(*path) && !state_ref.loading.contains(*path)
                    })
                    .map(|(index, path)| (index, path.clone()))
                    .collect::<Vec<_>>();
                state_ref
                    .loading
                    .extend(idle_jobs.iter().map(|(_, path)| path.clone()));
                idle_jobs
            } else {
                Vec::new()
            };
            (
                state_ref.folder.clone(),
                paths,
                requested_range,
                idle_jobs,
                state_ref.generation,
            )
        };
        let jobs = if let Some(range) = requested_range {
            let end = range.1.min(paths.len());
            (range.0.min(end)..end)
                .filter_map(|index| {
                    let path = paths[index].clone();
                    let state_ref = state.lock().unwrap();
                    (state_ref.generation == generation
                        && !state_ref.generated.contains(&path)
                        && state_ref.loading.contains(&path))
                    .then_some((index, path))
                })
                .collect::<Vec<_>>()
        } else {
            idle_jobs
        };
        let worker_count = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1)
            .min(jobs.len().max(1));
        if jobs.is_empty() {
            let mut state_ref = state.lock().unwrap();
            if state_ref.requested_range.is_none() {
                state_ref.running = false;
                return;
            }
            continue;
        }
        let job_count = jobs.len();
        let jobs = Arc::new(Mutex::new(VecDeque::from(jobs)));
        let (worker_sender, worker_receiver) = mpsc::channel();
        let worker_receiver = Arc::new(Mutex::new(worker_receiver));
        let cancellation_generation = state.lock().unwrap().cancellation_generation.clone();
        let mut workers = Vec::new();
        for _ in 0..worker_count {
            let jobs = Arc::clone(&jobs);
            let worker_sender = worker_sender.clone();
            let folder = folder.clone();
            let cancellation_generation = Arc::clone(&cancellation_generation);
            workers.push(thread::spawn(move || loop {
                if cancellation_generation.load(Ordering::Acquire) != generation {
                    return;
                }
                let Some((index, path)) = jobs.lock().unwrap().pop_front() else {
                    return;
                };
                let result = waveform::load_or_generate_cancelable(&path, &folder, || {
                    cancellation_generation.load(Ordering::Acquire) != generation
                });
                if worker_sender.send((index, path, result)).is_err() {
                    return;
                }
            }));
        }
        drop(worker_sender);
        for _ in 0..job_count {
            let Ok((index, path, result)) = worker_receiver.lock().unwrap().recv() else {
                break;
            };
            let Some((cache_key, peaks)) = result else {
                continue;
            };
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
            state_ref.loading.remove(&path);
            state_ref.generated.insert(path);
            state_ref.completed += 1;
            let _ = state_ref.result_sender.send(Result {
                generation,
                index,
                path: path_string,
                peaks,
            });
        }
        for worker in workers {
            let _ = worker.join();
        }
    }
}
