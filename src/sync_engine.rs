//! Sync engine.
//!
//! The local git repository is a *mirror*: for every tracked game it holds a copy of the
//! live save files of each profile. A sync pass compares three things per game — the live
//! files, the mirror, and what both looked like after the previous pass — and then either
//! uploads (live → mirror → commit → push), downloads (mirror → live, after a safety
//! backup), does nothing, or reports a conflict. Nothing here ever deletes live saves without
//! first zipping them into the safety folder.

use crate::auth::{RepoEnsureResult, ensure_remote_repo_exists, remote_url_for_auth};
use crate::chunker::{
    ChunkManifest, copy_or_split_file, is_chunk_part_file_name, is_manifest_file_name,
    normalize_rel_path, path_matches_selection, reassemble_from_manifest, sha256_file, to_hex,
};
use crate::detect::mods::scan_mod_fingerprint;
use crate::git_ops::{
    IntegrationKind, IntegrationOutcome, PROFILES_DIR, ROOTS_DIR, SNAPSHOT_META_FILE,
    commit_all_if_changed, extract_label_from_commit_message, extract_path_at, fetch_remote,
    has_origin_remote, head_commit, init_or_open_local_repo, integrate_remote,
    is_non_fast_forward_error, list_commits_for_path, list_commits_for_paths, list_tree,
    push_to_remote, read_file_at,
};
use crate::models::{
    AppConfig, CatalogRoot, CommitInfo, ConflictInfo, ConflictPolicy, DetectionInfo,
    DetectionSource, GameEntry, LargeFileWarning, LogLevel, ModFingerprint, PRIMARY_ROOT_ID,
    RepoGameCatalogEntry, RepoMetadataManifest, RootKind, SaveRoot, SaveStateLabel, SyncRate,
    SyncSelectionMode,
};
use crate::pattern::PathFilter;
use crate::safety::create_safety_backup;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;
use tempfile::TempDir;
use walkdir::WalkDir;

/// Metadata filename stored at the root of the save repository for system reset re-instatement and labels.
pub const REPO_METADATA_FILE: &str = "sgm_repo_metadata.json";
const EMPTY_DIGEST: &str = "empty";

/// Summary of a synchronization run.
#[derive(Debug, Clone, Default)]
pub struct SyncOutcome {
    /// Games that were examined and are now in sync (uploaded, downloaded, or already equal).
    pub synced_games: usize,
    pub committed_sha: Option<String>,
    pub pushed_to_remote: bool,
    pub push_error: Option<String>,
    pub cross_provider_synced: Vec<String>,
    pub warnings: Vec<LargeFileWarning>,
    pub notes: Vec<String>,
    /// Ids of games whose live saves were copied into the repository.
    pub uploaded: Vec<String>,
    /// Ids of games whose live saves were replaced by newer saves from the repository.
    pub downloaded: Vec<String>,
    /// Ids of games that needed nothing.
    pub unchanged: Vec<String>,
    /// Ids of games where both sides changed and the user has to choose.
    pub conflicts: Vec<String>,
    /// `(game id, reason)` for games that could not be processed.
    pub skipped: Vec<(String, String)>,
    /// Games that appeared from the cloud catalog during this pass.
    pub adopted_from_cloud: Vec<String>,
}

/// Summary of removing a tracked game from configuration (and optionally from the Git repository).
#[derive(Debug, Clone, Default)]
pub struct RemoveGameOutcome {
    pub game_id: String,
    pub game_name: String,
    pub repo_subpath: PathBuf,
    pub removed_from_git: bool,
    pub committed_sha: Option<String>,
    pub pushed_to_remote: bool,
    pub push_error: Option<String>,
}

// ---- Repository layout ----------------------------------------------------------------

/// Computes the relative path inside the git repository for a game.
///
/// When `separate_folder_per_provider` is `true`, produces `<provider>/<game_slug>`
/// (for example `epic_games/metro_exodus` and `steam/metro_exodus`).
/// When `false`, produces `<game_slug>`.
pub fn repo_subpath_for_game(game: &GameEntry, separate_folder_per_provider: bool) -> PathBuf {
    if separate_folder_per_provider {
        PathBuf::from(game.provider.slug()).join(&game.game_slug)
    } else {
        PathBuf::from(&game.game_slug)
    }
}

/// Repository folder of one profile ("" = main profile, which lives at the game folder itself).
pub fn profile_base_dir(game: &GameEntry, separate: bool, profile_id: &str) -> PathBuf {
    let game_dir = repo_subpath_for_game(game, separate);
    if profile_id.is_empty() {
        game_dir
    } else {
        game_dir.join(PROFILES_DIR).join(profile_id)
    }
}

/// Repository folder that mirrors `root` for `profile_id`.
pub fn root_repo_dir(game: &GameEntry, separate: bool, profile_id: &str, root: &SaveRoot) -> PathBuf {
    let base = if root.shared_across_profiles {
        repo_subpath_for_game(game, separate)
    } else {
        profile_base_dir(game, separate, profile_id)
    };
    if root.id == PRIMARY_ROOT_ID {
        base
    } else {
        base.join(ROOTS_DIR).join(&root.id)
    }
}

/// Git pathspecs selecting the history of one profile.
pub fn profile_pathspecs(game: &GameEntry, separate: bool, profile_id: &str) -> Vec<PathBuf> {
    let game_dir = repo_subpath_for_game(game, separate);
    if profile_id.is_empty() {
        vec![
            game_dir.clone(),
            PathBuf::from(format!(
                ":(exclude){}/{PROFILES_DIR}",
                normalize_rel_path(&game_dir)
            )),
        ]
    } else {
        vec![game_dir.join(PROFILES_DIR).join(profile_id)]
    }
}

fn is_reserved_name(name: &str) -> bool {
    name == PROFILES_DIR || name == ROOTS_DIR || name == SNAPSHOT_META_FILE
}

fn first_component_reserved(rel: &Path) -> bool {
    rel.components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .is_some_and(is_reserved_name)
}

// ---- File collection ------------------------------------------------------------------

/// Lists all relative file paths (forward-slash normalized) inside `save_dir`.
pub fn list_all_save_files(save_dir: &Path) -> Vec<String> {
    if !save_dir.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(save_dir)
        .into_iter()
        .filter_entry(|e| e.file_name().to_str() != Some(".git"))
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file()
            && let Ok(rel) = entry.path().strip_prefix(save_dir)
        {
            files.push(normalize_rel_path(rel));
        }
    }
    files.sort();
    files
}

fn is_transient_dir_name(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "Cache"
            | "Logs"
            | "ShaderCache"
            | "D3DSCache"
            | "CrashReports"
            | ".gogdl-download-cache"
            | "Demos"
    )
}

/// Collects the relative paths under `root.path` that belong to the backup.
pub fn collect_root_files(
    root: &SaveRoot,
    game_excludes: &[String],
    global_excludes: &[String],
) -> Result<Vec<PathBuf>> {
    if root.path.as_os_str().is_empty() || !root.path.exists() {
        return Ok(Vec::new());
    }
    let mut excludes: Vec<&str> = root.exclude.iter().map(String::as_str).collect();
    excludes.extend(game_excludes.iter().map(String::as_str));
    excludes.extend(global_excludes.iter().map(String::as_str));
    excludes.extend(crate::pattern::BUILTIN_EXCLUDES);
    let filter = PathFilter::new(&root.include, &excludes);
    let whole_folder = filter.includes_everything();

    let mut selected = Vec::new();
    for entry in WalkDir::new(&root.path)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            if name == ".git" {
                return false;
            }
            // Whole-folder roots skip caches and logs; explicit selections are honoured as-is.
            !(whole_folder && e.depth() > 0 && e.file_type().is_dir() && is_transient_dir_name(name))
        })
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(&root.path)?.to_path_buf();
        if filter.accepts(&normalize_rel_path(&rel)) {
            selected.push(rel);
        }
    }
    selected.sort();
    Ok(selected)
}

/// Collects relative `PathBuf`s under `save_dir` that satisfy `selection`.
pub fn collect_selected_files(save_dir: &Path, selection: &SyncSelectionMode) -> Result<Vec<PathBuf>> {
    let root = SaveRoot {
        id: PRIMARY_ROOT_ID.to_string(),
        path: save_dir.to_path_buf(),
        include: match selection {
            SyncSelectionMode::EntireFolder => Vec::new(),
            SyncSelectionMode::SelectedFiles(l) => l.clone(),
        },
        enabled: true,
        ..Default::default()
    };
    collect_root_files(&root, &[], &[])
}

// ---- Scheduling -----------------------------------------------------------------------

/// Returns IDs of all enabled games whose effective sync rate is `SyncRate::OnStartup`.
pub fn games_due_on_startup(config: &AppConfig) -> Vec<String> {
    if !config.sync_enabled {
        return Vec::new();
    }
    config
        .games
        .iter()
        .filter(|g| g.enabled && g.effective_sync_rate(&config.global_sync_rate) == SyncRate::OnStartup)
        .map(|g| g.id.clone())
        .collect()
}

/// Returns IDs of all enabled games on a periodic schedule whose interval has elapsed as of `now`.
pub fn games_due_periodic(config: &AppConfig, now: DateTime<Utc>) -> Vec<String> {
    if !config.sync_enabled {
        return Vec::new();
    }
    config
        .games
        .iter()
        .filter(|g| {
            if !g.enabled {
                return false;
            }
            match g.effective_sync_rate(&config.global_sync_rate) {
                SyncRate::EveryMinutes(mins) => match g.last_synced_at {
                    None => true,
                    Some(last) => now.signed_duration_since(last) >= Duration::minutes(mins.max(1) as i64),
                },
                SyncRate::OnStartup | SyncRate::Manual => false,
            }
        })
        .map(|g| g.id.clone())
        .collect()
}

// ---- Fingerprints ---------------------------------------------------------------------

/// Cheap description of a game's live save files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveState {
    pub fingerprint: String,
    pub files: usize,
    pub bytes: u64,
    pub newest: Option<SystemTime>,
}

