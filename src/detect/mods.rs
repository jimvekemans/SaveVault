//! Mod awareness: finding where a game's mod list lives and fingerprinting it so every
//! snapshot remembers which mods it was made with.

use super::heuristics::name_similarity;
use super::template::PathContext;
use crate::chunker::to_hex;
use crate::models::{GameEntry, ModFingerprint, ModSource, ModSourceKind};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_ENTRIES_PER_SOURCE: usize = 5000;

/// Mod folders (relative to the install folder) used by common loaders and games.
const KNOWN_MOD_DIRS: &[(&str, &str)] = &[
    ("Mods", "Mods"),
    ("mods", "Mods"),
    ("BepInEx/plugins", "BepInEx plugins"),
    ("MelonLoader/Mods", "MelonLoader mods"),
    ("Mods/Plugins", "Plugins"),
    ("reframework/plugins", "REFramework plugins"),
    ("reframework/autorun", "REFramework scripts"),
    ("archive/pc/mod", "Archive mods"),
    ("r6/scripts", "Redscript mods"),
    ("red4ext/plugins", "RED4ext plugins"),
    ("bin/x64/plugins/cyber_engine_tweaks/mods", "CET mods"),
    ("Game/mod", "Mod Engine mods"),
    ("mod", "Mods"),
    ("dlc", "DLC folder"),
    ("QMods", "QMods"),
    ("UMM/Mods", "Unity Mod Manager mods"),
    ("Mods/ModLoader", "ModLoader mods"),
    ("resourcepacks", "Resource packs"),
    ("shaderpacks", "Shader packs"),
];

fn push_unique(out: &mut Vec<ModSource>, src: ModSource) {
    if !out.iter().any(|s| s.path == src.path) {
        out.push(src);
    }
}

/// Finds Unreal `~mods` pak folders a few levels below the install folder.
fn find_ue_pak_mods(install: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(install)
        .max_depth(5)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n == "~mods" || n == "logicmods"
        })
        .map(|e| e.path().to_path_buf())
        .take(4)
        .collect()
}

/// Suggests mod list locations for a game by probing its install folder and prefix.
pub fn suggest_mod_sources(game: &GameEntry, ctx: &PathContext) -> Vec<ModSource> {
    let mut out = Vec::new();

    if let Some(install) = &ctx.install_dir {
        for (rel, label) in KNOWN_MOD_DIRS {
            let p = install.join(rel);
            if p.is_dir() && fs::read_dir(&p).map(|mut d| d.next().is_some()).unwrap_or(false) {
                push_unique(
                    &mut out,
                    ModSource {
                        label: (*label).to_string(),
                        path: p,
                        kind: ModSourceKind::Folder,
                        enabled: true,
                    },
                );
            }
        }
        for p in find_ue_pak_mods(install) {
            push_unique(
                &mut out,
                ModSource {
                    label: "Pak mods".to_string(),
                    path: p,
                    kind: ModSourceKind::Folder,
                    enabled: true,
                },
            );
        }
    }

    let install_name = ctx
        .install_dir
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let names = [game.name.as_str(), install_name];

    for user in ctx.wine_user_dirs() {
        // Bethesda-style load order next to the game's settings.
        let local = user.join("AppData/Local");
        if let Ok(rd) = fs::read_dir(&local) {
            for entry in rd.flatten() {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                for file in ["Plugins.txt", "plugins.txt", "loadorder.txt", "LoadOrder.txt"] {
                    let p = dir.join(file);
                    if p.is_file() {
                        push_unique(
                            &mut out,
                            ModSource {
                                label: "Load order".to_string(),
                                path: p,
                                kind: ModSourceKind::ListFile,
                                enabled: true,
                            },
                        );
                        break;
                    }
                }
            }
        }

        // Mod Organizer 2 instances: use the selected profile's mod list.
        let mo2 = local.join("ModOrganizer");
        if let Ok(instances) = fs::read_dir(&mo2) {
            for inst in instances.flatten() {
                let inst_path = inst.path();
                let inst_name = inst.file_name().to_string_lossy().into_owned();
                if !inst_path.is_dir() || name_similarity(&names, &[inst_name]) < 0.5 {
                    continue;
                }
                let selected = fs::read_to_string(inst_path.join("ModOrganizer.ini"))
                    .ok()
                    .and_then(|ini| {
                        ini.lines().find_map(|l| {
                            l.trim()
                                .strip_prefix("selected_profile=")
                                .map(|v| v.trim_start_matches("@ByteArray(").trim_end_matches(')').to_string())
                        })
                    })
                    .unwrap_or_else(|| "Default".to_string());
                let modlist = inst_path.join("profiles").join(&selected).join("modlist.txt");
                if modlist.is_file() {
                    push_unique(
                        &mut out,
                        ModSource {
                            label: format!("Mod Organizer ({selected})"),
                            path: modlist,
                            kind: ModSourceKind::ListFile,
                            enabled: true,
                        },
                    );
                }
            }
        }
    }

    out
}

fn read_list_file(path: &Path) -> Vec<String> {
    let Ok(raw) = fs::read(path) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&raw)
        .lines()
        .map(str::trim)
        // '#' comments, '-' = disabled in Mod Organizer mod lists.
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-'))
        .map(|l| l.trim_start_matches(['+', '*']).trim().to_string())
        .filter(|l| !l.is_empty())
        .take(MAX_ENTRIES_PER_SOURCE)
        .collect()
}

fn read_folder(path: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && !n.eq_ignore_ascii_case("desktop.ini"))
        .take(MAX_ENTRIES_PER_SOURCE)
        .collect();
    names.sort();
    names
}

