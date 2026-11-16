//! Safety backups and archive handling.
//!
//! Before SaveVault overwrites or removes live save files (restoring a snapshot, switching
//! profile, downloading newer cloud saves, importing a file) it zips the current files into a
//! local safety folder so any such action can be undone. Also hosts the zip / tar readers
//! used for imports, which never write outside their destination.

use crate::chunker::normalize_rel_path;
use crate::models::{AppConfig, GameEntry, SaveRoot};
use crate::sync_engine::collect_root_files;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, Utc};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const BACKUP_INFO_FILE: &str = "savevault_backup.json";

/// Description stored inside every safety backup / exported bundle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupInfo {
    pub game_id: String,
    pub game_name: String,
    #[serde(default)]
    pub profile: String,
    pub created_at: DateTime<Utc>,
    pub reason: String,
    #[serde(default)]
    pub machine: String,
    /// Root ids contained in the archive.
    #[serde(default)]
    pub roots: Vec<String>,
}

/// One safety backup on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyBackup {
    pub path: PathBuf,
    pub created: DateTime<Local>,
    pub reason: String,
    pub bytes: u64,
}

/// Folder that holds safety backups (next to the local repository clone).
pub fn safety_root(config: &AppConfig) -> PathBuf {
    config
        .local_repo_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config.local_repo_path.clone())
        .join("safety_backups")
}

fn game_safety_dir(config: &AppConfig, game: &GameEntry) -> PathBuf {
    safety_root(config).join(game.id.replace([':', '/', '\\'], "_"))
}

fn slug(text: &str) -> String {
    let s: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

fn zip_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true)
}

/// Writes the files of `roots` into a zip at `dst` (layout `<root id>/<relative path>`).
/// Returns the number of files written.
pub fn write_roots_zip(
    dst: &Path,
    roots: &[SaveRoot],
    game_excludes: &[String],
    global_excludes: &[String],
    info: &BackupInfo,
) -> Result<usize> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = dst.with_extension("zip.part");
    let mut count = 0usize;
    {
        let file = File::create(&tmp).with_context(|| format!("Failed to create {}", tmp.display()))?;
        let mut zip = ZipWriter::new(BufWriter::new(file));
        let mut info = info.clone();
        info.roots = roots.iter().map(|r| r.id.clone()).collect();
        zip.start_file(BACKUP_INFO_FILE, zip_options())?;
        zip.write_all(serde_json::to_string_pretty(&info)?.as_bytes())?;

        for root in roots {
            for rel in collect_root_files(root, game_excludes, global_excludes)? {
                let src = root.path.join(&rel);
                let Ok(mut f) = File::open(&src) else {
                    continue; // vanished while we were walking
                };
                zip.start_file(format!("{}/{}", root.id, normalize_rel_path(&rel)), zip_options())?;
                io::copy(&mut f, &mut zip)?;
                count += 1;
            }
        }
        zip.finish()?.flush()?;
    }
    if count == 0 {
        let _ = fs::remove_file(&tmp);
        return Ok(0);
    }
    fs::rename(&tmp, dst)?;
    Ok(count)
}

/// Zips the game's live files for the active profile. Returns `None` when there is nothing
/// to protect. Old backups beyond `safety_backup_keep` are pruned.
pub fn create_safety_backup(config: &AppConfig, game: &GameEntry, reason: &str) -> Result<Option<PathBuf>> {
    if !config.safety_backups {
        return Ok(None);
    }
    let roots: Vec<SaveRoot> = game
        .active_roots()
        .into_iter()
        .filter(|r| r.path.is_dir())
        .collect();
    if roots.is_empty() {
        return Ok(None);
    }
    let dir = game_safety_dir(config, game);
    let stamp = Local::now().format("%Y%m%d-%H%M%S%3f");
    let dst = dir.join(format!("{stamp}_{}.zip", slug(reason)));
    let info = BackupInfo {
        game_id: game.id.clone(),
        game_name: game.name.clone(),
        profile: game.active_profile.clone(),
        created_at: Utc::now(),
        reason: reason.to_string(),
        machine: config.machine_name.clone(),
        roots: Vec::new(),
    };
    let written = write_roots_zip(&dst, &roots, &game.exclude_globs, &config.global_exclude_globs, &info)?;
    if written == 0 {
        return Ok(None);
    }
    prune_safety_backups(&dir, config.safety_backup_keep.max(1) as usize);
    Ok(Some(dst))
}

fn prune_safety_backups(dir: &Path, keep: usize) {
    let mut zips: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("zip"))
        .collect();
    zips.sort();
    while zips.len() > keep {
        let _ = fs::remove_file(zips.remove(0));
    }
}

/// Lists a game's safety backups, newest first.
pub fn list_safety_backups(config: &AppConfig, game: &GameEntry) -> Vec<SafetyBackup> {
    let mut out: Vec<SafetyBackup> = fs::read_dir(game_safety_dir(config, game))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("zip") {
                return None;
            }
            let meta = e.metadata().ok()?;
            let stem = path.file_stem()?.to_str()?.to_string();
            let reason = stem
                .split_once('_')
                .map(|(_, r)| r.replace('-', " "))
                .unwrap_or_default();
            Some(SafetyBackup {
                created: meta.modified().ok()?.into(),
                bytes: meta.len(),
                reason,
                path,
            })
        })
        .collect();
    out.sort_by(|a, b| b.path.cmp(&a.path));
    out
}