/// Stat-based fingerprint of the live files of every active root, combined with the
/// settings that influence how they are mirrored (so changing those forces a re-upload).
pub fn live_state(config: &AppConfig, game: &GameEntry) -> LiveState {
    let mut hasher = Sha256::new();
    hasher.update(format!(
        "split={};threshold={};chunk={}\n",
        config.split_large_saves, config.lfs_warning_threshold_mb, config.chunk_size_mb
    ));
    let mut state = LiveState::default();
    for root in game.active_roots() {
        hasher.update(format!(
            "root={}|{}|{:?}|{:?}|{}\n",
            root.id,
            root.path.display(),
            root.include,
            root.exclude,
            root.shared_across_profiles
        ));
        let files = collect_root_files(&root, &game.exclude_globs, &config.global_exclude_globs)
            .unwrap_or_default();
        for rel in files {
            let Ok(meta) = fs::metadata(root.path.join(&rel)) else {
                continue;
            };
            let mtime = meta.modified().ok();
            let nanos = mtime
                .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            hasher.update(format!("{}\0{}\0{}\n", normalize_rel_path(&rel), meta.len(), nanos));
            state.files += 1;
            state.bytes += meta.len();
            if let Some(m) = mtime {
                state.newest = Some(state.newest.map_or(m, |c| c.max(m)));
            }
        }
    }
    state.fingerprint = to_hex(&hasher.finalize());
    state
}

/// Repository tree entries (`path -> blob hash`) used to compute mirror digests.
pub type TreeListing = BTreeMap<String, String>;

/// Name of the live file a repository entry belongs to (split files map to their original).
fn original_rel_name(rel: &str) -> &str {
    if let Some(i) = rel.find(".sgm_part.") {
        &rel[..i]
    } else {
        rel.strip_suffix(".sgm_manifest.json").unwrap_or(rel)
    }
}

/// Repository entries of one root that are in scope under `filter` (files that are excluded
/// today but were committed by an older version are ignored).
fn tree_entries_for_root<'a>(
    tree: &'a TreeListing,
    repo_dir: &Path,
    filter: &PathFilter,
) -> Vec<(&'a str, &'a str)> {
    let prefix = format!("{}/", normalize_rel_path(repo_dir));
    tree.range(prefix.clone()..)
        .take_while(|(p, _)| p.starts_with(&prefix))
        .filter_map(|(p, h)| {
            let rel = &p[prefix.len()..];
            let first = rel.split('/').next().unwrap_or("");
            if is_reserved_name(first) || !filter.accepts(original_rel_name(rel)) {
                None
            } else {
                Some((rel, h.as_str()))
            }
        })
        .collect()
}

/// Content digest of the repository copy of `profile_id`, plus its file count.
pub fn mirror_digest(tree: &TreeListing, config: &AppConfig, game: &GameEntry, profile_id: &str) -> (String, usize) {
    let mut hasher = Sha256::new();
    let mut count = 0usize;
    for root in game.active_roots() {
        let dir = root_repo_dir(game, config.separate_folder_per_provider, profile_id, &root);
        let filter = root_filter(config, game, &root);
        for (rel, hash) in tree_entries_for_root(tree, &dir, &filter) {
            hasher.update(format!("{}\0{rel}\0{hash}\n", root.id));
            count += 1;
        }
    }
    if count == 0 {
        (EMPTY_DIGEST.to_string(), 0)
    } else {
        (to_hex(&hasher.finalize()), count)
    }
}

fn current_tree(repo: &Path) -> TreeListing {
    if head_commit(repo).is_none() {
        return TreeListing::new();
    }
    list_tree(repo, "HEAD").into_iter().collect()
}

/// Computes git blob hashes for files without writing them to the object store.
fn hash_objects(repo: &Path, files: &[PathBuf]) -> Result<Vec<String>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.autocrlf=false", "hash-object", "--no-filters", "--stdin-paths"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to run git hash-object")?;
    {
        let mut stdin = child.stdin.take().context("git hash-object has no stdin")?;
        for f in files {
            writeln!(stdin, "{}", f.display())?;
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("git hash-object failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .collect())
}

/// True when the live files are byte-for-byte what the repository already holds.
fn live_matches_mirror(config: &AppConfig, game: &GameEntry, tree: &TreeListing) -> bool {
    let repo = &config.local_repo_path;
    for root in game.active_roots() {
        let dir = root_repo_dir(game, config.separate_folder_per_provider, &game.active_profile, &root);
        let filter = root_filter(config, game, &root);
        let mirror: BTreeMap<&str, &str> = tree_entries_for_root(tree, &dir, &filter).into_iter().collect();
        let Ok(live) = collect_root_files(&root, &game.exclude_globs, &config.global_exclude_globs) else {
            return false;
        };
        let mut expected_mirror_files = 0usize;
        let mut plain: Vec<(String, PathBuf)> = Vec::new();
        for rel in &live {
            let rel_str = normalize_rel_path(rel);
            let manifest_rel = format!("{rel_str}.sgm_manifest.json");
            if mirror.contains_key(manifest_rel.as_str()) {
                // Split file: compare against the manifest's whole-file hash.
                let manifest_path = repo.join(&dir).join(&manifest_rel);
                let Some(manifest) = fs::read_to_string(&manifest_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<ChunkManifest>(&raw).ok())
                else {
                    return false;
                };
                if sha256_file(&root.path.join(rel)).ok().as_deref() != Some(manifest.full_sha256.as_str()) {
                    return false;
                }
                expected_mirror_files += 1 + manifest.chunks.len();
            } else if mirror.contains_key(rel_str.as_str()) {
                plain.push((rel_str, root.path.join(rel)));
                expected_mirror_files += 1;
            } else {
                return false;
            }
        }
        if expected_mirror_files != mirror.len() {
            return false;
        }
        let paths: Vec<PathBuf> = plain.iter().map(|(_, p)| p.clone()).collect();
        let Ok(hashes) = hash_objects(repo, &paths) else {
            return false;
        };
        if hashes.len() != plain.len() {
            return false;
        }
        for ((rel, _), hash) in plain.iter().zip(hashes) {
            if mirror.get(rel.as_str()).copied() != Some(hash.as_str()) {
                return false;
            }
        }
    }
    true
}

/// Whether the place a game's saves belong to exists on this machine right now (the game is
/// installed and its drive is connected), so that creating the save folder there is safe.
pub fn location_available(game: &GameEntry) -> bool {
    if game.is_placeholder() {
        return false;
    }
    if game.save_dir.exists() {
        return true;
    }
    if let Some(prefix) = &game.prefix_dir {
        return prefix.exists();
    }
    if let Some(install) = &game.install_dir {
        return install.exists();
    }
    game.save_dir
        .ancestors()
        .skip(1)
        .take(3)
        .any(|a| !a.as_os_str().is_empty() && a.exists())
}

/// Plain-language state of a tracked game, cheap enough to compute for the whole list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameHealth {
    /// Saves match the latest snapshot.
    Protected,
    /// Saves changed since the last snapshot.
    ChangesPending,
    /// Never snapshotted yet.
    NeverBackedUp,
    /// The cloud and this machine disagree; the user has to choose.
    Conflict,
    /// Location is a low-confidence guess.
    NeedsReview,
    /// Location known, no save files yet.
    NoSavesYet,
    /// Known from the cloud catalog but not installed here.
    NotInstalled,
    /// Drive / prefix not present right now.
    Unavailable,
    /// Game is running; snapshot follows when it closes.
    Running,
    /// Tracking switched off by the user.
    Paused,
}

impl GameHealth {
    pub fn label(&self) -> &'static str {
        match self {
            GameHealth::Protected => "Protected",
            GameHealth::ChangesPending => "New progress",
            GameHealth::NeverBackedUp => "Not backed up yet",
            GameHealth::Conflict => "Needs your choice",
            GameHealth::NeedsReview => "Check location",
            GameHealth::NoSavesYet => "No saves yet",
            GameHealth::NotInstalled => "Not installed here",
            GameHealth::Unavailable => "Drive not connected",
            GameHealth::Running => "Playing now",
            GameHealth::Paused => "Paused",
        }
    }

    pub fn explain(&self) -> &'static str {
        match self {
            GameHealth::Protected => "Your saves match the latest snapshot.",
            GameHealth::ChangesPending => "Saves changed since the last snapshot.",
            GameHealth::NeverBackedUp => "A first snapshot has not been taken yet.",
            GameHealth::Conflict => "This PC and the cloud both have new saves. Pick which to keep.",
            GameHealth::NeedsReview => "SaveVault is not sure this is the right folder.",
            GameHealth::NoSavesYet => "The game has not written any saves yet.",
            GameHealth::NotInstalled => "Saved on another PC. Install the game here to get the saves.",
            GameHealth::Unavailable => "The game's drive or folder is not available right now.",
            GameHealth::Running => "A snapshot is taken when you close the game.",
            GameHealth::Paused => "Not being backed up.",
        }
    }
}

/// Derives a game's health from the configuration and (optionally) a fresh live state.
pub fn game_health(game: &GameEntry, running: bool, live: Option<&LiveState>) -> GameHealth {
    if game.sync_state.conflict.is_some() {
        return GameHealth::Conflict;
    }
    if game.is_placeholder() {
        return if game
            .detection
            .as_ref()
            .is_some_and(|d| d.source == DetectionSource::Cloud)
        {
            GameHealth::NotInstalled
        } else {
            GameHealth::NoSavesYet
        };
    }
    if running {
        return GameHealth::Running;
    }
    if !game.save_dir.exists() && !location_available(game) {
        return GameHealth::Unavailable;
    }
    if game
        .detection
        .as_ref()
        .is_some_and(|d| d.source != DetectionSource::Manual && d.confidence < crate::detect::AUTO_ENABLE_CONFIDENCE)
        && game.sync_state.mirror_digest.is_none()
    {
        return GameHealth::NeedsReview;
    }
    if !game.enabled {
        return GameHealth::Paused;
    }
    match live {
        Some(l) if l.files == 0 => GameHealth::NoSavesYet,
        Some(l) => {
            if game.sync_state.live_fingerprint.as_deref() == Some(l.fingerprint.as_str()) {
                GameHealth::Protected
            } else if game.sync_state.live_fingerprint.is_none() {
                // No bookkeeping yet: either never snapshotted, or last handled by an older
                // version (the next check settles it).
                if game.last_synced_at.is_none() {
                    GameHealth::NeverBackedUp
                } else {
                    GameHealth::Protected
                }
            } else {
                GameHealth::ChangesPending
            }
        }
        None => {
            if game.last_synced_at.is_some() {
                GameHealth::Protected
            } else {
                GameHealth::NeverBackedUp
            }
        }
    }
}

