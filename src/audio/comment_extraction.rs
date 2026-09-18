use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub fn destination(source: &Path, start_seconds: f32, end_seconds: f32) -> Result<PathBuf, String> {
    validate_range(start_seconds, end_seconds)?;
    let stem = source
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "Source file has no usable name".to_string())?;
    let output_directory = source
        .parent()
        .ok_or_else(|| "Source file has no parent directory".to_string())?
        .join("extracted");
    fs::create_dir_all(&output_directory)
        .map_err(|error| format!("Could not create extracted directory: {error}"))?;
    Ok(output_directory.join(format!("{stem}_{start_seconds:.3}-{end_seconds:.3}.wav")))
}

pub fn validate_range(start_seconds: f32, end_seconds: f32) -> Result<(), String> {
    if !start_seconds.is_finite() || !end_seconds.is_finite() || end_seconds <= start_seconds {
        return Err("Cannot extract a zero-length comment".into());
    }
    Ok(())
}

pub fn extract(
    source: &Path,
    destination: &Path,
    start_seconds: f32,
    end_seconds: f32,
) -> Result<(), String> {
    validate_range(start_seconds, end_seconds)?;
    if !source.is_file() {
        return Err("Source audio file does not exist".into());
    }
    let status = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-ss"])
        .arg(start_seconds.to_string())
        .arg("-to")
        .arg(end_seconds.to_string())
        .arg("-i")
        .arg(source)
        .args(["-vn", "-c:a", "pcm_s16le"])
        .arg(destination)
        .status()
        .map_err(|error| format!("Could not start ffmpeg: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::validate_range;

    #[test]
    fn rejects_zero_length_and_invalid_ranges() {
        assert!(validate_range(1.0, 1.0).is_err());
        assert!(validate_range(2.0, 1.0).is_err());
        assert!(validate_range(f32::NAN, 1.0).is_err());
        assert!(validate_range(1.0, 2.0).is_ok());
    }
}
