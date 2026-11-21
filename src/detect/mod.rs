//! Save location detection.
//!
//! Combines four sources, best first:
//! 1. the save location database (PCGamingWiki via the Ludusavi manifest),
//! 2. Steam Cloud's `userdata` folder,
//! 3. a ranked scan of the game's prefix / install folder,
//! 4. watching which files change while the game runs (see `heuristics::learn_from_session`).
//!
//! Every result carries a confidence and a one-sentence reason so the UI can explain
//! itself and only auto-enable what it is sure about.

pub mod db;
pub mod heuristics;
pub mod mods;
pub mod template;

use crate::chunker::normalize_rel_path;
use crate::models::{
    AppConfig, DetectionInfo, DetectionSource, GameEntry, Platform, Provider, ProviderLibraryConfig,
    RootKind, SaveRoot, SyncSelectionMode,
};
use crate::pattern::PathFilter;
use crate::providers::scan_provider_libraries;
use db::{DbFile, DbGame, SaveDb};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use template::{PathContext, resolve_template, tokenize_path};
use walkdir::WalkDir;

/// Confidence at or above which a detected location is trusted without review.
pub const AUTO_ENABLE_CONFIDENCE: u8 = 60;
const MEASURE_LIMIT: usize = 20_000;
/// Secondary save folders above this size are not enabled automatically.
pub const LARGE_SECONDARY_ROOT_BYTES: u64 = 256 * 1024 * 1024;

/// Size / freshness of a tracked folder under its include patterns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootStats {
    pub exists: bool,
    pub files: usize,
    pub bytes: u64,
    pub newest: Option<SystemTime>,
}

/// Counts the files a root would back up.
pub fn measure_root(path: &Path, include: &[String], exclude: &[String]) -> RootStats {
    let mut stats = RootStats {
        exists: path.is_dir(),
        ..Default::default()
    };
    if !stats.exists {
        return stats;
    }
    let mut excludes: Vec<&str> = exclude.iter().map(String::as_str).collect();
    excludes.extend(crate::pattern::BUILTIN_EXCLUDES);
    let filter = PathFilter::new(include, &excludes);
    for entry in WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .take(MEASURE_LIMIT)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(path) else {
            continue;
        };
        if !filter.accepts(&normalize_rel_path(rel)) {
            continue;
        }
        stats.files += 1;
        if let Ok(meta) = entry.metadata() {
            stats.bytes += meta.len();
            if let Ok(m) = meta.modified() {
                stats.newest = Some(stats.newest.map_or(m, |c| c.max(m)));
            }
        }
    }
    stats
}

/// A folder detection proposes to track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedRoot {
    pub kind: RootKind,
    pub id_hint: Option<String>,
    pub label_hint: Option<String>,
    pub path: PathBuf,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub template: Option<String>,
    pub stats: RootStats,
}

/// Result of detecting one game's save locations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detection {
    /// Save folders, best first. Empty when nothing is known.
    pub saves: Vec<DetectedRoot>,
    /// Settings folders reported by the database.
    pub configs: Vec<DetectedRoot>,
    pub info: DetectionInfo,
    pub mod_sources: Vec<crate::models::ModSource>,
    /// Other plausible folders the user can pick from when confidence is low.
    pub alternatives: Vec<heuristics::Candidate>,
}

impl Detection {
    pub fn has_location(&self) -> bool {
        !self.saves.is_empty()
    }

    pub fn has_files(&self) -> bool {
        self.saves.iter().any(|r| r.stats.files > 0)
    }
}

/// Finds the database entry for a game using store ids first, then titles.
pub fn lookup_db_game<'a>(game: &GameEntry, db: &'a SaveDb) -> Option<&'a DbGame> {
    let numeric_id = game.app_id.as_deref().and_then(|s| s.parse::<u64>().ok());
    let by_id = match game.provider {
        Provider::Steam => numeric_id.and_then(|id| db.by_steam_id(id)),
        Provider::Gog => numeric_id.and_then(|id| db.by_gog_id(id)),
        Provider::Lutris => game.app_id.as_deref().and_then(|s| db.by_lutris_slug(s)),
        _ => None,
    };
    by_id
        .or_else(|| db.by_title(&game.name))
        .or_else(|| {
            game.install_dir
                .as_deref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .and_then(|n| db.by_install_dir_name(n))
        })
}