// ---- Cross-provider reconciliation ----------------------------------------------------

/// Keeps local save files synchronized across all enabled providers sharing `game_slug`
/// when `sync_across_providers` is enabled, propagating the newest save state to the others.
pub fn reconcile_cross_provider_saves(
    games: &mut [GameEntry],
    game_slug: &str,
) -> Result<Option<String>> {
    reconcile_cross_provider_inner(games, game_slug, &mut |_| {})
}

fn reconcile_cross_provider_inner(
    games: &mut [GameEntry],
    game_slug: &str,
    before_overwrite: &mut dyn FnMut(&GameEntry),
) -> Result<Option<String>> {
    let linked_indices: Vec<usize> = games
        .iter()
        .enumerate()
        .filter(|(_, g)| {
            g.enabled && g.game_slug == game_slug && g.sync_across_providers && !g.is_placeholder()
        })
        .map(|(i, _)| i)
        .collect();

    if linked_indices.len() < 2 {
        return Ok(None);
    }

    struct ProviderSnapshot {
        index: usize,
        files: Vec<PathBuf>,
        latest_mtime: Option<SystemTime>,
    }

    let mut snapshots = Vec::new();
    for &idx in &linked_indices {
        let game = &games[idx];
        let files = collect_selected_files(&game.save_dir, &game.sync_selection)?;
        let mut latest: Option<SystemTime> = None;
        for rel in &files {
            if let Ok(meta) = fs::metadata(game.save_dir.join(rel))
                && let Ok(mtime) = meta.modified()
            {
                latest = Some(match latest {
                    Some(cur) if cur >= mtime => cur,
                    _ => mtime,
                });
            }
        }
        snapshots.push(ProviderSnapshot {
            index: idx,
            files,
            latest_mtime: latest,
        });
    }

    // Pick the snapshot with existing files and the newest modification timestamp.
    let winner_pos = snapshots
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.files.is_empty())
        .max_by(|(_, a), (_, b)| a.latest_mtime.cmp(&b.latest_mtime))
        .map(|(pos, _)| pos);

    let Some(winner_pos) = winner_pos else {
        return Ok(None);
    };

    let src_idx = snapshots[winner_pos].index;
    let src_save_dir = games[src_idx].save_dir.clone();
    let src_provider_name = games[src_idx].provider.display_name().to_string();
    let src_files = snapshots[winner_pos].files.clone();
    let src_set: BTreeSet<String> = src_files.iter().map(|p| normalize_rel_path(p)).collect();

    let mut copied_any = false;

    for &dst_idx in &linked_indices {
        if dst_idx == src_idx {
            continue;
        }
        let dst_save_dir = games[dst_idx].save_dir.clone();
        let dst_selection = games[dst_idx].sync_selection.clone();

        // Work out what would change before touching anything.
        let mut to_copy: Vec<&PathBuf> = Vec::new();
        for rel in &src_files {
            if !path_matches_selection(rel, &dst_selection) {
                continue;
            }
            let src_file = src_save_dir.join(rel);
            let dst_file = dst_save_dir.join(rel);
            let same = dst_file.is_file() && {
                let src_len = fs::metadata(&src_file).map(|m| m.len()).unwrap_or(u64::MAX);
                let dst_len = fs::metadata(&dst_file).map(|m| m.len()).unwrap_or(u64::MAX - 1);
                src_len == dst_len && sha256_file(&src_file).ok() == sha256_file(&dst_file).ok()
            };
            if !same {
                to_copy.push(rel);
            }
        }
        let stale: Vec<PathBuf> = collect_selected_files(&dst_save_dir, &dst_selection)?
            .into_iter()
            .filter(|rel| !src_set.contains(&normalize_rel_path(rel)))
            .collect();
        if to_copy.is_empty() && stale.is_empty() {
            continue;
        }

        before_overwrite(&games[dst_idx]);
        fs::create_dir_all(&dst_save_dir).with_context(|| {
            format!(
                "Failed to create cross-provider destination save dir {}",
                dst_save_dir.display()
            )
        })?;
        for rel in to_copy {
            let src_file = src_save_dir.join(rel);
            let dst_file = dst_save_dir.join(rel);
            if let Some(parent) = dst_file.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src_file, &dst_file).with_context(|| {
                format!(
                    "Failed to sync save file {} -> {}",
                    src_file.display(),
                    dst_file.display()
                )
            })?;
        }
        for rel in stale {
            let path = dst_save_dir.join(&rel);
            if path.is_file() {
                fs::remove_file(&path)?;
            }
        }
        copied_any = true;
    }

    if copied_any {
        Ok(Some(src_provider_name))
    } else {
        Ok(None)
    }
}

// ---- Portable repository catalog ------------------------------------------------------

/// Builds the portable `RepoMetadataManifest` from the current `AppConfig`.
pub fn build_repo_metadata_manifest(config: &AppConfig) -> RepoMetadataManifest {
    let mut games: Vec<RepoGameCatalogEntry> = config
        .games
        .iter()
        .map(|g| RepoGameCatalogEntry {
            id: g.id.clone(),
            name: g.name.clone(),
            game_slug: g.game_slug.clone(),
            provider: g.provider,
            app_id: g.app_id.clone(),
            repo_subpath: normalize_rel_path(&repo_subpath_for_game(
                g,
                config.separate_folder_per_provider,
            )),
            sync_selection: g.sync_selection.clone(),
            sync_across_providers: g.sync_across_providers,
            active_save_label: g.active_save_label.clone(),
            save_dir_template: g.save_dir_template.clone(),
            extra_roots: g
                .extra_roots
                .iter()
                .filter(|r| r.enabled)
                .map(|r| CatalogRoot {
                    id: r.id.clone(),
                    label: r.label.clone(),
                    kind: r.kind,
                    template: r.template.clone(),
                    include: r.include.clone(),
                    exclude: r.exclude.clone(),
                    shared_across_profiles: r.shared_across_profiles,
                })
                .collect(),
            exclude_globs: g.exclude_globs.clone(),
            profiles: g.profiles.clone(),
            main_profile_name: g.main_profile_name.clone(),
        })
        .collect();
    // Stable order keeps the file identical across machines.
    games.sort_by(|a, b| a.id.cmp(&b.id));

    let mut labels = config.save_state_labels.clone();
    labels.sort_by(|a, b| {
        (&a.game_id, &a.commit_hash, a.created_at).cmp(&(&b.game_id, &b.commit_hash, b.created_at))
    });

    RepoMetadataManifest {
        version: 2,
        separate_folder_per_provider: config.separate_folder_per_provider,
        games,
        save_state_labels: labels,
    }
}

/// Writes `sgm_repo_metadata.json` inside `config.local_repo_path`.
pub fn write_repo_metadata_manifest(config: &AppConfig) -> Result<()> {
    fs::create_dir_all(&config.local_repo_path)?;
    let manifest = build_repo_metadata_manifest(config);
    let json = serde_json::to_string_pretty(&manifest)?;
    let path = config.local_repo_path.join(REPO_METADATA_FILE);
    if fs::read_to_string(&path).ok().as_deref() != Some(json.as_str()) {
        fs::write(path, json)?;
    }
    Ok(())
}

