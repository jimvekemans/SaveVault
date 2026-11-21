//! Multi-game KBM profile registry persistence (`retroarch_kbm_profiles.json`).

use crate::config::write_private_atomic;
use crate::kbm::keys::KbmProfile;
use crate::models::{AppConfig, Provider};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const KBM_PROFILES_FILENAME: &str = "retroarch_kbm_profiles.json";

/// Serializable registry consumed by `retroarch-mkb-bridge` for per-game KBM profiles and
/// live hot-reloading during gameplay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KbmProfileRegistry {
    pub version: u32,
    #[serde(default)]
    pub default_profile: KbmProfile,
    #[serde(default)]
    pub games: Vec<KbmGameProfileEntry>,
}

impl Default for KbmProfileRegistry {
    fn default() -> Self {
        Self {
            version: 1,
            default_profile: KbmProfile::default(),
            games: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KbmGameProfileEntry {
    pub game_id: String,
    pub name: String,
    pub rom_path: PathBuf,
    pub rom_stem: String,
    pub core_path: PathBuf,
    pub core_name: String,
    pub enabled: bool,
    pub profile: KbmProfile,
}

/// Returns the host user's default `~/.config/save_game_manager/retroarch_kbm_profiles.json`.
pub fn default_kbm_profiles_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("save_game_manager")
        .join(KBM_PROFILES_FILENAME)
}

/// Returns the KBM registry path sibling to a given `config.json` path.
pub fn kbm_profiles_path_for(config_path: &Path) -> PathBuf {
    config_path.with_file_name(KBM_PROFILES_FILENAME)
}

/// Builds the serializable multi-game KBM registry from `AppConfig`.
pub fn build_kbm_registry(config: &AppConfig) -> KbmProfileRegistry {
    let mut games = Vec::new();
    for g in &config.games {
        if let Some(meta) = &g.retroarch {
            let rom_stem = meta
                .rom_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&g.name)
                .to_string();
            games.push(KbmGameProfileEntry {
                game_id: g.id.clone(),
                name: g.name.clone(),
                rom_path: meta.rom_path.clone(),
                rom_stem,
                core_path: meta.core_path.clone(),
                core_name: meta.core_name.clone(),
                enabled: meta.kbm_enabled,
                profile: meta.kbm_profile.clone(),
            });
        }
    }
    KbmProfileRegistry {
        version: 1,
        default_profile: config.default_kbm_profile.clone(),
        games,
    }
}

/// Writes `retroarch_kbm_profiles.json` atomically at `path`.
pub fn write_kbm_profiles_registry_at(path: &Path, config: &AppConfig) -> Result<()> {
    let registry = build_kbm_registry(config);
    let json = serde_json::to_string_pretty(&registry)
        .context("Failed to serialize RetroArch KBM profile registry")?;
    write_private_atomic(path, json.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

/// Single consistent rule determining whether `config` needs the KBM registry on disk
/// (either a RetroArch game has KBM enabled, the default template was customized, or the
/// registry file already exists and should stay synchronized).
pub fn sync_kbm_registry_for_config(
    config_path: &Path,
    config: &AppConfig,
) -> Result<Option<PathBuf>> {
    let reg_path = kbm_profiles_path_for(config_path);
    let any_kbm_enabled = config.games.iter().any(|g| {
        g.provider == Provider::RetroArch
            && g.retroarch.as_ref().is_some_and(|m| m.kbm_enabled)
    });
    let should_write = any_kbm_enabled
        || config.default_kbm_profile != KbmProfile::default()
        || reg_path.exists();
    if !should_write {
        return Ok(None);
    }
    write_kbm_profiles_registry_at(&reg_path, config)?;
    Ok(Some(reg_path))
}

/// Loads `KbmProfileRegistry` from `path`, returning an empty default registry if missing or unreadable.
pub fn load_kbm_profiles_registry(path: &Path) -> KbmProfileRegistry {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<KbmProfileRegistry>(&raw).ok())
        .unwrap_or_default()
}
