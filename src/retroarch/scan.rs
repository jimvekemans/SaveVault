//! RetroArch library scanning (playlists and ROM folders), per-ROM save filtering,
//! playlist maintenance, and honest save-root detection.

use crate::config::write_private_atomic;
use crate::detect::template::{PathContext, tokenize_path};
use crate::detect::{DetectedRoot, Detection, measure_root};
use crate::models::{
    DetectionInfo, DetectionSource, GameEntry, Provider, RootKind, SyncSelectionMode,
};
use crate::providers::slugify_game_name;
use crate::retroarch::cfg::{
    RetroArchConfig, default_retroarch_config_dir, infer_kbm_from_existing_cfg,
    resolve_core_for_rom_with_root,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn is_unambiguous_rom_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "nds" | "dsi" | "ids" | "3ds" | "cci" | "cxi" | "3dsx" | "iso" | "chd" | "cso" | "zso"
            | "cue" | "m3u"
    )
}

fn clean_rom_display_name(stem: &str) -> String {
    let mut parts = Vec::new();
    let mut rest = stem.trim();
    if let Some(idx) = rest.find('(') {
        let base = rest[..idx].trim();
        if !base.is_empty() {
            parts.push(base.to_string());
        }
        rest = &rest[idx..];
        if let Some(end) = rest.find(')') {
            let region = rest[..=end].trim();
            if !region.starts_with("(En") {
                parts.push(region.to_string());
            }
        }
    } else if !rest.is_empty() {
        parts.push(rest.to_string());
    }
    if parts.is_empty() {
        stem.to_string()
    } else {
        parts.join(" ")
    }
}

fn normalize_rom_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Returns a per-ROM glob include list so multiple games sharing a RetroArch core folder
/// (e.g. `saves/<core>/`) remain isolated during backup and rollback.
pub fn per_rom_save_includes(rom_stem: &str) -> Vec<String> {
    vec![
        format!("{rom_stem}.*"),
        format!("{rom_stem}_*"),
        format!("{rom_stem}/**"),
    ]
}

/// Detects RetroArch primary save and save-state roots for `game` using `retroarch.cfg` paths
/// and dynamic path templates (`tokenize_path`).
pub fn detect_retroarch_roots(game: &GameEntry) -> Detection {
    let Some(meta) = game.retroarch.as_ref() else {
        return Detection::default();
    };
    let ra_root = game
        .library_root
        .clone()
        .unwrap_or_else(default_retroarch_config_dir);
    let ra_cfg = RetroArchConfig::load(&ra_root);
    let ctx = PathContext::for_game(game);

    let rom_stem = meta
        .rom_path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&game.name);

    let save_dir = ra_cfg.saves_dir_for(&ra_root, &meta.core_name, &meta.rom_path);
    let states_dir = ra_cfg.states_dir_for(&ra_root, &meta.core_name, &meta.rom_path);

    let save_includes = per_rom_save_includes(rom_stem);
    let state_includes = vec![format!("{rom_stem}.state*")];

    let primary = DetectedRoot {
        kind: RootKind::Saves,
        id_hint: None,
        label_hint: None,
        template: tokenize_path(&save_dir, &ctx),
        stats: measure_root(&save_dir, &save_includes, &game.exclude_globs),
        path: save_dir,
        include: save_includes,
        exclude: Vec::new(),
    };
    let states = DetectedRoot {
        kind: RootKind::Saves,
        id_hint: Some("savestates".to_string()),
        label_hint: Some("Save States".to_string()),
        template: tokenize_path(&states_dir, &ctx),
        stats: measure_root(&states_dir, &state_includes, &game.exclude_globs),
        path: states_dir,
        include: state_includes,
        exclude: Vec::new(),
    };

    Detection {
        info: DetectionInfo {
            source: DetectionSource::Heuristic,
            confidence: 90,
            reason: format!(
                "RetroArch ({}) per-game saves & save states ({}).",
                meta.core_name,
                meta.system_name.as_deref().unwrap_or("RetroArch"),
            ),
            database_title: None,
        },
        saves: vec![primary, states],
        configs: Vec::new(),
        mod_sources: Vec::new(),
        alternatives: Vec::new(),
    }
}

