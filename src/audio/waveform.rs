use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream,
    meta::MetadataOptions, probe::Hint,
};

pub const PEAK_COUNT: usize = 4096;
pub const DISPLAY_PEAK_COUNT: usize = 160;

#[derive(serde::Deserialize, serde::Serialize)]
struct CacheEntry {
    source_path: String,
    peaks: Vec<f32>,
}

pub fn aggregate_peaks(peaks: &[f32]) -> Vec<f32> {
    if peaks.is_empty() {
        return vec![0.0; DISPLAY_PEAK_COUNT];
    }
    (0..DISPLAY_PEAK_COUNT)
        .map(|bucket| {
            let start = bucket * peaks.len() / DISPLAY_PEAK_COUNT;
            let end = ((bucket + 1) * peaks.len() / DISPLAY_PEAK_COUNT).max(start + 1);
            peaks[start..end.min(peaks.len())]
                .iter()
                .copied()
                .fold(0.0, f32::max)
        })
        .collect()
}

pub fn load_or_generate(path: &Path, workspace: &Path) -> (String, Vec<f32>) {
    let source_path = relative_source_path(path, workspace);
    let cache_key = cache_key(path, &source_path);
    let cache_path = cache_path(&cache_key, workspace);
    if let Ok(contents) = fs::read_to_string(&cache_path) {
        if let Ok(entry) = serde_json::from_str::<CacheEntry>(&contents) {
            if entry.source_path == source_path {
                return (cache_key, entry.peaks);
            }
        }
    }

    let peaks = decode_peaks(path).unwrap_or_default();
    write_cache(&cache_path, &source_path, &peaks);
    (cache_key, peaks)
}

fn cache_path(cache_key: &str, workspace: &Path) -> std::path::PathBuf {
    workspace
        .join(".adbstudio")
        .join("waveforms")
        .join(format!("{cache_key}.json"))
}

fn write_cache(path: &Path, source_path: &str, peaks: &[f32]) {
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let entry = CacheEntry {
        source_path: source_path.to_owned(),
        peaks: peaks.to_vec(),
    };
    if let Ok(contents) = serde_json::to_string(&entry) {
        let _ = fs::write(path, contents);
    }
}

fn relative_source_path(path: &Path, workspace: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn cache_key(path: &Path, source_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(PEAK_COUNT.to_le_bytes());
    hasher.update(source_path.as_bytes());
    if let Ok(contents) = fs::read(path) {
        hasher.update(contents);
    } else {
        hasher.update(path.to_string_lossy().as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn decode_peaks(path: &Path) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let file = fs::File::open(path)?;
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }
    let probed = symphonia::default::get_probe().format(
        &hint,
        source,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("audio has no default track")?;
    let codec_params = track.codec_params.clone();
    let track_id = track.id;
    let mut decoder =
        symphonia::default::get_codecs().make(&codec_params, &DecoderOptions::default())?;
    let mut samples = Vec::new();

    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet)?;
        let spec = *decoded.spec();
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        samples.extend(buffer.samples().iter().copied().map(f32::abs));
    }
    if samples.is_empty() {
        return Ok(Vec::new());
    }
    let bucket_size = (samples.len() / PEAK_COUNT).max(1);
    Ok((0..PEAK_COUNT)
        .map(|bucket| {
            samples
                .iter()
                .skip(bucket * bucket_size)
                .take(bucket_size)
                .copied()
                .fold(0.0, f32::max)
        })
        .collect())
}