fn store_matches(constraint: Option<&str>, provider: Provider) -> bool {
    match (constraint, provider.manifest_store()) {
        (None, _) => true,
        (Some(c), Some(p)) => c == p,
        // Unknown store (Lutris, standalone, …): anything but Microsoft Store layouts.
        (Some(c), None) => c != "microsoft",
    }
}

/// Which interpretations (`windows_mode`) of a database path apply to this game.
fn applicable_modes(file: &DbFile, provider: Provider, ctx: &PathContext) -> Vec<bool> {
    let mut modes = Vec::new();
    let windows_ok = ctx.has_windows_view();
    let native_os = match ctx.host {
        Platform::Linux => Some("linux"),
        Platform::MacOs => Some("mac"),
        Platform::Windows => None,
    };
    let mut consider = |os: Option<&str>| match os {
        None => {
            if windows_ok && !modes.contains(&true) {
                modes.push(true);
            }
            if native_os.is_some() && !modes.contains(&false) {
                modes.push(false);
            }
        }
        Some("windows") => {
            if windows_ok && !modes.contains(&true) {
                modes.push(true);
            }
        }
        Some("dos") => {
            if !modes.contains(&false) {
                modes.push(false);
            }
            if windows_ok && !modes.contains(&true) {
                modes.push(true);
            }
        }
        Some(os) => {
            if native_os == Some(os) && !modes.contains(&false) {
                modes.push(false);
            }
        }
    };
    if file.when.is_empty() {
        consider(None);
    } else {
        for w in &file.when {
            if store_matches(w.store.as_deref(), provider) {
                consider(w.os.as_deref());
            }
        }
    }
    modes
}

fn roots_from_db(entry: &DbGame, game: &GameEntry, ctx: &PathContext) -> (Vec<DetectedRoot>, Vec<DetectedRoot>) {
    // (path, kind) -> (includes, track_everything, template)
    let mut grouped: BTreeMap<(PathBuf, RootKind), (Vec<String>, bool, String)> = BTreeMap::new();
    for file in &entry.files {
        for windows_mode in applicable_modes(file, game.provider, ctx) {
            for resolved in resolve_template(&file.path, ctx, windows_mode) {
                let kind = if file.save { RootKind::Saves } else { RootKind::Config };
                let slot = grouped
                    .entry((resolved.root.clone(), kind))
                    .or_insert_with(|| (Vec::new(), false, resolved.template.clone()));
                match resolved.include {
                    // "*" beneath the root is the same as the whole folder.
                    Some(inc) if inc != "*" && inc != "**" => {
                        if !slot.0.contains(&inc) {
                            slot.0.push(inc);
                        }
                    }
                    _ => slot.1 = true,
                }
            }
        }
    }

    let mut saves = Vec::new();
    let mut configs = Vec::new();
    for ((path, kind), (include, everything, template)) in grouped {
        let include = if everything { Vec::new() } else { include };
        let stats = measure_root(&path, &include, &[]);
        let root = DetectedRoot {
            kind,
            id_hint: None,
            label_hint: None,
            path,
            include,
            exclude: Vec::new(),
            template: Some(template),
            stats,
        };
        match kind {
            RootKind::Saves => saves.push(root),
            _ => configs.push(root),
        }
    }

    // Drop save roots nested inside another whole-folder save root.
    let whole: Vec<PathBuf> = saves
        .iter()
        .filter(|r| r.include.is_empty())
        .map(|r| r.path.clone())
        .collect();
    saves.retain(|r| !whole.iter().any(|w| r.path != *w && r.path.starts_with(w)));

    // Settings roots: skip ones already covered by saves, and carve nested save folders out
    // of the ones that contain them.
    configs.retain(|c| {
        !saves
            .iter()
            .any(|s| s.include.is_empty() && c.path.starts_with(&s.path))
    });
    for c in &mut configs {
        for s in &saves {
            if s.path != c.path
                && let Ok(rel) = s.path.strip_prefix(&c.path)
            {
                c.exclude.push(normalize_rel_path(rel));
            }
        }
        if !c.exclude.is_empty() {
            c.stats = measure_root(&c.path, &c.include, &c.exclude);
        }
    }

    // Best save root first: has files, then most recently written.
    saves.sort_by(|a, b| {
        (b.stats.files > 0, b.stats.newest, b.stats.exists)
            .cmp(&(a.stats.files > 0, a.stats.newest, a.stats.exists))
            .then(a.path.cmp(&b.path))
    });
    (saves, configs)
}

