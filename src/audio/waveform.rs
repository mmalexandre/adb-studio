use sha2::{Digest, Sha256};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream,
    meta::MetadataOptions, probe::Hint,
};

pub const PEAK_COUNT: usize = 4096;
pub const DISPLAY_PEAK_COUNT: usize = 160;
pub const RASTER_SCALE: usize = 3;
pub const RASTER_WIDTH: usize = DISPLAY_PEAK_COUNT * 4 * RASTER_SCALE;
pub const RASTER_HEIGHT: usize = 58 * RASTER_SCALE;

static CACHE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const RASTER_MAGIC: &[u8] = b"ADB-STUDIO-WAVEFORM-RASTER-2";

#[derive(serde::Deserialize, serde::Serialize)]
struct CacheEntry {
    checksum: String,
    peaks: Vec<f32>,
    #[serde(default)]
    bpm: f32,
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

pub fn raster_image(raster: &[u8]) -> Image {
    let mut buffer =
        SharedPixelBuffer::<Rgba8Pixel>::new(RASTER_WIDTH as u32, RASTER_HEIGHT as u32);
    for (pixel, alpha) in buffer
        .make_mut_slice()
        .iter_mut()
        .zip(raster.iter().take(RASTER_WIDTH * RASTER_HEIGHT))
    {
        *pixel = Rgba8Pixel {
            r: 255,
            g: 255,
            b: 255,
            a: *alpha,
        };
    }
    Image::from_rgba8(buffer)
}

pub fn load_or_generate_cancelable(
    path: &Path,
    workspace: &Path,
    should_cancel: impl Fn() -> bool,
) -> Option<(String, Vec<u8>, f32)> {
    let checksum = crate::metadata::checksum_for_file(workspace, path)
        .unwrap_or_else(|_| path.to_string_lossy().into_owned());
    let cache_key = cache_key(&checksum);
    let cache_path = cache_path(&cache_key, workspace);
    if let Ok(contents) = fs::read_to_string(&cache_path) {
        if let Ok(entry) = serde_json::from_str::<CacheEntry>(&contents) {
            if entry.checksum == checksum && !entry.peaks.is_empty() {
                let display_peaks = aggregate_peaks(&entry.peaks);
                let raster_path = raster_cache_path(&cache_key, workspace);
                let raster = load_raster_cache(&raster_path).unwrap_or_else(|| {
                    let raster = rasterize_peaks(&display_peaks);
                    write_raster_cache(&raster_path, &raster);
                    raster
                });
                if entry.bpm > 0.0 {
                    return Some((cache_key, raster, entry.bpm));
                }
            }
        }
    }

    if should_cancel() {
        return None;
    }
    let (peaks, bpm) = decode_peaks(path, &should_cancel).ok()?;
    if should_cancel() {
        return None;
    }
    write_cache(&cache_path, &checksum, &peaks, bpm);
    let raster = rasterize_peaks(&aggregate_peaks(&peaks));
    write_raster_cache(&raster_cache_path(&cache_key, workspace), &raster);
    Some((cache_key, raster, bpm))
}

fn cache_path(cache_key: &str, workspace: &Path) -> std::path::PathBuf {
    workspace
        .join(".adbstudio")
        .join("waveforms")
        .join(format!("{cache_key}.json"))
}

fn write_cache(path: &Path, checksum: &str, peaks: &[f32], bpm: f32) {
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let entry = CacheEntry {
        checksum: checksum.to_owned(),
        peaks: peaks.to_vec(),
        bpm,
    };
    if let Ok(contents) = serde_json::to_string(&entry) {
        let temp_path = path.with_extension(format!(
            "json.tmp-{}-{}",
            std::process::id(),
            CACHE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        if fs::write(&temp_path, contents).is_ok() {
            if fs::rename(&temp_path, path).is_err() {
                let _ = fs::remove_file(temp_path);
            }
        }
    }
}

fn raster_cache_path(cache_key: &str, workspace: &Path) -> std::path::PathBuf {
    workspace
        .join(".adbstudio")
        .join("waveforms")
        .join(format!("{cache_key}.raster"))
}

fn rasterize_peaks(peaks: &[f32]) -> Vec<u8> {
    let mut raster = vec![0; RASTER_WIDTH * RASTER_HEIGHT];
    let slot_width = RASTER_WIDTH / DISPLAY_PEAK_COUNT;
    for (index, peak) in peaks.iter().take(DISPLAY_PEAK_COUNT).enumerate() {
        let height = ((2.0 + peak.clamp(0.0, 1.0) * 25.0) * RASTER_SCALE as f32).round() as usize;
        let x_start = index * slot_width + slot_width / 2;
        let x_end = (index + 1) * slot_width;
        for y in RASTER_HEIGHT / 2 - height..RASTER_HEIGHT / 2 {
            for x in x_start..x_end {
                raster[y * RASTER_WIDTH + x] = 255;
            }
        }
        for y in RASTER_HEIGHT / 2..(RASTER_HEIGHT / 2 + height).min(RASTER_HEIGHT) {
            for x in x_start..x_end {
                raster[y * RASTER_WIDTH + x] = 128;
            }
        }
    }
    raster
}

fn load_raster_cache(path: &Path) -> Option<Vec<u8>> {
    let contents = fs::read(path).ok()?;
    let header_len = RASTER_MAGIC.len() + 8;
    if contents.len() < header_len || &contents[..RASTER_MAGIC.len()] != RASTER_MAGIC {
        return None;
    }
    let width_start = RASTER_MAGIC.len();
    let width = u32::from_le_bytes(contents[width_start..width_start + 4].try_into().ok()?);
    let height_start = width_start + 4;
    let height = u32::from_le_bytes(contents[height_start..height_start + 4].try_into().ok()?);
    if width != RASTER_WIDTH as u32 || height != RASTER_HEIGHT as u32 {
        return None;
    }
    let expected_len = header_len + RASTER_WIDTH * RASTER_HEIGHT;
    (contents.len() == expected_len).then(|| contents[header_len..].to_vec())
}

fn write_raster_cache(path: &Path, raster: &[u8]) {
    if raster.len() != RASTER_WIDTH * RASTER_HEIGHT {
        return;
    }
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut contents = Vec::with_capacity(RASTER_MAGIC.len() + 8 + raster.len());
    contents.extend_from_slice(RASTER_MAGIC);
    contents.extend_from_slice(&(RASTER_WIDTH as u32).to_le_bytes());
    contents.extend_from_slice(&(RASTER_HEIGHT as u32).to_le_bytes());
    contents.extend_from_slice(raster);
    let temp_path = path.with_extension(format!(
        "raster.tmp-{}-{}",
        std::process::id(),
        CACHE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if fs::write(&temp_path, contents).is_ok() && fs::rename(&temp_path, path).is_err() {
        let _ = fs::remove_file(temp_path);
    }
}

fn cache_key(checksum: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(PEAK_COUNT.to_le_bytes());
    hasher.update(checksum.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn decode_peaks(
    path: &Path,
    should_cancel: &impl Fn() -> bool,
) -> Result<(Vec<f32>, f32), Box<dyn std::error::Error>> {
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
    let sample_rate = codec_params.sample_rate.unwrap_or(44_100) as f32;
    let channels = codec_params.channels.map(|value| value.count()).unwrap_or(1);

    while let Ok(packet) = format.next_packet() {
        if should_cancel() {
            return Err("waveform generation cancelled".into());
        }
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
        return Ok((Vec::new(), 0.0));
    }
    let bucket_size = (samples.len() / PEAK_COUNT).max(1);
    let bpm = estimate_bpm(&samples, sample_rate, channels);
    Ok((
        (0..PEAK_COUNT)
        .map(|bucket| {
            samples
                .iter()
                .skip(bucket * bucket_size)
                .take(bucket_size)
                .copied()
                .fold(0.0, f32::max)
        })
        .collect(),
        bpm,
    ))
}

fn estimate_bpm(samples: &[f32], sample_rate: f32, channels: usize) -> f32 {
    if samples.len() < channels * 2048 || sample_rate <= 0.0 || channels == 0 {
        return 0.0;
    }
    let frame_count = samples.len() / channels;
    let frames_per_bin = 1024;
    let envelope = (0..frame_count / frames_per_bin)
        .map(|bin| {
            let start = bin * frames_per_bin * channels;
            let end = (start + frames_per_bin * channels).min(samples.len());
            samples[start..end].iter().copied().sum::<f32>() / (end - start) as f32
        })
        .collect::<Vec<_>>();
    let mean = envelope.iter().sum::<f32>() / envelope.len() as f32;
    let envelope = envelope
        .into_iter()
        .map(|value| (value - mean).max(0.0))
        .collect::<Vec<_>>();
    let envelope_rate = sample_rate / frames_per_bin as f32;
    let min_lag = (envelope_rate * 60.0 / 180.0).round() as usize;
    let max_lag = (envelope_rate * 60.0 / 60.0).round() as usize;
    let mut best_lag = 0;
    let mut best_score = 0.0;
    for lag in min_lag..=max_lag.min(envelope.len().saturating_sub(1)) {
        let score = envelope
            .iter()
            .skip(lag)
            .zip(envelope.iter())
            .map(|(current, previous)| current * previous)
            .sum::<f32>();
        if score > best_score {
            best_score = score;
            best_lag = lag;
        }
    }
    (best_lag > 0 && best_score > 0.0)
        .then(|| (60.0 * envelope_rate / best_lag as f32).round())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::{
        aggregate_peaks, load_or_generate_cancelable, load_raster_cache, rasterize_peaks,
        write_raster_cache, DISPLAY_PEAK_COUNT, RASTER_HEIGHT, RASTER_WIDTH,
    };
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
            let path = std::env::temp_dir().join(format!("adb-studio-waveform-{suffix}"));
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
    fn empty_input_produces_zero_display_peaks() {
        let peaks = aggregate_peaks(&[]);

        assert_eq!(peaks.len(), DISPLAY_PEAK_COUNT);
        assert!(peaks.iter().all(|peak| *peak == 0.0));
    }

    #[test]
    fn aggregation_preserves_bucket_maxima() {
        let mut source = vec![0.0; DISPLAY_PEAK_COUNT * 2];
        source[1] = 0.5;
        source[DISPLAY_PEAK_COUNT + 1] = 0.75;

        let display = aggregate_peaks(&source);

        assert_eq!(display[0], 0.5);
        assert_eq!(display[DISPLAY_PEAK_COUNT / 2], 0.75);
    }

    #[test]
    fn raster_contains_expected_layers_and_dimensions() {
        let mut peaks = vec![0.0; DISPLAY_PEAK_COUNT];
        peaks[0] = 1.0;

        let raster = rasterize_peaks(&peaks);

        assert_eq!(raster.len(), RASTER_WIDTH * RASTER_HEIGHT);
        assert_eq!(raster[6 * RASTER_WIDTH + 6], 255);
        assert_eq!(raster[86 * RASTER_WIDTH + 6], 255);
        assert_eq!(raster[87 * RASTER_WIDTH + 6], 128);
        assert_eq!(raster[167 * RASTER_WIDTH + 6], 128);
        assert_eq!(raster[5 * RASTER_WIDTH + 6], 0);
    }

    #[test]
    fn raster_cache_round_trips_with_dimensions() {
        let temp = TempDirectory::new();
        let path = temp.0.join("waveform.raster");
        let raster = vec![64; RASTER_WIDTH * RASTER_HEIGHT];

        write_raster_cache(&path, &raster);

        assert_eq!(load_raster_cache(&path), Some(raster));
    }

    #[test]
    fn cancellation_prevents_generation_and_cache_creation() {
        let temp = TempDirectory::new();
        let source = temp.0.join("missing.wav");
        let result = load_or_generate_cancelable(&source, &temp.0, || true);

        assert_eq!(result, None);
        assert!(!temp.0.join(".adbstudio/waveforms").exists());
    }

    #[test]
    fn failed_decode_is_not_cached_as_empty_peaks() {
        let temp = TempDirectory::new();
        let source = temp.0.join("missing.wav");
        assert_eq!(
            load_or_generate_cancelable(&source, &temp.0, || false),
            None
        );
        assert!(!temp.0.join(".adbstudio/waveforms").exists());
    }
}