struct DiscoveredRetroGame {
    rom_path: PathBuf,
    label: String,
    core_path: PathBuf,
    core_name: String,
    system_name: String,
    playlist_name: Option<String>,
}

fn looks_like_ra_config_root(root: &Path) -> bool {
    root.join("playlists").is_dir()
        || root.join("retroarch.cfg").is_file()
        || root.join("saves").is_dir()
        || root.join("states").is_dir()
}

/// Scans configured RetroArch paths (config directories containing `.lpl` playlists and optional
/// ROM folders) and returns `Provider::RetroArch` `GameEntry` records.
pub fn scan_retroarch_paths(paths: &[PathBuf]) -> Vec<GameEntry> {
    let mut ra_root: Option<PathBuf> = None;
    let mut by_rom: BTreeMap<String, DiscoveredRetroGame> = BTreeMap::new();

    // Pass 1: Identify RetroArch config root and parse `.lpl` playlists for any core.
    for root in paths {
        if !root.exists() {
            continue;
        }
        if looks_like_ra_config_root(root) {
            if ra_root.is_none() {
                ra_root = Some(root.clone());
            }
            let pl_dir = root.join("playlists");
            if let Ok(entries) = fs::read_dir(&pl_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("lpl") {
                        continue;
                    }
                    let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if fname.contains("history") || fname.contains("favorites") {
                        continue;
                    }
                    let Ok(raw) = fs::read_to_string(&path) else {
                        continue;
                    };
                    let Ok(doc) = serde_json::from_str::<Value>(&raw) else {
                        continue;
                    };
                    let default_core_path = doc
                        .get("default_core_path")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let default_core_name = doc
                        .get("default_core_name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let playlist_title = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_string);

                    for item in doc
                        .get("items")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default()
                    {
                        let Some(rom_str) = item.get("path").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        let rom_path = PathBuf::from(rom_str);
                        if !rom_path.exists() {
                            continue;
                        }
                        let c_path = item
                            .get("core_path")
                            .and_then(|v| v.as_str())
                            .or(default_core_path.as_deref());
                        let c_name = item
                            .get("core_name")
                            .and_then(|v| v.as_str())
                            .or(default_core_name.as_deref());
                        let Some((core_path, core_name, sys_name)) =
                            resolve_core_for_rom_with_root(Some(root), c_path, c_name, &rom_path)
                        else {
                            continue;
                        };
                        let label = item
                            .get("label")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                let stem = rom_path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("RetroArch Game");
                                clean_rom_display_name(stem)
                            });
                        let key = normalize_rom_key(&rom_path);
                        by_rom.insert(
                            key,
                            DiscoveredRetroGame {
                                rom_path,
                                label,
                                core_path,
                                core_name,
                                system_name: playlist_title.clone().unwrap_or(sys_name),
                                playlist_name: playlist_title.clone(),
                            },
                        );
                    }
                }
            }
        }
    }

    let resolved_ra_root = ra_root.clone();

    // Pass 2: Scan non-config folders in `paths` for loose ROMs with unambiguous system hints.
    for root in paths {
        if !root.exists() || looks_like_ra_config_root(root) {
            continue;
        }
        for entry in walkdir::WalkDir::new(root)
            .max_depth(3)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let rom_path = entry.path().to_path_buf();
            if !is_unambiguous_rom_extension(&rom_path) {
                continue;
            }
            let key = normalize_rom_key(&rom_path);
            if by_rom.contains_key(&key) {
                continue;
            }
            let Some((core_path, core_name, sys_name)) =
                resolve_core_for_rom_with_root(resolved_ra_root.as_deref(), None, None, &rom_path)
            else {
                continue;
            };
            let stem = rom_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("RetroArch Game")
                .to_string();
            by_rom.insert(
                key,
                DiscoveredRetroGame {
                    rom_path,
                    label: clean_rom_display_name(&stem),
                    core_path,
                    core_name,
                    system_name: sys_name,
                    playlist_name: None,
                },
            );
        }
    }

    let ra_cfg_root = ra_root.unwrap_or_else(default_retroarch_config_dir);
    let mut results = Vec::new();
    let mut used_ids = BTreeSet::new();

    for disc in by_rom.into_values() {
        let mut slug = slugify_game_name(&disc.label);
        let mut id = format!("{}:{slug}", Provider::RetroArch.slug());
        if !used_ids.insert(id.clone()) {
            let stem_slug = slugify_game_name(
                disc.rom_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&slug),
            );
            slug = stem_slug.clone();
            id = format!("{}:{slug}", Provider::RetroArch.slug());
            if !used_ids.insert(id.clone()) {
                for n in 2.. {
                    let cand_slug = format!("{stem_slug}_{n}");
                    let cand_id = format!("{}:{cand_slug}", Provider::RetroArch.slug());
                    if used_ids.insert(cand_id.clone()) {
                        slug = cand_slug;
                        id = cand_id;
                        break;
                    }
                }
            }
        }

        let rom_stem = disc
            .rom_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&disc.label)
            .to_string();
        let cfg_file = ra_cfg_root
            .join("config")
            .join(&disc.core_name)
            .join(format!("{rom_stem}.cfg"));
        let kbm_enabled = infer_kbm_from_existing_cfg(&cfg_file);

        let meta = crate::kbm::keys::RetroArchGameMeta {
            rom_path: disc.rom_path.clone(),
            core_path: disc.core_path.clone(),
            core_name: disc.core_name.clone(),
            playlist_name: disc.playlist_name,
            system_name: Some(disc.system_name.clone()),
            kbm_enabled,
            kbm_profile: crate::kbm::keys::KbmProfile::default(),
            kbm_customized: kbm_enabled,
        };

        let mut entry = GameEntry {
            id,
            name: disc.label,
            game_slug: slug,
            provider: Provider::RetroArch,
            app_id: Some(disc.core_name),
            install_dir: disc.rom_path.parent().map(Path::to_path_buf),
            enabled: true,
            sync_selection: SyncSelectionMode::SelectedFiles(per_rom_save_includes(&rom_stem)),
            library_root: Some(ra_cfg_root.clone()),
            retroarch: Some(meta),
            ..Default::default()
        };

        let det = detect_retroarch_roots(&entry);
        crate::detect::apply_detection(&mut entry, &det, false);
        results.push(entry);
    }

    results
}