fn steam_cloud_root(game: &GameEntry, ctx: &PathContext) -> Option<DetectedRoot> {
    if game.provider != Provider::Steam {
        return None;
    }
    let appid = game.app_id.as_deref()?;
    let userdata = ctx.library_root.as_deref()?.join("userdata");
    let mut best: Option<DetectedRoot> = None;
    for user in fs::read_dir(&userdata).ok()?.flatten() {
        let remote = user.path().join(appid).join("remote");
        if !remote.is_dir() {
            continue;
        }
        let stats = measure_root(&remote, &[], &[]);
        if stats.files == 0 {
            continue;
        }
        if best.as_ref().is_none_or(|b| stats.newest > b.stats.newest) {
            best = Some(DetectedRoot {
                kind: RootKind::Saves,
                id_hint: None,
                label_hint: None,
                template: tokenize_path(&remote, ctx),
                path: remote,
                include: Vec::new(),
                exclude: Vec::new(),
                stats,
            });
        }
    }
    best
}

/// Detects where `game` keeps its saves on this machine.
pub fn detect_game(game: &GameEntry, db: Option<&SaveDb>) -> Detection {
    if game.provider == Provider::RetroArch && game.retroarch.is_some() {
        return crate::retroarch::detect_retroarch_roots(game);
    }

    let ctx = PathContext::for_game(game);
    let mod_sources = mods::suggest_mod_sources(game, &ctx);
    let db_entry = db.and_then(|d| lookup_db_game(game, d));

    let (db_saves, db_configs) = db_entry
        .map(|e| roots_from_db(e, game, &ctx))
        .unwrap_or_default();

    // 1. Database, with real files on disk.
    if db_saves.iter().any(|r| r.stats.files > 0) {
        let title = db_entry.map(|e| e.title.clone());
        let found: Vec<DetectedRoot> = db_saves.into_iter().filter(|r| r.stats.files > 0).collect();
        return Detection {
            info: DetectionInfo {
                source: DetectionSource::Database,
                confidence: 97,
                reason: format!(
                    "Matched \"{}\" in the save location database.",
                    title.clone().unwrap_or_else(|| game.name.clone())
                ),
                database_title: title,
            },
            saves: found,
            configs: db_configs.into_iter().filter(|r| r.stats.files > 0).collect(),
            mod_sources,
            alternatives: Vec::new(),
        };
    }

    // 2. Steam Cloud folder with files.
    if let Some(cloud) = steam_cloud_root(game, &ctx) {
        return Detection {
            info: DetectionInfo {
                source: DetectionSource::SteamCloud,
                confidence: 85,
                reason: "Found this game's Steam Cloud save folder.".to_string(),
                database_title: db_entry.map(|e| e.title.clone()),
            },
            saves: vec![cloud],
            configs: Vec::new(),
            mod_sources,
            alternatives: Vec::new(),
        };
    }

    // 3. Smart scan.
    let guesses = heuristics::guess_save_dirs(game, &ctx);
    if let Some(best) = guesses.first().filter(|c| c.score >= AUTO_ENABLE_CONFIDENCE) {
        let stats = measure_root(&best.path, &[], &[]);
        return Detection {
            info: DetectionInfo {
                source: DetectionSource::Heuristic,
                confidence: best.score,
                reason: format!("Smart scan: {}.", best.reason),
                database_title: None,
            },
            saves: vec![DetectedRoot {
                kind: RootKind::Saves,
                id_hint: None,
                label_hint: None,
                template: tokenize_path(&best.path, &ctx),
                path: best.path.clone(),
                include: Vec::new(),
                exclude: Vec::new(),
                stats,
            }],
            configs: Vec::new(),
            mod_sources,
            alternatives: guesses.into_iter().skip(1).take(4).collect(),
        };
    }

    // 4. Database knows the location but the game has not written saves yet.
    if let Some(expected) = db_saves.into_iter().next() {
        return Detection {
            info: DetectionInfo {
                source: DetectionSource::Database,
                confidence: 70,
                reason: "Save location is known; no saves have been written yet.".to_string(),
                database_title: db_entry.map(|e| e.title.clone()),
            },
            saves: vec![expected],
            configs: Vec::new(),
            mod_sources,
            alternatives: guesses.into_iter().take(4).collect(),
        };
    }

    // 5. A weak guess the user should confirm.
    if let Some(best) = guesses.first() {
        let stats = measure_root(&best.path, &[], &[]);
        return Detection {
            info: DetectionInfo {
                source: DetectionSource::Heuristic,
                confidence: best.score,
                reason: format!("Best guess, please check: {}.", best.reason),
                database_title: None,
            },
            saves: vec![DetectedRoot {
                kind: RootKind::Saves,
                id_hint: None,
                label_hint: None,
                template: tokenize_path(&best.path, &ctx),
                path: best.path.clone(),
                include: Vec::new(),
                exclude: Vec::new(),
                stats,
            }],
            configs: Vec::new(),
            mod_sources,
            alternatives: guesses.into_iter().skip(1).take(4).collect(),
        };
    }

    Detection {
        info: DetectionInfo {
            source: DetectionSource::Heuristic,
            confidence: 0,
            reason: if db_entry.is_some() {
                "This game is known, but it has not been started on this PC yet. Play once and SaveVault picks it up."
                    .to_string()
            } else {
                "No saves found yet. Play once and SaveVault will learn where this game saves."
                    .to_string()
            },
            database_title: db_entry.map(|e| e.title.clone()),
        },
        mod_sources,
        ..Default::default()
    }
}

