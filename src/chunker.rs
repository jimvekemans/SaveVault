use crate::models::{LargeFileWarning, Provider, SyncSelectionMode};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub const DEFAULT_CHUNK_SIZE_BYTES: u64 = 45 * 1024 * 1024;
pub const GITHUB_LFS_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const STREAM_BUF_SIZE: usize = 64 * 1024;

/// Metadata describing a large save file split into GitHub-safe chunks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkManifest {
    pub original_file_name: String,
    pub total_size: u64,
    pub full_sha256: String,
    pub chunk_size: u64,
    pub chunks: Vec<ChunkPartInfo>,
}

/// Metadata for a single chunk part file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkPartInfo {
    pub part_file_name: String,
    pub index: usize,
    pub size: u64,
    pub sha256: String,
}

/// Returns the manifest filename for a split file.
pub fn manifest_file_name(file_name: &str) -> String {
    format!("{file_name}.sgm_manifest.json")
}

/// Returns true if a filename is a Save Game Manager chunk manifest.
pub fn is_manifest_file_name(name: &str) -> bool {
    name.ends_with(".sgm_manifest.json")
}

/// Returns true if a filename is a Save Game Manager chunk part.
pub fn is_chunk_part_file_name(name: &str) -> bool {
    name.contains(".sgm_part.")
}

/// Formats a byte slice as lowercase hexadecimal.
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Computes the SHA-256 hex digest of a file on disk using streaming I/O.
pub fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; STREAM_BUF_SIZE];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