/// Reads `sgm_repo_metadata.json` from `repo_path` if present and valid.
pub fn read_repo_metadata_manifest(repo_path: &Path) -> Option<RepoMetadataManifest> {
    let path = repo_path.join(REPO_METADATA_FILE);
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Merges a catalog written by another machine into the local configuration: unknown games
/// become placeholders (filled in once the game is found here), and labels / profiles are
/// unioned. Returns the ids of newly added games.
pub fn adopt_catalog(config: &mut AppConfig, catalog: &RepoMetadataManifest) -> Vec<String> {
    let mut added = Vec::new();
    for entry in &catalog.games {
        if let Some(existing) = config.games.iter_mut().find(|g| g.id == entry.id) {
            for p in &entry.profiles {
                if !existing.profiles.iter().any(|q| q.id == p.id) {
                    existing.profiles.push(p.clone());
                }
            }
            if existing.main_profile_name.is_none() {
                existing.main_profile_name = entry.main_profile_name.clone();
            }
            if existing.save_dir_template.is_none() {
                existing.save_dir_template = entry.save_dir_template.clone();
            }
            continue;
        }
        config.games.push(GameEntry {
            id: entry.id.clone(),
            name: entry.name.clone(),
            game_slug: entry.game_slug.clone(),
            provider: entry.provider,
            app_id: entry.app_id.clone(),
            save_dir: PathBuf::new(),
            // Activated once the game is located on this machine.
            enabled: false,
            sync_selection: entry.sync_selection.clone(),
            sync_across_providers: entry.sync_across_providers,
            active_save_label: entry.active_save_label.clone(),
            save_dir_template: entry.save_dir_template.clone(),
            extra_roots: entry
                .extra_roots
                .iter()
                .map(|r| SaveRoot {
                    id: r.id.clone(),
                    label: r.label.clone(),
                    kind: r.kind,
                    path: PathBuf::new(),
                    include: r.include.clone(),
                    exclude: r.exclude.clone(),
                    enabled: true,
                    shared_across_profiles: r.shared_across_profiles,
                    template: r.template.clone(),
                })
                .collect(),
            exclude_globs: entry.exclude_globs.clone(),
            profiles: entry.profiles.clone(),
            main_profile_name: entry.main_profile_name.clone(),
            detection: Some(DetectionInfo {
                source: DetectionSource::Cloud,
                confidence: 0,
                reason: "Saved on another PC. Install the game here, or choose a folder to restore into."
                    .to_string(),
                database_title: None,
            }),
            ..Default::default()
        });
        added.push(entry.id.clone());
    }

    let mut known: BTreeSet<(String, String)> = config
        .save_state_labels
        .iter()
        .map(|l| (l.game_id.clone(), l.commit_hash.clone()))
        .collect();
    for lbl in &catalog.save_state_labels {
        if known.insert((lbl.game_id.clone(), lbl.commit_hash.clone())) {
            config.save_state_labels.push(lbl.clone());
        }
    }
    added
}

// ---- Labels ---------------------------------------------------------------------------

/// Resolves the user-assigned label for `(game_id, commit_hash)`, checking persisted metadata
/// first and falling back to `[Label: ...]` embedded in the git commit message.
pub fn label_for_commit(
    config: &AppConfig,
    game_id: &str,
    commit_hash: &str,
    commit_message: Option<&str>,
) -> Option<String> {
    if let Some(entry) = config.save_state_labels.iter().rev().find(|l| {
        l.game_id == game_id
            && (l.commit_hash == commit_hash
                || l.commit_hash.starts_with(commit_hash)
                || commit_hash.starts_with(&l.commit_hash))
    }) {
        let trimmed = entry.label.trim();
        return if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }

    commit_message.and_then(extract_label_from_commit_message)
}

/// The remote this configuration syncs with, if any.
pub fn remote_url_for(config: &AppConfig) -> Option<String> {
    if let Some(custom) = config.custom_remote() {
        return Some(custom.to_string());
    }
    if config.auth.has_remote_credentials() {
        remote_url_for_auth(config.auth.github_username(), &config.repo_name, &config.auth)
    } else {
        None
    }
}

fn with_machine(config: &AppConfig, subject: &str) -> String {
    let machine = config.machine_name.trim();
    if machine.is_empty() {
        subject.to_string()
    } else {
        format!("{subject}\n\nMachine: {machine}")
    }
}

/// Assigns or clears a label for a specific `(game_id, commit_hash)` save state and persists it
/// into `sgm_repo_metadata.json` in the git repository.
pub fn set_save_state_label(
    config: &mut AppConfig,
    game_id: &str,
    commit_hash: &str,
    label: &str,
) -> Result<()> {
    let idx = config
        .games
        .iter()
        .position(|g| g.id == game_id)
        .with_context(|| format!("Game '{game_id}' not found in configuration"))?;

    let trimmed = label.trim().to_string();
    if let Some(existing) = config
        .save_state_labels
        .iter_mut()
        .find(|l| l.game_id == game_id && l.commit_hash == commit_hash)
    {
        existing.label = trimmed.clone();
        existing.created_at = Utc::now();
    } else {
        config.save_state_labels.push(SaveStateLabel {
            game_id: game_id.to_string(),
            commit_hash: commit_hash.to_string(),
            label: trimmed.clone(),
            created_at: Utc::now(),
        });
    }

    let subpath = repo_subpath_for_game(&config.games[idx], config.separate_folder_per_provider);
    let latest_hash = list_commits_for_paths(&config.local_repo_path, &[subpath], 1)
        .ok()
        .and_then(|list| list.into_iter().next().map(|c| c.commit_hash));
    if latest_hash.as_deref() == Some(commit_hash) {
        config.games[idx].active_save_label = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.clone())
        };
    }

    init_or_open_local_repo(&config.local_repo_path, remote_url_for(config).as_deref())?;
    write_repo_metadata_manifest(config)?;
    let msg = if trimmed.is_empty() {
        format!("Clear save state label for {game_id}")
    } else {
        format!("Set save state label for {game_id}: {trimmed}")
    };
    commit_all_if_changed(&config.local_repo_path, &with_machine(config, &msg))?;
    Ok(())
}

// ---- Snapshot description -------------------------------------------------------------

/// Small description stored next to a profile's files so a snapshot remembers the mod
/// setup it was made with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SnapshotMeta {
    #[serde(default)]
    pub mods: Option<ModFingerprint>,
}

/// Reads the snapshot description of `profile_id` as of `rev`.
pub fn read_snapshot_meta(config: &AppConfig, game: &GameEntry, profile_id: &str, rev: &str) -> Option<SnapshotMeta> {
    let path = profile_base_dir(game, config.separate_folder_per_provider, profile_id).join(SNAPSHOT_META_FILE);
    let raw = read_file_at(&config.local_repo_path, rev, &path)?;
    serde_json::from_slice(&raw).ok()
}

// ---- Mirroring ------------------------------------------------------------------------

fn remove_stale_repo_files(repo_dir: &Path, expected_rel_files: &BTreeSet<String>) -> Result<()> {
    if !repo_dir.is_dir() {
        return Ok(());
    }
    let mut to_delete = Vec::new();
    for entry in WalkDir::new(repo_dir)
        .into_iter()
        .filter_entry(|e| {
            // Other profiles and extra roots live beneath the primary folder; never touch them.
            !(e.depth() == 1 && e.file_name().to_str().is_some_and(is_reserved_name))
        })
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(repo_dir)?;
        let norm = normalize_rel_path(rel);
        if !expected_rel_files.contains(&norm) {
            to_delete.push(entry.path().to_path_buf());
        }
    }
    for path in to_delete {
        fs::remove_file(&path)?;
    }
    prune_empty_dirs(repo_dir)?;
    Ok(())
}

fn prune_empty_dirs(root: &Path) -> Result<()> {
    let mut dirs: Vec<PathBuf> = WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .map(|e| e.path().to_path_buf())
        .collect();
    // Deepest first.
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in dirs {
        if fs::read_dir(&d).map(|mut it| it.next().is_none()).unwrap_or(false) {
            let _ = fs::remove_dir(&d);
        }
    }
    Ok(())
}

/// Copies one root's live files into its repository folder (splitting oversized files) and
/// removes repository files that no longer exist live. Returns the number of live files.
fn mirror_root(
    config: &AppConfig,
    game: &GameEntry,
    root: &SaveRoot,
    warnings: &mut Vec<LargeFileWarning>,
) -> Result<usize> {
    let repo_dir = config.local_repo_path.join(root_repo_dir(
        game,
        config.separate_folder_per_provider,
        &game.active_profile,
        root,
    ));
    fs::create_dir_all(&repo_dir)?;
    let threshold_bytes = config.lfs_warning_threshold_mb.saturating_mul(1024 * 1024);
    let chunk_size_bytes = config.chunk_size_mb.max(1).saturating_mul(1024 * 1024);

    let selected = collect_root_files(root, &game.exclude_globs, &config.global_exclude_globs)?;
    let mut expected: BTreeSet<String> = BTreeSet::new();
    for rel in &selected {
        let src_file = root.path.join(rel);
        let rel_parent = rel.parent().unwrap_or_else(|| Path::new(""));
        let dst_dir = repo_dir.join(rel_parent);
        let file_name = rel
            .file_name()
            .and_then(|n| n.to_str())
            .context("Save file name is not valid UTF-8")?;
        if first_component_reserved(rel) {
            continue;
        }

        let warn_opt = copy_or_split_file(
            &src_file,
            &dst_dir,
            file_name,
            threshold_bytes,
            chunk_size_bytes,
            config.split_large_saves,
        )?;

        let rel_norm = normalize_rel_path(rel);
        let parent_prefix = if rel_parent.as_os_str().is_empty() {
            String::new()
        } else {
            format!("{}/", normalize_rel_path(rel_parent))
        };
        match warn_opt {
            Some(mut warn) => {
                warn.game_name = game.name.clone();
                warn.provider = game.provider;
                warn.relative_path = rel_norm.clone();
                if warn.split_applied {
                    let manifest_rel = format!("{parent_prefix}{file_name}.sgm_manifest.json");
                    if let Ok(raw) = fs::read_to_string(repo_dir.join(&manifest_rel))
                        && let Ok(manifest) = serde_json::from_str::<ChunkManifest>(&raw)
                    {
                        for part in manifest.chunks {
                            expected.insert(format!("{parent_prefix}{}", part.part_file_name));
                        }
                    }
                    expected.insert(manifest_rel);
                } else {
                    expected.insert(rel_norm);
                }
                warnings.push(warn);
            }
            None => {
                expected.insert(rel_norm);
            }
        }
    }
    remove_stale_repo_files(&repo_dir, &expected)?;
    Ok(selected.len())
}

fn write_snapshot_meta(config: &AppConfig, game: &GameEntry) -> Result<()> {
    let dir = config.local_repo_path.join(profile_base_dir(
        game,
        config.separate_folder_per_provider,
        &game.active_profile,
    ));
    let path = dir.join(SNAPSHOT_META_FILE);
    match scan_mod_fingerprint(&game.mod_sources) {
        Some(mods) => {
            fs::create_dir_all(&dir)?;
            let json = serde_json::to_string_pretty(&SnapshotMeta { mods: Some(mods) })?;
            if fs::read_to_string(&path).ok().as_deref() != Some(json.as_str()) {
                fs::write(&path, json)?;
            }
        }
        None => {
            if path.exists() {
                let _ = fs::remove_file(&path);
            }
        }
    }
    Ok(())
}

/// Original (reassembled) relative paths present in a repository folder, for stale cleanup.
fn expected_restored_rel_paths(repo_dir: &Path, filter: &PathFilter) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for entry in WalkDir::new(repo_dir)
        .into_iter()
        .filter_entry(|e| !(e.depth() == 1 && e.file_name().to_str().is_some_and(is_reserved_name)))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if fname == ".gitattributes" || fname == ".gitkeep" || fname == REPO_METADATA_FILE || is_chunk_part_file_name(fname) {
            continue;
        }
        let rel = path.strip_prefix(repo_dir)?;
        let original = if is_manifest_file_name(fname) {
            let manifest: ChunkManifest = serde_json::from_str(&fs::read_to_string(path)?)?;
            rel.with_file_name(&manifest.original_file_name)
        } else {
            rel.to_path_buf()
        };
        let norm = normalize_rel_path(&original);
        if filter.accepts(&norm) {
            out.insert(norm);
        }
    }
    Ok(out)
}