/// Returns a list of existing RetroArch save state file paths for `game`.
pub fn list_retroarch_save_states(game: &GameEntry) -> Vec<PathBuf> {
    let Some(root) = game.extra_roots.iter().find(|r| r.id == "savestates") else {
        return Vec::new();
    };
    let Ok(rels) = crate::sync_engine::collect_root_files(root, &game.exclude_globs, &[]) else {
        return Vec::new();
    };
    rels.into_iter().map(|rel| root.path.join(rel)).collect()
}

/// Ensures `rom_path` exists in RetroArch `.lpl` JSON playlist at `lpl_path` atomically.
pub fn ensure_playlist_item(
    lpl_path: &Path,
    rom_path: &Path,
    label: &str,
    core_path: &Path,
    core_name: &str,
    db_name: Option<&str>,
) -> Result<()> {
    if let Some(parent) = lpl_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create playlist dir {}", parent.display()))?;
    }
    let mut data: Value = fs::read_to_string(lpl_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| {
            json!({
                "version": "1.5",
                "default_core_path": core_path.display().to_string(),
                "default_core_name": core_name,
                "label_display_mode": 0,
                "right_thumbnail_mode": 0,
                "left_thumbnail_mode": 0,
                "thumbnail_match_mode": 0,
                "sort_mode": 0,
                "items": []
            })
        });

    let rom_str = rom_path.display().to_string();
    let db = db_name
        .map(str::to_string)
        .or_else(|| {
            lpl_path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "playlist.lpl".to_string());

    let items = data
        .get_mut("items")
        .and_then(|v| v.as_array_mut())
        .context("Invalid RetroArch playlist JSON (missing items array)")?;

    if !items
        .iter()
        .any(|it| it.get("path").and_then(|v| v.as_str()) == Some(rom_str.as_str()))
    {
        items.push(json!({
            "path": rom_str,
            "label": label,
            "core_path": core_path.display().to_string(),
            "core_name": core_name,
            "crc32": "DETECT",
            "db_name": db,
        }));
    }

    let rendered = format!("{}\n", serde_json::to_string_pretty(&data)?);
    write_private_atomic(lpl_path, rendered.as_bytes())
        .with_context(|| format!("Failed to write playlist {}", lpl_path.display()))?;
    Ok(())
}