fn cleanup_split_artifacts(dst_dir: &Path, file_name: &str) -> Result<()> {
    let manifest_path = dst_dir.join(manifest_file_name(file_name));
    if manifest_path.exists() {
        fs::remove_file(&manifest_path)?;
    }
    let prefix = format!("{file_name}.sgm_part.");
    if let Ok(entries) = fs::read_dir(dst_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(&prefix) && entry.path().is_file() {
                fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}

/// Copies `src_file` into `dst_dir` or splits it into `.sgm_part.NNN` chunks + `.sgm_manifest.json`
/// when it exceeds `threshold_bytes` and `split_enabled` is true.
pub fn copy_or_split_file(
    src_file: &Path,
    dst_dir: &Path,
    file_name: &str,
    threshold_bytes: u64,
    chunk_size_bytes: u64,
    split_enabled: bool,
) -> Result<Option<LargeFileWarning>> {
    fs::create_dir_all(dst_dir)
        .with_context(|| format!("Failed to create destination directory {}", dst_dir.display()))?;

    let meta = fs::metadata(src_file)
        .with_context(|| format!("Failed to stat source save file {}", src_file.display()))?;
    let total_size = meta.len();
    let effective_chunk_size = if chunk_size_bytes == 0 {
        DEFAULT_CHUNK_SIZE_BYTES
    } else {
        chunk_size_bytes
    };

    if total_size > threshold_bytes {
        let size_mb = total_size as f64 / (1024.0 * 1024.0);
        let threshold_mb = threshold_bytes as f64 / (1024.0 * 1024.0);
        let lfs_note = if total_size >= GITHUB_LFS_MAX_FILE_BYTES {
            " (exceeds GitHub LFS 2 GB single-file limit)"
        } else {
            ""
        };

        if split_enabled {
            let raw_dst = dst_dir.join(file_name);
            if raw_dst.exists() {
                fs::remove_file(&raw_dst)?;
            }
            cleanup_split_artifacts(dst_dir, file_name)?;

            let file = File::open(src_file)
                .with_context(|| format!("Failed to open {} for splitting", src_file.display()))?;
            let mut reader = BufReader::new(file);
            let mut full_hasher = Sha256::new();
            let mut buf = [0u8; STREAM_BUF_SIZE];
            let mut chunks = Vec::new();
            let mut chunk_index = 0usize;

            loop {
                let part_name = format!("{file_name}.sgm_part.{chunk_index:03}");
                let part_path = dst_dir.join(&part_name);
                let mut part_writer = BufWriter::new(
                    File::create(&part_path)
                        .with_context(|| format!("Failed to create chunk {}", part_path.display()))?,
                );
                let mut part_hasher = Sha256::new();
                let mut written_in_part: u64 = 0;

                while written_in_part < effective_chunk_size {
                    let remaining = (effective_chunk_size - written_in_part) as usize;
                    let to_read = remaining.min(buf.len());
                    let n = reader.read(&mut buf[..to_read])?;
                    if n == 0 {
                        break;
                    }
                    part_writer.write_all(&buf[..n])?;
                    part_hasher.update(&buf[..n]);
                    full_hasher.update(&buf[..n]);
                    written_in_part += n as u64;
                }

                part_writer.flush()?;

                if written_in_part == 0 {
                    drop(part_writer);
                    let _ = fs::remove_file(&part_path);
                    break;
                }

                chunks.push(ChunkPartInfo {
                    part_file_name: part_name,
                    index: chunk_index,
                    size: written_in_part,
                    sha256: to_hex(&part_hasher.finalize()),
                });
                chunk_index += 1;
            }

            let manifest = ChunkManifest {
                original_file_name: file_name.to_string(),
                total_size,
                full_sha256: to_hex(&full_hasher.finalize()),
                chunk_size: effective_chunk_size,
                chunks,
            };

            let manifest_path = dst_dir.join(manifest_file_name(file_name));
            let manifest_json = serde_json::to_string_pretty(&manifest)?;
            fs::write(&manifest_path, manifest_json)?;

            let chunk_count = manifest.chunks.len();
            return Ok(Some(LargeFileWarning {
                game_name: String::new(),
                provider: Provider::Steam,
                file_path: src_file.to_path_buf(),
                relative_path: file_name.to_string(),
                size_bytes: total_size,
                threshold_bytes,
                split_applied: true,
                message: format!(
                    "Save file '{}' ({:.2} MB) exceeded the {:.2} MB GitHub/LFS warning threshold{} and was split into {} chunks.",
                    file_name, size_mb, threshold_mb, lfs_note, chunk_count
                ),
            }));
        } else {
            cleanup_split_artifacts(dst_dir, file_name)?;
            fs::copy(src_file, dst_dir.join(file_name)).with_context(|| {
                format!(
                    "Failed to copy large save file {} to {}",
                    src_file.display(),
                    dst_dir.display()
                )
            })?;

            return Ok(Some(LargeFileWarning {
                game_name: String::new(),
                provider: Provider::Steam,
                file_path: src_file.to_path_buf(),
                relative_path: file_name.to_string(),
                size_bytes: total_size,
                threshold_bytes,
                split_applied: false,
                message: format!(
                    "Warning: Save file '{}' ({:.2} MB) exceeds the {:.2} MB GitHub/LFS warning threshold{} and auto-splitting is disabled.",
                    file_name, size_mb, threshold_mb, lfs_note
                ),
            }));
        }
    }

    cleanup_split_artifacts(dst_dir, file_name)?;
    fs::copy(src_file, dst_dir.join(file_name)).with_context(|| {
        format!(
            "Failed to copy {} to {}",
            src_file.display(),
            dst_dir.join(file_name).display()
        )
    })?;
    Ok(None)
}

/// Checks whether a normalized relative path matches the game's `SyncSelectionMode`.
pub fn path_matches_selection(rel_path: &Path, selection: &SyncSelectionMode) -> bool {
    match selection {
        SyncSelectionMode::EntireFolder => true,
        SyncSelectionMode::SelectedFiles(selected) => {
            crate::pattern::PatternSet::new(selected).matches(&normalize_rel_path(rel_path))
        }
    }
}

/// Normalizes a relative path using forward slashes for cross-platform consistency.
pub fn normalize_rel_path(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Restores files from `repo_dir` into `target_save_dir`, reassembling any `.sgm_manifest.json`
/// split saves with SHA-256 verification.
pub fn reassemble_or_copy_dir(
    repo_dir: &Path,
    target_save_dir: &Path,
    selection: &SyncSelectionMode,
) -> Result<usize> {
    if !repo_dir.exists() {
        bail!("Repository save folder does not exist: {}", repo_dir.display());
    }
    fs::create_dir_all(target_save_dir).with_context(|| {
        format!(
            "Failed to create target save directory {}",
            target_save_dir.display()
        )
    })?;

    let mut restored_count = 0usize;

    for entry in WalkDir::new(repo_dir)
        .into_iter()
        .filter_entry(|e| e.file_name().to_str() != Some(".git"))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(repo_dir)?;
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if fname == ".gitattributes" || fname == ".gitkeep" || fname == "sgm_repo_metadata.json" {
            continue;
        }
        // SaveVault's own bookkeeping folders (other profiles, extra roots) are never save data.
        if rel
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .is_some_and(|c| c.starts_with("@sgm_"))
        {
            continue;
        }

        if is_chunk_part_file_name(fname) {
            continue;
        }

        if is_manifest_file_name(fname) {
            let manifest_raw = fs::read_to_string(path)
                .with_context(|| format!("Failed to read manifest {}", path.display()))?;
            let manifest: ChunkManifest = serde_json::from_str(&manifest_raw)
                .with_context(|| format!("Failed to parse manifest {}", path.display()))?;

            let rel_parent = rel.parent().unwrap_or_else(|| Path::new(""));
            let original_rel = if rel_parent.as_os_str().is_empty() {
                PathBuf::from(&manifest.original_file_name)
            } else {
                rel_parent.join(&manifest.original_file_name)
            };

            if !path_matches_selection(&original_rel, selection) {
                continue;
            }

            let dst_file = target_save_dir.join(&original_rel);
            if let Some(parent) = dst_file.parent() {
                fs::create_dir_all(parent)?;
            }

            let src_dir = path.parent().unwrap_or(repo_dir);
            reassemble_from_manifest(&manifest, src_dir, &dst_file)?;
            restored_count += 1;
        } else {
            if !path_matches_selection(rel, selection) {
                continue;
            }
            let dst_file = target_save_dir.join(rel);
            if let Some(parent) = dst_file.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(path, &dst_file).with_context(|| {
                format!(
                    "Failed to copy {} to {}",
                    path.display(),
                    dst_file.display()
                )
            })?;
            restored_count += 1;
        }
    }

    Ok(restored_count)
}

/// Rebuilds one split file from its parts, verifying every chunk and the whole file.
pub fn reassemble_from_manifest(
    manifest: &ChunkManifest,
    parts_dir: &Path,
    dst_file: &Path,
) -> Result<()> {
    let mut parts = manifest.chunks.clone();
    parts.sort_by_key(|p| p.index);

    let out = File::create(dst_file)
        .with_context(|| format!("Failed to create restored file {}", dst_file.display()))?;
    let mut writer = BufWriter::new(out);
    let mut full_hasher = Sha256::new();
    let mut total_written: u64 = 0;
    let mut buf = [0u8; STREAM_BUF_SIZE];

    for part in &parts {
        let part_path = parts_dir.join(&part.part_file_name);
        let part_file = File::open(&part_path)
            .with_context(|| format!("Missing chunk part {}", part_path.display()))?;
        let mut reader = BufReader::new(part_file);
        let mut part_hasher = Sha256::new();
        let mut part_bytes: u64 = 0;

        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n])?;
            part_hasher.update(&buf[..n]);
            full_hasher.update(&buf[..n]);
            part_bytes += n as u64;
        }

        let actual_part_sha = to_hex(&part_hasher.finalize());
        if actual_part_sha != part.sha256 || part_bytes != part.size {
            bail!(
                "Chunk integrity mismatch for '{}' (expected sha256 {}, got {})",
                part.part_file_name,
                part.sha256,
                actual_part_sha
            );
        }
        total_written += part_bytes;
    }

    writer.flush()?;

    let actual_full_sha = to_hex(&full_hasher.finalize());
    if actual_full_sha != manifest.full_sha256 || total_written != manifest.total_size {
        bail!(
            "Reassembled file integrity mismatch for '{}' (expected sha256 {}, got {})",
            manifest.original_file_name,
            manifest.full_sha256,
            actual_full_sha
        );
    }

    Ok(())
}