fn unique_root_id(existing: &[SaveRoot], base: &str) -> String {
    if !existing.iter().any(|r| r.id == base) && base != crate::models::PRIMARY_ROOT_ID {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|id| !existing.iter().any(|r| &r.id == id))
        .unwrap_or_else(|| base.to_string())
}

/// Writes a detection into a game entry: primary save folder, extra save folders, optional
/// settings folders, and mod sources. Locations the user set by hand are left alone.
pub fn apply_detection(game: &mut GameEntry, det: &Detection, include_config: bool) {
    let manual = game
        .detection
        .as_ref()
        .is_some_and(|d| d.source == DetectionSource::Manual);
    if game.mod_sources.is_empty() {
        game.mod_sources = det.mod_sources.clone();
    }
    if manual {
        return;
    }
    let Some(primary) = det.saves.first() else {
        game.detection = Some(det.info.clone());
        return;
    };

    game.save_dir = primary.path.clone();
    game.save_dir_template = primary.template.clone();
    game.sync_selection = if primary.include.is_empty() {
        SyncSelectionMode::EntireFolder
    } else {
        SyncSelectionMode::SelectedFiles(primary.include.clone())
    };
    game.detection = Some(det.info.clone());

    // Keep roots the user added themselves (no template, or user-labelled mod roots).
    let mut roots: Vec<SaveRoot> = game
        .extra_roots
        .iter()
        .filter(|r| r.kind == RootKind::Mods || r.template.is_none())
        .cloned()
        .collect();
    let previously_enabled: HashMap<PathBuf, bool> = game
        .extra_roots
        .iter()
        .map(|r| (r.path.clone(), r.enabled))
        .collect();

    for extra in det.saves.iter().skip(1) {
        if roots.iter().any(|r| r.path == extra.path && r.kind == RootKind::Saves) {
            continue;
        }
        let id = extra
            .id_hint
            .as_deref()
            .filter(|h| *h != crate::models::PRIMARY_ROOT_ID && !roots.iter().any(|r| r.id == *h))
            .map(str::to_string)
            .unwrap_or_else(|| unique_root_id(&roots, "saves"));
        let label = extra
            .label_hint
            .clone()
            .unwrap_or_else(|| {
                extra
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "More saves".to_string())
            });
        roots.push(SaveRoot {
            id,
            label,
            kind: RootKind::Saves,
            path: extra.path.clone(),
            include: extra.include.clone(),
            exclude: extra.exclude.clone(),
            // Very large secondary folders (replays, caches some games file under "saves")
            // are listed but left off until the user opts in.
            enabled: previously_enabled
                .get(&extra.path)
                .copied()
                .unwrap_or(extra.stats.bytes <= LARGE_SECONDARY_ROOT_BYTES),
            shared_across_profiles: false,
            template: extra.template.clone(),
        });
    }
    for cfg in &det.configs {
        if roots.iter().any(|r| r.path == cfg.path && r.kind == RootKind::Config) {
            continue;
        }
        let id = unique_root_id(&roots, "config");
        roots.push(SaveRoot {
            id,
            label: "Settings".to_string(),
            kind: RootKind::Config,
            path: cfg.path.clone(),
            include: cfg.include.clone(),
            exclude: cfg.exclude.clone(),
            enabled: previously_enabled.get(&cfg.path).copied().unwrap_or(include_config),
            // Graphics / key binding settings normally belong to the machine, not the character.
            shared_across_profiles: true,
            template: cfg.template.clone(),
        });
    }
    game.extra_roots = roots;
}