/// Reads the description embedded in a SaveVault zip, if present.
pub fn read_backup_info(zip_path: &Path) -> Option<BackupInfo> {
    let file = File::open(zip_path).ok()?;
    let mut archive = ZipArchive::new(BufReader::new(file)).ok()?;
    let mut entry = archive.by_name(BACKUP_INFO_FILE).ok()?;
    let mut raw = String::new();
    entry.read_to_string(&mut raw).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Puts the files from a SaveVault zip back into the game's tracked folders (overwriting
/// same-named files, leaving others alone). Returns the number of files written.
pub fn restore_roots_zip(zip_path: &Path, roots: &[SaveRoot]) -> Result<usize> {
    let file = File::open(zip_path).with_context(|| format!("Failed to open {}", zip_path.display()))?;
    let mut archive = ZipArchive::new(BufReader::new(file)).context("Not a valid zip archive")?;
    let mut count = 0usize;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let mut comps = name.components();
        let Some(Component::Normal(root_id)) = comps.next() else {
            continue;
        };
        let rel: PathBuf = comps.collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        let Some(root) = roots.iter().find(|r| root_id.to_str() == Some(r.id.as_str())) else {
            continue;
        };
        if root.path.as_os_str().is_empty() {
            continue;
        }
        let target = root.path.join(&rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&target)
            .with_context(|| format!("Failed to write {}", target.display()))?;
        io::copy(&mut entry, &mut out)?;
        count += 1;
    }
    Ok(count)
}

fn safe_relative(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() { None } else { Some(out) }
}

/// Extracts a zip into `dst`, refusing entries that would land outside it.
pub fn extract_zip_safely(archive_path: &Path, dst: &Path) -> Result<usize> {
    let file = File::open(archive_path)
        .with_context(|| format!("Failed to open {}", archive_path.display()))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .with_context(|| format!("{} is not a valid zip archive", archive_path.display()))?;
    fs::create_dir_all(dst)?;
    let mut count = 0usize;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name().and_then(|p| safe_relative(&p)) else {
            continue;
        };
        let target = dst.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if !entry.is_file() {
            continue; // symlinks and other special entries are not save data
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&target)
            .with_context(|| format!("Failed to write {}", target.display()))?;
        io::copy(&mut entry, &mut out)?;
        count += 1;
    }
    Ok(count)
}

/// Extracts a `.tar`, `.tar.gz` or `.tgz` into `dst`, refusing entries that would land
/// outside it and skipping links / devices.
pub fn extract_tar_safely(archive_path: &Path, dst: &Path) -> Result<usize> {
    let file = File::open(archive_path)
        .with_context(|| format!("Failed to open {}", archive_path.display()))?;
    let lower = archive_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    let reader: Box<dyn Read> = if lower.ends_with(".gz") || lower.ends_with(".tgz") {
        Box::new(GzDecoder::new(BufReader::new(file)))
    } else {
        Box::new(BufReader::new(file))
    };
    fs::create_dir_all(dst)?;
    let mut archive = tar::Archive::new(reader);
    let mut count = 0usize;
    for entry in archive
        .entries()
        .with_context(|| format!("{} is not a valid tar archive", archive_path.display()))?
    {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        let Some(rel) = safe_relative(&entry.path()?) else {
            continue;
        };
        let target = dst.join(&rel);
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
        } else if kind.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = File::create(&target)
                .with_context(|| format!("Failed to write {}", target.display()))?;
            io::copy(&mut entry, &mut out)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Extracts any supported archive (`.zip`, `.tar`, `.tar.gz`, `.tgz`).
pub fn extract_archive(archive_path: &Path, dst: &Path) -> Result<usize> {
    let lower = archive_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    if lower.ends_with(".zip") {
        extract_zip_safely(archive_path, dst)
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") || lower.ends_with(".tar") {
        extract_tar_safely(archive_path, dst)
    } else {
        bail!("Unsupported archive type: {}", archive_path.display());
    }
}

/// True for file names `extract_archive` understands.
pub fn is_supported_archive(path: &Path) -> bool {
    let lower = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    lower.ends_with(".zip") || lower.ends_with(".tar.gz") || lower.ends_with(".tgz") || lower.ends_with(".tar")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn zip_extraction_never_escapes_destination() {
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("evil.zip");
        {
            let mut zip = ZipWriter::new(File::create(&zip_path).unwrap());
            zip.start_file("../outside.txt", zip_options()).unwrap();
            zip.write_all(b"nope").unwrap();
            zip.start_file("/abs.txt", zip_options()).unwrap();
            zip.write_all(b"nope").unwrap();
            zip.start_file("saves/slot1.sav", zip_options()).unwrap();
            zip.write_all(b"ok").unwrap();
            zip.finish().unwrap();
        }
        let dst = tmp.path().join("out/inner");
        let n = extract_zip_safely(&zip_path, &dst).unwrap();
        assert_eq!(fs::read(dst.join("saves/slot1.sav")).unwrap(), b"ok");
        assert!(!tmp.path().join("out/outside.txt").exists());
        assert!(!tmp.path().join("outside.txt").exists());
        // Only entries that stay inside the destination are written.
        let written: Vec<_> = walkdir::WalkDir::new(&dst)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .collect();
        assert_eq!(written.len(), n);
        assert!(written.iter().all(|e| e.path().starts_with(&dst)));
    }

    #[test]
    fn tar_gz_extraction_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("a")).unwrap();
        fs::write(src.join("a/one.sav"), b"1").unwrap();
        let tgz = tmp.path().join("s.tar.gz");
        {
            let enc = flate2::write::GzEncoder::new(File::create(&tgz).unwrap(), flate2::Compression::fast());
            let mut builder = tar::Builder::new(enc);
            builder.append_dir_all("pack", &src).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let dst = tmp.path().join("dst");
        assert_eq!(extract_archive(&tgz, &dst).unwrap(), 1);
        assert_eq!(fs::read(dst.join("pack/a/one.sav")).unwrap(), b"1");
        assert!(is_supported_archive(&tgz));
        assert!(!is_supported_archive(Path::new("save.rar")));
    }
}
