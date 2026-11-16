use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_EDITABLE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_BINARY_PREVIEW_BYTES: usize = 64 * 1024;
const MAX_EXTRACTED_STRINGS: usize = 2000;

/// Detected save file encoding/format for viewing and editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveFileFormat {
    Json,
    StructuredText,
    PlainText,
    Binary,
}

impl SaveFileFormat {
    /// Human-readable label shown in the Save Editor header badge.
    pub fn display_name(&self) -> &'static str {
        match self {
            SaveFileFormat::Json => "JSON (Editable)",
            SaveFileFormat::StructuredText => "Config / Structured Text (Editable)",
            SaveFileFormat::PlainText => "Plain Text (Editable)",
            SaveFileFormat::Binary => "Binary Save (Read-Only Hex & Strings)",
        }
    }

    /// Whether direct text editing is supported for this format.
    pub fn is_editable(&self) -> bool {
        !matches!(self, SaveFileFormat::Binary)
    }
}

/// Loaded save file ready for text editing or binary inspection.
#[derive(Debug, Clone)]
pub struct LoadedSaveFile {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub format: SaveFileFormat,
    pub editable: bool,
    pub text_content: String,
    pub hex_preview: String,
    pub extracted_strings: Vec<String>,
}

fn is_structured_text_ext(ext: &str) -> bool {
    matches!(
        ext,
        "ini"
            | "cfg"
            | "conf"
            | "xml"
            | "toml"
            | "yaml"
            | "yml"
            | "vdf"
            | "acf"
            | "plist"
            | "csv"
            | "properties"
    )
}

fn format_hex_dump(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (row_idx, chunk) in bytes.chunks(16).enumerate() {
        let offset = row_idx * 16;
        let _ = write!(out, "{offset:08x}:  ");
        for i in 0..16 {
            if i == 8 {
                out.push(' ');
            }
            if let Some(b) = chunk.get(i) {
                let _ = write!(out, "{b:02x} ");
            } else {
                out.push_str("   ");
            }
        }
        out.push_str(" |");
        for &b in chunk {
            let ch = if (0x20..=0x7e).contains(&b) {
                b as char
            } else {
                '.'
            };
            out.push(ch);
        }
        out.push_str("|\n");
    }
    out
}

fn extract_printable_strings(bytes: &[u8], min_len: usize) -> Vec<String> {
    let mut strings = Vec::new();
    let mut current = String::new();
    for &b in bytes {
        if (0x20..=0x7e).contains(&b) {
            current.push(b as char);
        } else {
            if current.len() >= min_len {
                strings.push(std::mem::take(&mut current));
                if strings.len() >= MAX_EXTRACTED_STRINGS {
                    return strings;
                }
            } else {
                current.clear();
            }
        }
    }
    if current.len() >= min_len && strings.len() < MAX_EXTRACTED_STRINGS {
        strings.push(current);
    }
    strings
}

/// Inspects a save file on disk, detecting whether it is editable JSON/text or read-only binary.
pub fn inspect_save_file(path: &Path) -> Result<LoadedSaveFile> {
    let meta = fs::metadata(path)
        .with_context(|| format!("Cannot access save file {}", path.display()))?;
    let size_bytes = meta.len();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();

    let file =
        File::open(path).with_context(|| format!("Failed to open save file {}", path.display()))?;

    if size_bytes <= MAX_EDITABLE_BYTES {
        let mut buf = Vec::with_capacity(size_bytes as usize);
        file.take(MAX_EDITABLE_BYTES + 1)
            .read_to_end(&mut buf)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        if !buf.contains(&0)
            && let Ok(text) = String::from_utf8(buf.clone())
        {
            let trimmed = text.trim();
            let looks_json = ext == "json"
                || ((trimmed.starts_with('{') || trimmed.starts_with('['))
                    && serde_json::from_str::<Value>(trimmed).is_ok());

            let format = if looks_json {
                SaveFileFormat::Json
            } else if is_structured_text_ext(&ext) {
                SaveFileFormat::StructuredText
            } else {
                SaveFileFormat::PlainText
            };

            return Ok(LoadedSaveFile {
                path: path.to_path_buf(),
                size_bytes,
                format,
                editable: true,
                text_content: text,
                hex_preview: String::new(),
                extracted_strings: Vec::new(),
            });
        }

        let preview_slice = &buf[..buf.len().min(MAX_BINARY_PREVIEW_BYTES)];
        return Ok(LoadedSaveFile {
            path: path.to_path_buf(),
            size_bytes,
            format: SaveFileFormat::Binary,
            editable: false,
            text_content: String::new(),
            hex_preview: format_hex_dump(preview_slice),
            extracted_strings: extract_printable_strings(preview_slice, 4),
        });
    }

    let mut preview_buf = Vec::with_capacity(MAX_BINARY_PREVIEW_BYTES);
    file.take(MAX_BINARY_PREVIEW_BYTES as u64)
        .read_to_end(&mut preview_buf)
        .with_context(|| format!("Failed to preview {}", path.display()))?;

    Ok(LoadedSaveFile {
        path: path.to_path_buf(),
        size_bytes,
        format: SaveFileFormat::Binary,
        editable: false,
        text_content: String::new(),
        hex_preview: format_hex_dump(&preview_buf),
        extracted_strings: extract_printable_strings(&preview_buf, 4),
    })
}

/// Pretty-prints raw JSON content using `serde_json`.
pub fn format_json_content(raw: &str) -> Result<String> {
    let parsed: Value = serde_json::from_str(raw).context("Invalid JSON syntax")?;
    serde_json::to_string_pretty(&parsed).context("Failed to format JSON")
}

/// Validates edited content according to the save file's format.
pub fn validate_save_content(format: SaveFileFormat, content: &str) -> Result<()> {
    match format {
        SaveFileFormat::Binary => {
            bail!("Binary save files are read-only and cannot be saved via the text editor.");
        }
        SaveFileFormat::Json => {
            serde_json::from_str::<Value>(content).context("Invalid JSON syntax")?;
            Ok(())
        }
        SaveFileFormat::StructuredText | SaveFileFormat::PlainText => Ok(()),
    }
}

/// Validates and writes edited text save content back to `path`, optionally creating `<name>.sgm.bak`.
pub fn save_edited_file(
    path: &Path,
    format: SaveFileFormat,
    content: &str,
    create_backup: bool,
) -> Result<Option<PathBuf>> {
    validate_save_content(format, content)?;

    let backup_path = if create_backup && path.exists() {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid save filename for backup")?;
        let bak = path.with_file_name(format!("{name}.sgm.bak"));
        fs::copy(path, &bak)
            .with_context(|| format!("Failed to create backup file {}", bak.display()))?;
        Some(bak)
    } else {
        None
    };

    fs::write(path, content)
        .with_context(|| format!("Failed to write save file {}", path.display()))?;
    Ok(backup_path)
}