/// True when `det` points somewhere else than what the game currently tracks, and is
/// trustworthy enough to suggest.
pub fn detection_differs(game: &GameEntry, det: &Detection) -> bool {
    let Some(primary) = det.saves.first() else {
        return false;
    };
    if det.info.confidence < AUTO_ENABLE_CONFIDENCE {
        return false;
    }
    let current_include: Vec<String> = match &game.sync_selection {
        SyncSelectionMode::EntireFolder => Vec::new(),
        SyncSelectionMode::SelectedFiles(l) => l.clone(),
    };
    if primary.path == game.save_dir {
        return primary.include != current_include;
    }
    // Already tracking one of the other known save folders: nothing to improve.
    !det.saves.iter().any(|r| r.path == game.save_dir)
}

/// Scans every enabled launcher and detects save locations for each installed game.
pub fn scan_installed_games(
    libraries: &[ProviderLibraryConfig],
    db: Option<&SaveDb>,
    include_config: bool,
) -> Vec<(GameEntry, Detection)> {
    scan_provider_libraries(libraries)
        .into_iter()
        .map(|mut game| {
            let legacy_dir = std::mem::take(&mut game.save_dir);
            let det = detect_game(&game, db);
            apply_detection(&mut game, &det, include_config);
            if !det.has_location() {
                // Keep the launcher-level guess only when it really exists.
                if legacy_dir.is_dir() && game.install_dir.as_deref() != Some(legacy_dir.as_path()) {
                    game.save_dir = legacy_dir;
                    game.detection = Some(DetectionInfo {
                        source: DetectionSource::Heuristic,
                        confidence: 40,
                        reason: "Best guess from the launcher's folder layout, please check.".to_string(),
                        database_title: None,
                    });
                }
            }
            game.enabled = det.info.confidence >= AUTO_ENABLE_CONFIDENCE;
            (game, det)
        })
        .collect()
}

