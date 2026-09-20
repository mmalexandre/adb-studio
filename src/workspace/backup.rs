use chrono::{DateTime, Duration};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};
use zip::{write::SimpleFileOptions, ZipWriter};

const BACKUP_DIR: &str = ".adbstudio/workspace-backups";
const MAX_BACKUPS: usize = 10;
const BACKUP_INTERVAL: Duration = Duration::days(1);

pub fn ensure_recent_backup(workspace: &Path) -> io::Result<()> {
    let backup_dir = workspace.join(BACKUP_DIR);
    fs::create_dir_all(&backup_dir)?;
    let mut backups = backup_files(&backup_dir)?;
    backups.sort_by(|left, right| right.1.cmp(&left.1));

    let now = SystemTime::now();
    if backups
        .first()
        .and_then(|(_, modified)| now.duration_since(*modified).ok())
        .is_some_and(|age| age < BACKUP_INTERVAL.to_std().unwrap())
    {
        rotate_backups(&backups)?;
        return Ok(());
    }

    let timestamp = DateTime::<chrono::Utc>::from(now).format("%Y%m%d-%H%M%S");
    let destination = backup_dir.join(format!("workspace-{timestamp}.zip"));
    let temporary = backup_dir.join(format!(
        ".workspace-{timestamp}-{}.zip.tmp",
        std::process::id()
    ));
    if let Err(error) = create_backup(workspace, &temporary) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    backups.push((destination, now));
    backups.sort_by(|left, right| right.1.cmp(&left.1));
    rotate_backups(&backups)
}

fn create_backup(workspace: &Path, destination: &Path) -> io::Result<()> {
    let file = File::create(destination)?;
    let mut archive = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut checksums = Vec::new();
    add_directory(workspace, workspace, &mut archive, options, &mut checksums)?;
    checksums.sort_by(|left, right| left.0.cmp(&right.0));
    archive.start_file("CHECKSUMS.sha256", options)?;
    for (path, checksum) in checksums {
        writeln!(archive, "{checksum}  {path}")?;
    }
    archive.finish()?;
    Ok(())
}

fn add_directory(
    workspace: &Path,
    directory: &Path,
    archive: &mut ZipWriter<File>,
    options: SimpleFileOptions,
    checksums: &mut Vec<(String, String)>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(workspace).map_err(io::Error::other)?;
        if should_skip(relative) {
            continue;
        }
        if path.is_dir() {
            add_directory(workspace, &path, archive, options, checksums)?;
        } else if path.is_file() {
            let archive_path = relative.to_string_lossy().replace('\\', "/");
            let mut input = File::open(&path)?;
            let mut contents = Vec::new();
            input.read_to_end(&mut contents)?;
            let checksum = format!("{:x}", Sha256::digest(&contents));
            archive.start_file(&archive_path, options)?;
            archive.write_all(&contents)?;
            checksums.push((archive_path, checksum));
        }
    }
    Ok(())
}

fn should_skip(relative: &Path) -> bool {
    let mut components = relative.components();
    let Some(first_component) = components
        .next()
        .and_then(|component| component.as_os_str().to_str())
    else {
        return false;
    };
    if first_component != ".adbstudio" {
        return true;
    }
    match components
        .next()
        .and_then(|component| component.as_os_str().to_str())
    {
        Some("waveforms") | Some("workspace-backups") => true,
        _ => false,
    }
}

fn backup_files(directory: &Path) -> io::Result<Vec<(PathBuf, SystemTime)>> {
    Ok(fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|extension| extension.to_str()) == Some("zip"))
                .then(|| Some((path.clone(), fs::metadata(path).ok()?.modified().ok()?)))
                .flatten()
        })
        .collect())
}

fn rotate_backups(backups: &[(PathBuf, SystemTime)]) -> io::Result<()> {
    for (path, _) in backups.iter().skip(MAX_BACKUPS) {
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ensure_recent_backup, BACKUP_DIR};
    use std::{fs, io::Read, time::Duration};
    use zip::ZipArchive;

    fn temp_workspace(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("adb-studio-backup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn backup_keeps_adbstudio_metadata_only() {
        let workspace = temp_workspace("contents");
        fs::write(workspace.join("song.wav"), b"audio").unwrap();
        fs::write(workspace.join("model.safetensors"), b"model").unwrap();
        fs::write(workspace.join("adapter.safetensors"), b"lora").unwrap();
        fs::create_dir_all(workspace.join(".adbstudio.bak")).unwrap();
        fs::write(workspace.join(".adbstudio.bak/old-metadata.json"), b"old").unwrap();
        fs::create_dir_all(workspace.join(".adbstudio")).unwrap();
        fs::write(workspace.join(".adbstudio/metadata.json"), b"metadata").unwrap();
        fs::create_dir_all(workspace.join("workflows")).unwrap();
        fs::write(workspace.join("workflows/song.workflow.json"), b"workflow").unwrap();
        fs::create_dir_all(workspace.join(".adbstudio/waveforms")).unwrap();
        fs::write(workspace.join(".adbstudio/waveforms/cache.json"), b"cache").unwrap();
        fs::create_dir_all(workspace.join(".adbstudio/workspace-backups")).unwrap();
        ensure_recent_backup(&workspace).unwrap();

        let backup = fs::read_dir(workspace.join(BACKUP_DIR))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let file = fs::File::open(backup).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        assert!(archive.by_name("song.wav").is_err());
        assert!(archive.by_name("model.safetensors").is_err());
        assert!(archive.by_name("adapter.safetensors").is_err());
        assert!(archive.by_name(".adbstudio.bak/old-metadata.json").is_err());
        assert!(archive.by_name(".adbstudio/metadata.json").is_ok());
        assert!(archive.by_name("workflows/song.workflow.json").is_err());
        assert!(archive.by_name(".adbstudio/waveforms/cache.json").is_err());
        let mut checksums = String::new();
        archive
            .by_name("CHECKSUMS.sha256")
            .unwrap()
            .read_to_string(&mut checksums)
            .unwrap();
        assert!(!checksums.contains("song.wav"));
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn recent_backup_is_not_recreated() {
        let workspace = temp_workspace("daily");
        ensure_recent_backup(&workspace).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        ensure_recent_backup(&workspace).unwrap();
        assert_eq!(fs::read_dir(workspace.join(BACKUP_DIR)).unwrap().count(), 1);
        let _ = fs::remove_dir_all(workspace);
    }
}