fn root_filter(config: &AppConfig, game: &GameEntry, root: &SaveRoot) -> PathFilter {
    let mut excludes: Vec<&str> = root.exclude.iter().map(String::as_str).collect();
    excludes.extend(game.exclude_globs.iter().map(String::as_str));
    excludes.extend(config.global_exclude_globs.iter().map(String::as_str));
    excludes.extend(crate::pattern::BUILTIN_EXCLUDES);
    PathFilter::new(&root.include, &excludes)
}

/// Makes `root.path` equal to the copy in `src_dir`: restores / reassembles every file and
/// removes live files (inside the root's scope) that the copy does not have.
fn restore_root_from(config: &AppConfig, game: &GameEntry, root: &SaveRoot, src_dir: &Path) -> Result<usize> {
    if root.path.as_os_str().is_empty() {
        return Ok(0);
    }
    let filter = root_filter(config, game, root);
    let expected = expected_restored_rel_paths(src_dir, &filter)?;

    // Remove live files the snapshot does not contain.
    let mut removed: Vec<PathBuf> = Vec::new();
    for rel in collect_root_files(root, &game.exclude_globs, &config.global_exclude_globs)? {
        if !expected.contains(&normalize_rel_path(&rel)) {
            let full = root.path.join(&rel);
            if full.is_file() {
                fs::remove_file(&full)?;
                removed.push(full);
            }
        }
    }

    fs::create_dir_all(&root.path)
        .with_context(|| format!("Failed to create save folder {}", root.path.display()))?;
    let mut restored = 0usize;
    for entry in WalkDir::new(src_dir)
        .into_iter()
        .filter_entry(|e| !(e.depth() == 1 && e.file_name().to_str().is_some_and(is_reserved_name)))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if fname == ".gitattributes" || fname == ".gitkeep" || fname == REPO_METADATA_FILE || is_chunk_part_file_name(fname) {
            continue;
        }
        let rel = path.strip_prefix(src_dir)?;
        if is_manifest_file_name(fname) {
            let manifest: ChunkManifest = serde_json::from_str(&fs::read_to_string(path)?)
                .with_context(|| format!("Corrupt split-file manifest {}", path.display()))?;
            let original = rel.with_file_name(&manifest.original_file_name);
            if !filter.accepts(&normalize_rel_path(&original)) {
                continue;
            }
            let dst = root.path.join(&original);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            reassemble_from_manifest(&manifest, path.parent().unwrap_or(src_dir), &dst)?;
        } else {
            if !filter.accepts(&normalize_rel_path(rel)) {
                continue;
            }
            let dst = root.path.join(rel);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(path, &dst)
                .with_context(|| format!("Failed to restore {}", dst.display()))?;
        }
        restored += 1;
    }
    remove_emptied_parents(&root.path, &removed);
    Ok(restored)
}

/// Removes folders that became empty because SaveVault deleted the files in them. Folders
/// the game created empty are left exactly as they were.
fn remove_emptied_parents(root: &Path, removed_files: &[PathBuf]) {
    for file in removed_files {
        let mut dir = file.parent();
        while let Some(d) = dir {
            if d == root || !d.starts_with(root) {
                break;
            }
            if fs::remove_dir(d).is_err() {
                break; // not empty (or already gone)
            }
            dir = d.parent();
        }
    }
}

/// Replaces the live files of `game` with the repository working-tree copy of `profile_id`.
/// With `clear_missing`, roots the profile has no copy of are emptied (fresh profile).
pub(crate) fn apply_mirror_to_live(
    config: &AppConfig,
    game: &GameEntry,
    profile_id: &str,
    clear_missing: bool,
) -> Result<usize> {
    let mut restored = 0usize;
    for root in game.active_roots() {
        let src = config.local_repo_path.join(root_repo_dir(
            game,
            config.separate_folder_per_provider,
            profile_id,
            &root,
        ));
        if src.is_dir() {
            restored += restore_root_from(config, game, &root, &src)?;
        } else if clear_missing && !root.shared_across_profiles {
            let mut removed = Vec::new();
            for rel in collect_root_files(&root, &game.exclude_globs, &config.global_exclude_globs)? {
                let full = root.path.join(&rel);
                if full.is_file() {
                    fs::remove_file(&full)?;
                    removed.push(full);
                }
            }
            remove_emptied_parents(&root.path, &removed);
        }
    }
    Ok(restored)
}

// ---- Sync -----------------------------------------------------------------------------

/// Progress callback invoked during sync operations.
pub type ProgressCallback<'a> = &'a dyn Fn(LogLevel, &str);

fn emit_progress(progress: Option<ProgressCallback<'_>>, level: LogLevel, msg: &str) {
    if let Some(cb) = progress {
        cb(level, msg);
    }
}

/// Options for [`sync_games`].
#[derive(Default)]
pub struct SyncOptions<'a> {
    /// Save state label to attach to the resulting snapshot.
    pub label: Option<&'a str>,
    /// Push to (and first pull from) the configured remote.
    pub use_remote: bool,
    pub progress: Option<ProgressCallback<'a>>,
    /// Ids of games that are currently running; they are left for later.
    pub running: BTreeSet<String>,
    /// On a conflict, keep this machine's saves instead of asking.
    pub local_wins: bool,
    /// Replaces the generated commit subject (used for "Rollback …" / "Switch profile …").
    pub subject_override: Option<String>,
}

/// Synchronizes a single game (and any cross-provider siblings if enabled) to the local git repo
/// and optionally pushes to GitHub.
pub fn sync_game_in_config(
    config: &mut AppConfig,
    game_id: &str,
    push_remote: bool,
) -> Result<SyncOutcome> {
    sync_games(
        config,
        &[game_id.to_string()],
        SyncOptions {
            use_remote: push_remote,
            ..Default::default()
        },
    )
}

/// Synchronizes a single game with an optional save state label attached to the resulting commit.
pub fn sync_game_with_label(
    config: &mut AppConfig,
    game_id: &str,
    label: Option<&str>,
    push_remote: bool,
) -> Result<SyncOutcome> {
    sync_games(
        config,
        &[game_id.to_string()],
        SyncOptions {
            label: label.map(str::trim).filter(|s| !s.is_empty()),
            use_remote: push_remote,
            ..Default::default()
        },
    )
}

/// Synchronizes the specified game IDs in one git commit & push batch.
pub fn sync_multiple_games(
    config: &mut AppConfig,
    game_ids: &[String],
    push_remote: bool,
) -> Result<SyncOutcome> {
    sync_games(
        config,
        game_ids,
        SyncOptions {
            use_remote: push_remote,
            ..Default::default()
        },
    )
}