/// What a scan changed in the configuration.
#[derive(Debug, Clone, Default)]
pub struct ScanSummary {
    pub installed: usize,
    pub added: Vec<String>,
    /// Games that were waiting for a location (cloud placeholders, not-yet-played) and now have one.
    pub located: Vec<String>,
    /// Better locations for games that are already tracked (applied only on request).
    pub suggestions: HashMap<String, Detection>,
    pub needs_review: Vec<String>,
    /// Installed games that were found but not added (automatic tracking is off).
    pub untracked: Vec<GameEntry>,
}

/// Merges scan results into the configuration without overriding the user's choices.
pub fn merge_scan_results(config: &mut AppConfig, scanned: Vec<(GameEntry, Detection)>) -> ScanSummary {
    let mut summary = ScanSummary {
        installed: scanned.len(),
        ..Default::default()
    };
    for (found, det) in scanned {
        if let Some(existing) = config.games.iter_mut().find(|g| g.id == found.id) {
            // Refresh install facts; they are never user-edited.
            existing.install_dir = found.install_dir.clone();
            existing.prefix_dir = found.prefix_dir.clone();
            existing.library_root = found.library_root.clone();
            if existing.app_id.is_none() {
                existing.app_id = found.app_id.clone();
            }
            if existing.mod_sources.is_empty() {
                existing.mod_sources = found.mod_sources.clone();
            }
            if existing.retroarch.is_none() && found.retroarch.is_some() {
                existing.retroarch = found.retroarch.clone();
            }

            let waiting = existing.is_placeholder()
                || (!existing.save_dir.exists()
                    && existing
                        .detection
                        .as_ref()
                        .is_none_or(|d| d.source != DetectionSource::Manual));
            if waiting && det.has_location() {
                let keep_profiles = (
                    existing.profiles.clone(),
                    existing.active_profile.clone(),
                    existing.main_profile_name.clone(),
                );
                // Cloud placeholders carry their own root templates: resolve those first so
                // files land exactly where the other machine had them.
                let ctx = PathContext::for_game(existing);
                let from_template = existing
                    .save_dir_template
                    .as_deref()
                    .and_then(|t| template::resolve_own_template(t, &ctx));
                if let Some(path) = from_template {
                    existing.save_dir = path;
                    existing.detection = Some(if det.info.confidence >= AUTO_ENABLE_CONFIDENCE {
                        det.info.clone()
                    } else {
                        DetectionInfo {
                            source: DetectionSource::Cloud,
                            confidence: 80,
                            reason: "Same location your other PC uses for this game.".to_string(),
                            database_title: None,
                        }
                    });
                    if existing.mod_sources.is_empty() {
                        existing.mod_sources = det.mod_sources.clone();
                    }
                } else {
                    apply_detection(existing, &det, config.include_config_roots);
                }
                for root in &mut existing.extra_roots {
                    if root.path.as_os_str().is_empty()
                        && let Some(p) = root
                            .template
                            .as_deref()
                            .and_then(|t| template::resolve_own_template(t, &ctx))
                    {
                        root.path = p;
                    }
                }
                (existing.profiles, existing.active_profile, existing.main_profile_name) = keep_profiles;
                if existing.detection.as_ref().is_none_or(|d| d.confidence >= AUTO_ENABLE_CONFIDENCE) {
                    existing.enabled = true;
                }
                summary.located.push(existing.id.clone());
            } else if detection_differs(existing, &det) && det.has_files() {
                summary.suggestions.insert(existing.id.clone(), det);
            } else if existing.detection.is_none() && det.has_location() {
                // Same place the older version picked: record why we trust it.
                existing.detection = Some(det.info.clone());
                existing.save_dir_template = det.saves[0].template.clone();
            }
            continue;
        }

        if config.ignored_game_ids.contains(&found.id) {
            continue;
        }
        if !config.auto_track_new_games {
            summary.untracked.push(found);
            continue;
        }
        if found
            .detection
            .as_ref()
            .is_none_or(|d| d.confidence < AUTO_ENABLE_CONFIDENCE)
        {
            summary.needs_review.push(found.id.clone());
        }
        summary.added.push(found.id.clone());
        config.games.push(found);
    }
    summary
}