/// Reads every enabled source and produces a stable fingerprint of the installed mods.
/// Returns `None` when the game has no mod sources at all.
pub fn scan_mod_fingerprint(sources: &[ModSource]) -> Option<ModFingerprint> {
    let enabled: Vec<&ModSource> = sources.iter().filter(|s| s.enabled).collect();
    if enabled.is_empty() {
        return None;
    }
    let mut entries: BTreeSet<String> = BTreeSet::new();
    for src in enabled {
        let names = match src.kind {
            ModSourceKind::ListFile => read_list_file(&src.path),
            ModSourceKind::Folder => read_folder(&src.path),
        };
        for n in names {
            entries.insert(format!("{}: {n}", src.label));
        }
    }
    let entries: Vec<String> = entries.into_iter().collect();
    let mut hasher = Sha256::new();
    for e in &entries {
        hasher.update(e.as_bytes());
        hasher.update(b"\n");
    }
    Some(ModFingerprint {
        hash: to_hex(&hasher.finalize()),
        entries,
    })
}

/// Difference between the mods a snapshot was made with and the mods installed now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModDiff {
    /// In the snapshot but not installed now.
    pub missing_now: Vec<String>,
    /// Installed now but not when the snapshot was made.
    pub added_since: Vec<String>,
}

impl ModDiff {
    pub fn is_empty(&self) -> bool {
        self.missing_now.is_empty() && self.added_since.is_empty()
    }

    /// One-line plain-language summary.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "Same mods as now.".to_string();
        }
        let mut parts = Vec::new();
        if !self.missing_now.is_empty() {
            parts.push(format!("{} mod(s) it used are not installed now", self.missing_now.len()));
        }
        if !self.added_since.is_empty() {
            parts.push(format!("{} mod(s) were added since", self.added_since.len()));
        }
        parts.join("; ")
    }
}

pub fn diff_fingerprints(snapshot: Option<&ModFingerprint>, current: Option<&ModFingerprint>) -> ModDiff {
    let empty = Vec::new();
    let old: BTreeSet<&String> = snapshot.map(|f| &f.entries).unwrap_or(&empty).iter().collect();
    let new: BTreeSet<&String> = current.map(|f| &f.entries).unwrap_or(&empty).iter().collect();
    ModDiff {
        missing_now: old.difference(&new).map(|s| (*s).clone()).collect(),
        added_since: new.difference(&old).map(|s| (*s).clone()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Platform, Provider};
    use tempfile::TempDir;

    #[test]
    fn suggests_known_mod_folders_and_load_order_files() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path().join("common/Skyrim Special Edition");
        let prefix = tmp.path().join("compatdata/489830/pfx");
        let local = prefix.join("drive_c/users/steamuser/AppData/Local/Skyrim Special Edition");
        fs::create_dir_all(install.join("BepInEx/plugins/CoolMod")).unwrap();
        fs::create_dir_all(install.join("Mods")).unwrap(); // empty -> ignored
        fs::create_dir_all(install.join("Game/Content/Paks/~mods")).unwrap();
        fs::create_dir_all(&local).unwrap();
        fs::write(local.join("Plugins.txt"), "# header\n*Unofficial Patch.esp\nDisabled.esp\n").unwrap();

        let game = GameEntry {
            name: "The Elder Scrolls V: Skyrim Special Edition".into(),
            provider: Provider::Steam,
            install_dir: Some(install.clone()),
            prefix_dir: Some(prefix.clone()),
            ..Default::default()
        };
        let ctx = PathContext {
            host: Platform::Linux,
            home: tmp.path().join("home"),
            xdg_data: tmp.path().join("d"),
            xdg_config: tmp.path().join("c"),
            install_dir: Some(install.clone()),
            library_root: None,
            prefix_dir: Some(prefix),
            app_id: None,
        };
        let sources = suggest_mod_sources(&game, &ctx);
        assert!(sources.iter().any(|s| s.path == install.join("BepInEx/plugins")));
        assert!(sources.iter().any(|s| s.path.ends_with("~mods")));
        assert!(!sources.iter().any(|s| s.path == install.join("Mods")));
        let lo = sources.iter().find(|s| s.kind == ModSourceKind::ListFile).unwrap();
        assert_eq!(lo.path, local.join("Plugins.txt"));

        let fp = scan_mod_fingerprint(&sources).unwrap();
        assert!(fp.entries.contains(&"BepInEx plugins: CoolMod".to_string()));
        assert!(fp.entries.contains(&"Load order: Unofficial Patch.esp".to_string()));
        assert!(fp.entries.contains(&"Load order: Disabled.esp".to_string()));
    }

    #[test]
    fn fingerprint_is_stable_and_diff_reports_both_directions() {
        let tmp = TempDir::new().unwrap();
        let mods = tmp.path().join("Mods");
        fs::create_dir_all(mods.join("A")).unwrap();
        fs::create_dir_all(mods.join("B")).unwrap();
        let src = vec![ModSource {
            label: "Mods".into(),
            path: mods.clone(),
            kind: ModSourceKind::Folder,
            enabled: true,
        }];
        let before = scan_mod_fingerprint(&src).unwrap();
        assert_eq!(before, scan_mod_fingerprint(&src).unwrap());
        assert!(scan_mod_fingerprint(&[]).is_none());

        fs::remove_dir(mods.join("A")).unwrap();
        fs::create_dir_all(mods.join("C")).unwrap();
        let after = scan_mod_fingerprint(&src).unwrap();
        assert_ne!(before.hash, after.hash);

        let diff = diff_fingerprints(Some(&before), Some(&after));
        assert_eq!(diff.missing_now, vec!["Mods: A".to_string()]);
        assert_eq!(diff.added_since, vec!["Mods: C".to_string()]);
        assert!(diff.summary().contains("not installed now"));
        assert!(diff_fingerprints(Some(&after), Some(&after)).is_empty());
    }
}