/// Synchronizes the specified game IDs while streaming step-by-step progress log events.
pub fn sync_multiple_games_with_progress(
    config: &mut AppConfig,
    game_ids: &[String],
    label: Option<&str>,
    push_remote: bool,
    progress: Option<ProgressCallback<'_>>,
) -> Result<SyncOutcome> {
    sync_games(
        config,
        game_ids,
        SyncOptions {
            label: label.map(str::trim).filter(|s| !s.is_empty()),
            use_remote: push_remote,
            progress,
            ..Default::default()
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Nothing,
    Upload,
    Download,
    Conflict,
    /// The game's folder disappeared from the cloud (removed on another machine).
    RemovedRemotely,
}

fn unit_for(config: &AppConfig, game: &GameEntry) -> String {
    normalize_rel_path(&profile_base_dir(
        game,
        config.separate_folder_per_provider,
        &game.active_profile,
    ))
}

fn decide_action(
    config: &AppConfig,
    game: &GameEntry,
    live: &LiveState,
    tree: &TreeListing,
    integration: &IntegrationOutcome,
    local_wins: bool,
) -> Action {
    let (mirror_now, mirror_files) = mirror_digest(tree, config, game, &game.active_profile);
    let state = &game.sync_state;
    let first_time = state.live_fingerprint.is_none() && state.mirror_digest.is_none();
    let live_changed = state.live_fingerprint.as_deref() != Some(live.fingerprint.as_str());
    let mirror_changed = state.mirror_digest.as_deref() != Some(mirror_now.as_str());
    let came_from_remote = integration.updated_units.contains(&unit_for(config, game));

    if live.files == 0 {
        if mirror_files > 0 {
            return Action::Download; // reinstall, new machine, wiped prefix: bring saves back
        }
        if !first_time && mirror_changed && came_from_remote {
            return Action::RemovedRemotely;
        }
        return Action::Nothing;
    }
    if mirror_files == 0 {
        if !first_time
            && came_from_remote
            && state.mirror_digest.as_deref().is_some_and(|d| d != EMPTY_DIGEST)
        {
            return Action::RemovedRemotely;
        }
        return Action::Upload;
    }

    let resolve = |ambiguous: bool| -> Action {
        if !ambiguous {
            return Action::Upload;
        }
        if live_matches_mirror(config, game, tree) {
            return Action::Nothing;
        }
        if local_wins {
            return Action::Upload;
        }
        match config.conflict_policy {
            ConflictPolicy::Ask => Action::Conflict,
            ConflictPolicy::PreferLocal => Action::Upload,
            ConflictPolicy::PreferCloud => Action::Download,
        }
    };

    if first_time {
        // An upgrade from an older version or a first run: only a real "other side" (something
        // that just arrived from the remote) can make this a conflict.
        return resolve(came_from_remote);
    }
    match (live_changed, mirror_changed) {
        (false, false) => Action::Nothing,
        (true, false) => Action::Upload,
        (false, true) => Action::Download,
        (true, true) => resolve(true),
    }
}

/// Runs one sync pass over `game_ids`.
pub fn sync_games(config: &mut AppConfig, game_ids: &[String], opts: SyncOptions<'_>) -> Result<SyncOutcome> {
    if game_ids.is_empty() {
        return Ok(SyncOutcome::default());
    }
    let progress = opts.progress;
    let repo = config.local_repo_path.clone();

    emit_progress(
        progress,
        LogLevel::Info,
        &format!("Checking {} game(s)...", game_ids.len()),
    );

    init_or_open_local_repo(&repo, remote_url_for(config).as_deref())?;

    for id in game_ids {
        if !config.games.iter().any(|g| &g.id == id) {
            bail!("Game with id '{id}' was not found in configuration.");
        }
    }

    let mut outcome = SyncOutcome::default();

    // 1. Bring in what other machines pushed.
    let remote_ready = opts.use_remote && config.remote_configured() && has_origin_remote(&repo);
    let mut integration = IntegrationOutcome::default();
    let mut remote_reachable = remote_ready;
    if remote_ready {
        match fetch_remote(&repo, &config.auth).and_then(|has_branch| {
            if has_branch {
                integrate_remote(&repo, &config.machine_name)
            } else {
                Ok(IntegrationOutcome::default())
            }
        }) {
            Ok(result) => {
                match result.kind {
                    IntegrationKind::FastForward | IntegrationKind::Merged => emit_progress(
                        progress,
                        LogLevel::Info,
                        &format!(
                            "Received newer saves for {} game folder(s) from the cloud.",
                            result.updated_units.len()
                        ),
                    ),
                    IntegrationKind::UpToDate | IntegrationKind::LocalAhead => {}
                }
                integration = result;
            }
            Err(err) => {
                remote_reachable = false;
                let msg = format!("Could not reach the cloud repository, working locally: {err}");
                emit_progress(progress, LogLevel::Warning, &msg);
                outcome.push_error = Some(msg);
            }
        }
        if let Some(catalog) = read_repo_metadata_manifest(&repo) {
            outcome.adopted_from_cloud = adopt_catalog(config, &catalog);
        }
    }

    // 2. Expand targets with cross-provider siblings and reconcile those.
    let mut target_slugs = BTreeSet::new();
    let mut target_ids: BTreeSet<String> = game_ids.iter().cloned().collect();
    for id in game_ids {
        if let Some(g) = config.games.iter().find(|g| &g.id == id)
            && g.sync_across_providers
        {
            target_slugs.insert(g.game_slug.clone());
        }
    }
    for g in &config.games {
        if g.enabled && g.sync_across_providers && target_slugs.contains(&g.game_slug) {
            target_ids.insert(g.id.clone());
        }
    }
    for slug in &target_slugs {
        let cfg_snapshot = config.clone();
        let mut backup = |g: &GameEntry| {
            let _ = create_safety_backup(&cfg_snapshot, g, "before cross-store sync");
        };
        if let Some(winner) = reconcile_cross_provider_inner(&mut config.games, slug, &mut backup)? {
            outcome
                .cross_provider_synced
                .push(format!("{slug} (from {winner})"));
        }
    }

    // 3. Decide and act per game.
    let mut tree = current_tree(&repo);
    let mut uploaded_names: Vec<String> = Vec::new();
    // (index, live state captured before copying)
    let mut uploaded: Vec<(usize, LiveState)> = Vec::new();

    // Git-level conflicts: both machines committed new saves for the same profile.
    for c in &integration.conflicts {
        if let Some(game) = config
            .games
            .iter_mut()
            .find(|g| unit_matches(&c.unit, g, config.separate_folder_per_provider))
        {
            let info = commit_summary(&repo, &c.their_commit);
            game.sync_state.conflict = Some(ConflictInfo {
                detected_at: Utc::now(),
                cloud_commit: c.their_commit.clone(),
                cloud_machine: info.as_ref().and_then(|i| i.machine.clone()),
                cloud_time: info.map(|i| i.timestamp),
            });
            outcome.conflicts.push(game.id.clone());
            emit_progress(
                progress,
                LogLevel::Warning,
                &format!(
                    "{}: another PC also saved new progress. Kept this PC's saves; the other version is in the timeline.",
                    game.name
                ),
            );
        }
    }

    for idx in 0..config.games.len() {
        if !target_ids.contains(&config.games[idx].id) {
            continue;
        }
        let game = config.games[idx].clone();
        let tag = format!("[{}] {}", game.provider.display_name(), game.name);

        if game.is_placeholder() {
            let why = "no save location on this PC yet".to_string();
            config.games[idx].last_sync_status = Some("Waiting: not located on this PC yet".to_string());
            outcome.skipped.push((game.id.clone(), why));
            continue;
        }
        if opts.running.contains(&game.id) {
            config.games[idx].last_sync_status = Some("Waiting: game is running".to_string());
            emit_progress(progress, LogLevel::Info, &format!("{tag}: running, will snapshot when it closes."));
            outcome.skipped.push((game.id.clone(), "game is running".to_string()));
            continue;
        }
        if game.install_dir.as_deref() == Some(game.save_dir.as_path()) {
            let msg = format!(
                "{} ({}): skipped because save folder points to the game install directory ({}). Set a dedicated save subfolder first.",
                game.name,
                game.provider.display_name(),
                game.save_dir.display()
            );
            config.games[idx].last_sync_status =
                Some("Skipped: save folder points to install directory".to_string());
            emit_progress(progress, LogLevel::Warning, &msg);
            outcome.notes.push(msg);
            outcome.skipped.push((game.id.clone(), "save folder is the install folder".to_string()));
            continue;
        }
        if !location_available(&game) {
            let msg = format!(
                "{} ({}): save folder {} does not exist yet.",
                game.name,
                game.provider.display_name(),
                game.save_dir.display()
            );
            config.games[idx].last_sync_status = Some(format!(
                "Skipped: save folder missing ({})",
                game.save_dir.display()
            ));
            emit_progress(progress, LogLevel::Info, &msg);
            outcome.notes.push(msg);
            outcome.skipped.push((game.id.clone(), "drive or folder not available".to_string()));
            continue;
        }

        let live = live_state(config, &game);
        let mut action = decide_action(config, &game, &live, &tree, &integration, opts.local_wins);
        // An unresolved earlier conflict keeps blocking automatic overwrites of live saves.
        if game.sync_state.conflict.is_some() && action == Action::Download && !opts.local_wins {
            action = Action::Conflict;
        }

        match action {
            Action::Nothing => {
                let (digest, files) = mirror_digest(&tree, config, &game, &game.active_profile);
                let g = &mut config.games[idx];
                if live.files == 0 && files == 0 {
                    g.last_sync_status = Some("No saves yet".to_string());
                    let msg = format!("{tag}: no save files yet.");
                    outcome.notes.push(msg);
                    outcome.skipped.push((g.id.clone(), "no saves yet".to_string()));
                    continue;
                }
                g.sync_state.live_fingerprint = Some(live.fingerprint.clone());
                g.sync_state.mirror_digest = Some(digest);
                g.last_synced_at = Some(Utc::now());
                g.last_sync_status = Some(match opts.label {
                    Some(lbl) => format!("Synced {} file(s) [{}]", live.files, lbl),
                    None => format!("Synced {} file(s)", live.files),
                });
                if let Some(lbl) = opts.label {
                    g.active_save_label = Some(lbl.to_string());
                }
                outcome.synced_games += 1;
                outcome.unchanged.push(g.id.clone());
                uploaded_names.push(format!("{}/{}", game.provider.slug(), game.game_slug));
            }
            Action::Upload => {
                emit_progress(
                    progress,
                    LogLevel::Info,
                    &format!("{tag}: saving {} file(s)...", live.files),
                );
                let mut count = 0usize;
                for root in game.active_roots() {
                    count += mirror_root(config, &game, &root, &mut outcome.warnings)?;
                }
                write_snapshot_meta(config, &game)?;
                let g = &mut config.games[idx];
                if let Some(lbl) = opts.label {
                    g.active_save_label = Some(lbl.to_string());
                }
                g.last_synced_at = Some(Utc::now());
                g.last_sync_status = Some(match opts.label {
                    Some(lbl) => format!("Synced {count} file(s) [{lbl}]"),
                    None => format!("Synced {count} file(s)"),
                });
                outcome.synced_games += 1;
                outcome.uploaded.push(g.id.clone());
                uploaded_names.push(format!("{}/{}", game.provider.slug(), game.game_slug));
                uploaded.push((idx, live));
            }
            Action::Download => {
                emit_progress(
                    progress,
                    LogLevel::Info,
                    &format!("{tag}: newer saves found in the repository, restoring them here..."),
                );
                if let Some(zip) = create_safety_backup(config, &game, "before cloud update")? {
                    emit_progress(
                        progress,
                        LogLevel::Info,
                        &format!("{tag}: previous files kept in {}", zip.display()),
                    );
                }
                let restored = apply_mirror_to_live(config, &game, &game.active_profile, false)?;
                let new_live = live_state(config, &game);
                let (digest, _) = mirror_digest(&tree, config, &game, &game.active_profile);
                let g = &mut config.games[idx];
                g.sync_state.live_fingerprint = Some(new_live.fingerprint);
                g.sync_state.mirror_digest = Some(digest);
                g.sync_state.conflict = None;
                g.last_synced_at = Some(Utc::now());
                g.last_sync_status = Some(format!("Updated from the cloud ({restored} file(s))"));
                outcome.synced_games += 1;
                outcome.downloaded.push(g.id.clone());
                emit_progress(
                    progress,
                    LogLevel::Success,
                    &format!("{tag}: restored {restored} file(s) from the repository."),
                );
            }
            Action::Conflict => {
                let g = &mut config.games[idx];
                if g.sync_state.conflict.is_none() {
                    let head = head_commit(&repo).unwrap_or_default();
                    let info = list_commits_for_paths(
                        &repo,
                        &profile_pathspecs(&game, config.separate_folder_per_provider, &game.active_profile),
                        1,
                    )
                    .ok()
                    .and_then(|l| l.into_iter().next());
                    g.sync_state.conflict = Some(ConflictInfo {
                        detected_at: Utc::now(),
                        cloud_commit: info.as_ref().map(|i| i.commit_hash.clone()).unwrap_or(head),
                        cloud_machine: info.as_ref().and_then(|i| i.machine.clone()),
                        cloud_time: info.map(|i| i.timestamp),
                    });
                }
                g.last_sync_status = Some("Needs your choice: this PC and the cloud both have new saves".to_string());
                outcome.conflicts.push(g.id.clone());
                emit_progress(
                    progress,
                    LogLevel::Warning,
                    &format!("{tag}: this PC and the cloud both have new saves. Nothing was overwritten; choose which to keep."),
                );
            }
            Action::RemovedRemotely => {
                let g = &mut config.games[idx];
                g.enabled = false;
                g.sync_state.mirror_digest = Some(EMPTY_DIGEST.to_string());
                g.last_sync_status = Some("Paused: removed from the cloud on another PC".to_string());
                let msg = format!(
                    "{tag}: its cloud folder was removed on another PC. Your local saves are untouched; tracking is paused."
                );
                emit_progress(progress, LogLevel::Warning, &msg);
                outcome.notes.push(msg);
                outcome.skipped.push((g.id.clone(), "removed from the cloud".to_string()));
            }
        }
    }

    // 4. Commit what was uploaded (plus the catalog).
    if outcome.synced_games > 0 || !outcome.adopted_from_cloud.is_empty() {
        write_repo_metadata_manifest(config)?;
        let base_msg = match &opts.subject_override {
            Some(subject) => subject.clone(),
            None => {
                if uploaded_names.len() == 1 {
                    format!("Sync {}", uploaded_names[0])
                } else {
                    format!(
                        "Sync {} game save folder(s): {}",
                        uploaded_names.len(),
                        uploaded_names.join(", ")
                    )
                }
            }
        };
        let subject = match opts.label {
            Some(lbl) => format!("{base_msg} [Label: {lbl}]"),
            None => base_msg,
        };
        outcome.committed_sha = commit_all_if_changed(&repo, &with_machine(config, &subject))?;
        if let Some(sha) = &outcome.committed_sha {
            let short = &sha[..sha.len().min(7)];
            emit_progress(
                progress,
                LogLevel::Success,
                &format!("Snapshot saved ({short})."),
            );
            tree = current_tree(&repo);
        } else {
            emit_progress(
                progress,
                LogLevel::Info,
                "Everything is already up to date (no new snapshot needed).",
            );
        }

        for (idx, live) in &uploaded {
            let game = config.games[*idx].clone();
            let (digest, _) = mirror_digest(&tree, config, &game, &game.active_profile);
            let g = &mut config.games[*idx];
            g.sync_state.live_fingerprint = Some(live.fingerprint.clone());
            g.sync_state.mirror_digest = Some(digest);
            // Uploading this machine's version settles any earlier conflict in its favour.
            if opts.local_wins {
                g.sync_state.conflict = None;
            }
        }

        if let Some(lbl) = opts.label {
            let labelled: Vec<String> = outcome
                .uploaded
                .iter()
                .chain(outcome.unchanged.iter())
                .cloned()
                .collect();
            if let Some(sha) = outcome.committed_sha.clone() {
                for gid in &labelled {
                    config.save_state_labels.push(SaveStateLabel {
                        game_id: gid.clone(),
                        commit_hash: sha.clone(),
                        label: lbl.to_string(),
                        created_at: Utc::now(),
                    });
                }
                write_repo_metadata_manifest(config)?;
                commit_all_if_changed(
                    &repo,
                    &with_machine(config, &format!("Record save state label metadata: {lbl}")),
                )?;
            } else {
                for gid in &labelled {
                    if let Some(g) = config.games.iter().find(|g| &g.id == gid) {
                        let sub = repo_subpath_for_game(g, config.separate_folder_per_provider);
                        if let Ok(commits) = list_commits_for_paths(&repo, &[sub], 1)
                            && let Some(latest) = commits.first()
                        {
                            let latest_hash = latest.commit_hash.clone();
                            let _ = set_save_state_label(config, gid, &latest_hash, lbl);
                        }
                    }
                }
            }
        }
    }

    // 5. Push.
    if remote_ready && remote_reachable {
        push_with_retry(config, progress, &mut outcome);
    } else if opts.use_remote && !remote_ready {
        emit_progress(
            progress,
            LogLevel::Info,
            "Saved on this PC only. Connect GitHub in Settings to also keep a cloud copy.",
        );
    }

    Ok(outcome)
}

/// Fetches the remote and merges its game catalog into the configuration without syncing
/// any game. Used on a fresh machine to discover what is in the cloud. Returns the ids of
/// games that were added as "not installed here yet".
pub fn pull_cloud_catalog(config: &mut AppConfig) -> Result<Vec<String>> {
    let repo = config.local_repo_path.clone();
    init_or_open_local_repo(&repo, remote_url_for(config).as_deref())?;
    if config.remote_configured() && has_origin_remote(&repo) && fetch_remote(&repo, &config.auth)? {
        integrate_remote(&repo, &config.machine_name)?;
    }
    match read_repo_metadata_manifest(&repo) {
        Some(catalog) => Ok(adopt_catalog(config, &catalog)),
        None => Ok(Vec::new()),
    }
}

fn unit_matches(unit: &str, game: &GameEntry, separate: bool) -> bool {
    normalize_rel_path(&profile_base_dir(game, separate, &game.active_profile)) == unit
}

fn commit_summary(repo: &Path, commit: &str) -> Option<CommitInfo> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["log", "-1", "--date=iso", "--pretty=format:%H%x1f%h%x1f%ad%x1f%at%x1f%s%x1f%b", commit])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut parts = text.splitn(6, '\x1f');
    let hash = parts.next()?.to_string();
    let short = parts.next()?.to_string();
    let ts = parts.next()?.to_string();
    let unix = parts.next()?.trim().parse().unwrap_or(0);
    let subject = parts.next()?.to_string();
    let body = parts.next().unwrap_or("");
    Some(CommitInfo {
        commit_hash: hash,
        short_hash: short,
        timestamp: ts,
        label: extract_label_from_commit_message(&subject),
        message: subject,
        changed_files: Vec::new(),
        machine: body
            .lines()
            .rev()
            .find_map(|l| l.trim().strip_prefix("Machine:"))
            .map(|m| m.trim().to_string()),
        unix_time: unix,
    })
}

