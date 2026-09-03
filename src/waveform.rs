use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use symphonia::core::{audio::SampleBuffer, codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions, probe::Hint};

pub const PEAK_COUNT: usize = 4096;

pub fn load_or_generate(path: &Path, workspace: &Path) -> (String, Vec<f32>) {
    let cache_key = cache_key(path);
    let cache_path = workspace.join(".adbstudio").join("waveforms").join(format!("{cache_key}.json"));
    if let Ok(contents) = fs::read_to_string(&cache_path) {
        if let Ok(peaks) = serde_json::from_str::<Vec<f32>>(&contents) {
            return (cache_key, peaks);
        }
    }

    let peaks = decode_peaks(path).unwrap_or_default();
    if let Some(parent) = cache_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(contents) = serde_json::to_string(&peaks) {
        let _ = fs::write(cache_path, contents);
    }
    (cache_key, peaks)
}

fn cache_key(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(PEAK_COUNT.to_le_bytes());
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
    let probed = symphonia::default::get_probe().format(&hint, source, &FormatOptions::default(), &MetadataOptions::default())?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("audio has no default track")?;
    let codec_params = track.codec_params.clone();
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs().make(&codec_params, &DecoderOptions::default())?;
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
        .map(|bucket| samples.iter().skip(bucket * bucket_size).take(bucket_size).copied().fold(0.0, f32::max))
        .collect())
}