fn push_with_retry(config: &mut AppConfig, progress: Option<ProgressCallback<'_>>, outcome: &mut SyncOutcome) {
    let repo = config.local_repo_path.clone();
    if head_commit(&repo).is_none() {
        return;
    }
    if config.custom_remote().is_none()
        && let Some(reason) = ensure_github_repo(config, progress, outcome)
    {
        outcome.push_error = Some(reason);
        return;
    }
    push_commits(config, progress, outcome);
}

/// Makes sure the GitHub repository exists. Returns a reason when pushing cannot proceed.
fn ensure_github_repo(
    config: &AppConfig,
    progress: Option<ProgressCallback<'_>>,
    outcome: &mut SyncOutcome,
) -> Option<String> {
    emit_progress(
        progress,
        LogLevel::Info,
        &format!(
            "Checking remote GitHub repository '{}/{}'...",
            config.auth.github_username(),
            config.repo_name
        ),
    );
    match ensure_remote_repo_exists(
        config.auth.github_username(),
        &config.repo_name,
        config.auth.pat_token(),
        &config.auth,
    ) {
        Ok(RepoEnsureResult::AlreadyExists { .. }) => None,
        Ok(RepoEnsureResult::CreatedPrivate { remote_url }) => {
            emit_progress(
                progress,
                LogLevel::Success,
                &format!("Created private GitHub repository: {remote_url}"),
            );
            outcome
                .notes
                .push(format!("Created private GitHub repository: {remote_url}"));
            None
        }
        Ok(RepoEnsureResult::NeedsPatOrManual { message, .. }) => {
            emit_progress(progress, LogLevel::Warning, &message);
            Some(message)
        }
        Err(err) => {
            let msg = err.to_string();
            emit_progress(progress, LogLevel::Error, &msg);
            Some(msg)
        }
    }
}

fn push_commits(config: &mut AppConfig, progress: Option<ProgressCallback<'_>>, outcome: &mut SyncOutcome) {
    let repo = config.local_repo_path.clone();
    emit_progress(progress, LogLevel::Info, "Uploading snapshots to the cloud...");
    let mut result = push_to_remote(&repo, &config.auth);
    if let Err(err) = &result
        && is_non_fast_forward_error(&err.to_string())
    {
        // Another machine pushed in the meantime: take its changes and try once more.
        emit_progress(
            progress,
            LogLevel::Info,
            "The cloud changed while syncing; combining and retrying...",
        );
        result = fetch_remote(&repo, &config.auth)
            .and_then(|_| integrate_remote(&repo, &config.machine_name))
            .and_then(|_| push_to_remote(&repo, &config.auth));
    }
    match result {
        Ok(()) => {
            emit_progress(
                progress,
                LogLevel::Success,
                "Cloud copy is up to date.",
            );
            outcome.pushed_to_remote = true;
            outcome.push_error = None;
        }
        Err(err) => {
            let msg = err.to_string();
            emit_progress(progress, LogLevel::Error, &format!("Git push failed: {msg}"));
            outcome.push_error = Some(msg);
        }
    }
}

// ---- Restore --------------------------------------------------------------------------

/// What restoring a snapshot would change, shown before the user confirms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestorePreview {
    pub files_in_snapshot: usize,
    pub files_live: usize,
    pub will_add: Vec<String>,
    pub will_replace: Vec<String>,
    pub will_remove: Vec<String>,
    pub snapshot_mods: Option<ModFingerprint>,
}

/// Extracts the files of `profile_id` as they were at `commit` into a temporary folder,
/// one sub-folder per root id.
fn extract_profile_at(
    config: &AppConfig,
    game: &GameEntry,
    profile_id: &str,
    commit: &str,
) -> Result<(TempDir, Vec<(SaveRoot, PathBuf)>)> {
    let tmp = TempDir::new()?;
    let mut extracted = Vec::new();
    for root in game.active_roots() {
        let rel = root_repo_dir(game, config.separate_folder_per_provider, profile_id, &root);
        let dst = tmp.path().join(&root.id);
        if extract_path_at(&config.local_repo_path, commit, &rel, &dst)? {
            extracted.push((root, dst));
        }
    }
    Ok((tmp, extracted))
}

/// Compares a snapshot with the live files without changing anything.
pub fn preview_restore(config: &AppConfig, game_id: &str, commit: &str) -> Result<RestorePreview> {
    let game = config
        .games
        .iter()
        .find(|g| g.id == game_id)
        .with_context(|| format!("Game '{game_id}' not found"))?;
    let (_tmp, extracted) = extract_profile_at(config, game, &game.active_profile, commit)?;
    let mut preview = RestorePreview {
        snapshot_mods: read_snapshot_meta(config, game, &game.active_profile, commit).and_then(|m| m.mods),
        ..Default::default()
    };
    for (root, dir) in &extracted {
        let filter = root_filter(config, game, root);
        let snap = expected_restored_rel_paths(dir, &filter)?;
        let live: BTreeSet<String> =
            collect_root_files(root, &game.exclude_globs, &config.global_exclude_globs)?
                .iter()
                .map(|p| normalize_rel_path(p))
                .collect();
        preview.files_in_snapshot += snap.len();
        preview.files_live += live.len();
        let prefix = if root.id == PRIMARY_ROOT_ID { String::new() } else { format!("{}: ", root.label) };
        for f in &snap {
            if !live.contains(f) {
                preview.will_add.push(format!("{prefix}{f}"));
            } else {
                let same = fs::metadata(dir.join(f)).map(|m| m.len()).ok()
                    == fs::metadata(root.path.join(f)).map(|m| m.len()).ok()
                    && sha256_file(&dir.join(f)).ok() == sha256_file(&root.path.join(f)).ok();
                if !same {
                    preview.will_replace.push(format!("{prefix}{f}"));
                }
            }
        }
        for f in &live {
            if !snap.contains(f) {
                preview.will_remove.push(format!("{prefix}{f}"));
            }
        }
    }
    Ok(preview)
}

/// Rolls back `game_id` to `commit_hash`, restoring and reassembling save files into the game's
/// local save folder (and linked cross-provider siblings when enabled), then records the
/// restored state as a new snapshot. The files that were live before are zipped into the
/// safety folder first.
pub fn rollback_game_to_commit(
    config: &mut AppConfig,
    game_id: &str,
    commit_hash: &str,
    push_remote: bool,
) -> Result<usize> {
    let idx = config
        .games
        .iter()
        .position(|g| g.id == game_id)
        .with_context(|| format!("Game '{game_id}' not found"))?;
    let target_game = config.games[idx].clone();
    if target_game.is_placeholder() {
        bail!("{} has no save folder on this PC yet.", target_game.name);
    }

    let (_tmp, extracted) = extract_profile_at(config, &target_game, &target_game.active_profile, commit_hash)?;
    if extracted.is_empty() {
        bail!("That snapshot has no files for {}.", target_game.name);
    }

    create_safety_backup(config, &target_game, "before restore")?;
    let mut restored = 0usize;
    for (root, dir) in &extracted {
        restored += restore_root_from(config, &target_game, root, dir)?;
    }

    let short = &commit_hash[..commit_hash.len().min(7)];

    // Propagate rollback to linked cross-provider siblings if enabled.
    let mut sync_ids = vec![target_game.id.clone()];
    if target_game.sync_across_providers {
        let siblings: Vec<GameEntry> = config
            .games
            .iter()
            .filter(|g| {
                g.id != target_game.id
                    && g.enabled
                    && g.game_slug == target_game.game_slug
                    && g.sync_across_providers
                    && !g.is_placeholder()
            })
            .cloned()
            .collect();
        for sib in siblings {
            create_safety_backup(config, &sib, "before restore")?;
            if let Some((_, dir)) = extracted.iter().find(|(r, _)| r.id == PRIMARY_ROOT_ID) {
                restore_root_from(config, &sib, &sib.primary_root(), dir)?;
            }
            sync_ids.push(sib.id.clone());
        }
    }

    // Record the restored state as the newest snapshot so the mirror and live files agree.
    // Cross-provider reconciliation is skipped on purpose: every sibling already holds the
    // restored files.
    let was_linked: Vec<(String, bool)> = config
        .games
        .iter()
        .map(|g| (g.id.clone(), g.sync_across_providers))
        .collect();
    for g in &mut config.games {
        g.sync_across_providers = false;
    }
    let result = sync_games(
        config,
        &sync_ids,
        SyncOptions {
            use_remote: push_remote,
            local_wins: true,
            subject_override: Some(format!(
                "Rollback {}/{} to {}",
                target_game.provider.slug(),
                target_game.game_slug,
                short
            )),
            ..Default::default()
        },
    );
    for (id, linked) in was_linked {
        if let Some(g) = config.games.iter_mut().find(|g| g.id == id) {
            g.sync_across_providers = linked;
        }
    }
    result?;

    for id in &sync_ids {
        if let Some(g) = config.games.iter_mut().find(|g| &g.id == id) {
            g.sync_state.conflict = None;
            g.last_sync_status = Some(if *id == target_game.id {
                format!("Rolled back to {short} ({restored} files)")
            } else {
                format!("Rolled back via {} to {short}", target_game.provider.display_name())
            });
        }
    }

    Ok(restored)
}

/// Resolves a pending conflict by taking the cloud version (live files are zipped first).
pub fn resolve_conflict_use_cloud(config: &mut AppConfig, game_id: &str, push_remote: bool) -> Result<usize> {
    let commit = config
        .games
        .iter()
        .find(|g| g.id == game_id)
        .and_then(|g| g.sync_state.conflict.as_ref())
        .map(|c| c.cloud_commit.clone())
        .with_context(|| "This game has no pending conflict.".to_string())?;
    rollback_game_to_commit(config, game_id, &commit, push_remote)
}

/// Resolves a pending conflict by keeping this machine's saves (they become the newest snapshot).
pub fn resolve_conflict_keep_local(config: &mut AppConfig, game_id: &str, push_remote: bool) -> Result<SyncOutcome> {
    let out = sync_games(
        config,
        &[game_id.to_string()],
        SyncOptions {
            use_remote: push_remote,
            local_wins: true,
            ..Default::default()
        },
    )?;
    if let Some(g) = config.games.iter_mut().find(|g| g.id == game_id) {
        g.sync_state.conflict = None;
    }
    Ok(out)
}

/// Loads the snapshot timeline of a game's profile, newest first, with labels resolved.
pub fn list_snapshots(config: &AppConfig, game_id: &str, profile_id: &str, limit: usize) -> Result<Vec<CommitInfo>> {
    let game = config
        .games
        .iter()
        .find(|g| g.id == game_id)
        .with_context(|| format!("Game '{game_id}' not found"))?;
    let specs = profile_pathspecs(game, config.separate_folder_per_provider, profile_id);
    let mut list = list_commits_for_paths(&config.local_repo_path, &specs, limit)?;
    for c in &mut list {
        c.label = label_for_commit(config, game_id, &c.commit_hash, Some(&c.message));
    }
    Ok(list)
}

// ---- Removal --------------------------------------------------------------------------

/// Removes `game_id` from `config.games` and optionally removes its save folder from the Git
/// repository (`<local_repo_path>/<repo_subpath>`), updates `sgm_repo_metadata.json`, commits the
/// removal, and pushes to the remote when `push_remote` is true.
///
/// Local game save files in `game.save_dir` are never modified or deleted.
pub fn remove_game_entry(
    config: &mut AppConfig,
    game_id: &str,
    remove_from_git: bool,
    push_remote: bool,
) -> Result<RemoveGameOutcome> {
    let idx = config
        .games
        .iter()
        .position(|g| g.id == game_id)
        .with_context(|| format!("Game '{game_id}' not found"))?;
    let removed_game = config.games.remove(idx);
    let subpath = repo_subpath_for_game(&removed_game, config.separate_folder_per_provider);

    if !remove_from_git {
        return Ok(RemoveGameOutcome {
            game_id: removed_game.id,
            game_name: removed_game.name,
            repo_subpath: subpath,
            removed_from_git: false,
            committed_sha: None,
            pushed_to_remote: false,
            push_error: None,
        });
    }

    config.save_state_labels.retain(|l| l.game_id != game_id);

    // Only delete the repo subfolder if no remaining tracked game maps to the exact same subpath.
    let subpath_shared_by_remaining = config
        .games
        .iter()
        .any(|g| repo_subpath_for_game(g, config.separate_folder_per_provider) == subpath);

    let repo_game_dir = config.local_repo_path.join(&subpath);
    let mut deleted_dir = false;
    if !subpath_shared_by_remaining && repo_game_dir.exists() {
        fs::remove_dir_all(&repo_game_dir).with_context(|| {
            format!(
                "Failed to remove game folder {} from git repository",
                repo_game_dir.display()
            )
        })?;
        deleted_dir = true;
        if config.separate_folder_per_provider
            && let Some(parent) = repo_game_dir.parent()
            && parent != config.local_repo_path
            && fs::read_dir(parent).map(|mut it| it.next().is_none()).unwrap_or(false)
        {
            let _ = fs::remove_dir(parent);
        }
    }

    let mut committed_sha = None;
    let mut pushed_to_remote = false;
    let mut push_error = None;

    if config.local_repo_path.join(".git").exists() {
        if deleted_dir || config.local_repo_path.join(REPO_METADATA_FILE).exists() {
            write_repo_metadata_manifest(config)?;
        }
        let msg = format!(
            "Remove {}/{} from save repository",
            removed_game.provider.slug(),
            removed_game.game_slug
        );
        committed_sha = commit_all_if_changed(&config.local_repo_path, &with_machine(config, &msg))?;

        if committed_sha.is_some()
            && push_remote
            && config.remote_configured()
            && has_origin_remote(&config.local_repo_path)
        {
            match push_to_remote(&config.local_repo_path, &config.auth) {
                Ok(()) => pushed_to_remote = true,
                Err(err) => push_error = Some(err.to_string()),
            }
        }
    }

    Ok(RemoveGameOutcome {
        game_id: removed_game.id,
        game_name: removed_game.name,
        repo_subpath: subpath,
        removed_from_git: true,
        committed_sha,
        pushed_to_remote,
        push_error,
    })
}

/// Kept for callers that only know a single repository sub-path.
pub fn list_commits_for_game_path(repo: &Path, subpath: &Path) -> Result<Vec<CommitInfo>> {
    list_commits_for_path(repo, subpath)
}

/// Kind of root, re-exported for UI code that builds roots.
pub fn default_label_for_kind(kind: RootKind) -> &'static str {
    kind.display_name()
}
