//! Save profiles: several independent sets of saves for one game.
//!
//! Useful for games with a single save slot or no character selection, for keeping a
//! modded run apart from a vanilla one, or for sharing a PC. Every profile has its own
//! folder and timeline in the repository; switching swaps the live files (after saving the
//! outgoing profile and zipping a safety copy).

use crate::git_ops::{PROFILES_DIR, ROOTS_DIR, SNAPSHOT_META_FILE, commit_all_if_changed, extract_path_at, init_or_open_local_repo};
use crate::models::{AppConfig, CommitInfo, GameEntry, GameSyncState, SaveProfile};
use crate::safety::create_safety_backup;
use crate::sync_engine::{
    SyncOptions, apply_mirror_to_live, list_snapshots, live_state, mirror_digest, profile_base_dir,
    root_repo_dir, sync_games, write_repo_metadata_manifest,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use walkdir::WalkDir;

/// How a new profile gets its first saves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileStart {
    /// No saves: the game starts fresh when this profile is activated.
    Empty,
    /// A copy of the currently active profile's saves.
    CopyOfCurrent,
    /// The saves of `profile` as they were at `commit`.
    FromSnapshot { profile: String, commit: String },
}

/// One row of the profile list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
    pub note: String,
    pub active: bool,
    pub last_snapshot: Option<CommitInfo>,
}

fn is_reserved(name: &str) -> bool {
    name == PROFILES_DIR || name == ROOTS_DIR || name == SNAPSHOT_META_FILE
}

fn profile_id_from_name(name: &str, game: &GameEntry) -> String {
    let mut base: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    while base.contains("__") {
        base = base.replace("__", "_");
    }
    let base = base.trim_matches('_').to_string();
    let base = if base.is_empty() { "profile".to_string() } else { base };
    if !game.profiles.iter().any(|p| p.id == base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|id| !game.profiles.iter().any(|p| &p.id == id))
        .unwrap_or(base)
}

/// Copies a profile folder's own files (not nested profiles / roots of the main profile).
fn copy_profile_tree(src: &Path, dst: &Path) -> Result<usize> {
    if !src.is_dir() {
        return Ok(0);
    }
    let mut copied = 0usize;
    for entry in WalkDir::new(src)
        .into_iter()
        .filter_entry(|e| !(e.depth() == 1 && e.file_name().to_str().is_some_and(is_reserved)))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(src)?;
        let target = dst.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(entry.path(), &target)?;
        copied += 1;
    }
    Ok(copied)
}

fn find_game<'a>(config: &'a AppConfig, game_id: &str) -> Result<&'a GameEntry> {
    config
        .games
        .iter()
        .find(|g| g.id == game_id)
        .with_context(|| format!("Game '{game_id}' not found"))
}

fn commit(config: &AppConfig, subject: &str) -> Result<Option<String>> {
    write_repo_metadata_manifest(config)?;
    let machine = config.machine_name.trim();
    let msg = if machine.is_empty() {
        subject.to_string()
    } else {
        format!("{subject}\n\nMachine: {machine}")
    };
    commit_all_if_changed(&config.local_repo_path, &msg)
}

/// Creates a profile and returns its id. The live files are not touched; call
/// [`switch_profile`] to start using it.
pub fn create_profile(
    config: &mut AppConfig,
    game_id: &str,
    name: &str,
    note: &str,
    start: ProfileStart,
) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("Give the profile a name.");
    }
    let game = find_game(config, game_id)?.clone();
    if game
        .profile_ids()
        .iter()
        .any(|id| game.profile_name(id).eq_ignore_ascii_case(name))
    {
        bail!("{} already has a profile called \"{name}\".", game.name);
    }
    let id = profile_id_from_name(name, &game);
    init_or_open_local_repo(&config.local_repo_path, None)?;

    match &start {
        ProfileStart::Empty => {}
        ProfileStart::CopyOfCurrent => {
            // Make sure the repository holds the very latest live files first.
            if !game.is_placeholder() {
                sync_games(
                    config,
                    &[game_id.to_string()],
                    SyncOptions {
                        local_wins: true,
                        ..Default::default()
                    },
                )?;
            }
            let game = find_game(config, game_id)?.clone();
            for root in game.active_roots().into_iter().filter(|r| !r.shared_across_profiles) {
                let src = config.local_repo_path.join(root_repo_dir(
                    &game,
                    config.separate_folder_per_provider,
                    &game.active_profile,
                    &root,
                ));
                let dst = config.local_repo_path.join(root_repo_dir(
                    &game,
                    config.separate_folder_per_provider,
                    &id,
                    &root,
                ));
                copy_profile_tree(&src, &dst)?;
            }
        }
        ProfileStart::FromSnapshot { profile, commit } => {
            let tmp = TempDir::new()?;
            for root in game.active_roots().into_iter().filter(|r| !r.shared_across_profiles) {
                let rel = root_repo_dir(&game, config.separate_folder_per_provider, profile, &root);
                let staged = tmp.path().join(&root.id);
                if extract_path_at(&config.local_repo_path, commit, &rel, &staged)? {
                    let dst = config.local_repo_path.join(root_repo_dir(
                        &game,
                        config.separate_folder_per_provider,
                        &id,
                        &root,
                    ));
                    copy_profile_tree(&staged, &dst)?;
                }
            }
        }
    }

    let game = config
        .games
        .iter_mut()
        .find(|g| g.id == game_id)
        .context("game vanished")?;
    game.profiles.push(SaveProfile {
        id: id.clone(),
        name: name.to_string(),
        note: note.trim().to_string(),
        created_at: Utc::now(),
    });
    let slug = format!("{}/{}", game.provider.slug(), game.game_slug);
    commit(config, &format!("Create profile \"{name}\" for {slug}"))?;
    Ok(id)
}

/// Makes `target_profile` the live one: saves the outgoing profile, zips a safety copy, then
/// swaps the files in the game's folders. Returns the number of files put in place.
pub fn switch_profile(
    config: &mut AppConfig,
    game_id: &str,
    target_profile: &str,
    use_remote: bool,
) -> Result<usize> {
    let game = find_game(config, game_id)?.clone();
    if game.is_placeholder() {
        bail!("{} has no save folder on this PC yet.", game.name);
    }
    if !game.profile_ids().iter().any(|p| p == target_profile) {
        bail!("Profile not found.");
    }
    if game.active_profile == target_profile {
        return Ok(0);
    }
    if game.sync_state.conflict.is_some() {
        bail!("Resolve the pending save conflict for {} before switching profiles.", game.name);
    }

    // 1. Capture the outgoing profile.
    sync_games(
        config,
        &[game_id.to_string()],
        SyncOptions {
            local_wins: true,
            ..Default::default()
        },
    )?;
    let game = find_game(config, game_id)?.clone();
    create_safety_backup(config, &game, "before switching profile")?;

    // 2. Swap.
    let from_name = game.profile_name(&game.active_profile);
    let to_name = game.profile_name(target_profile);
    let mut switched = game.clone();
    switched.active_profile = target_profile.to_string();
    switched.sync_state = GameSyncState {
        last_played_at: game.sync_state.last_played_at,
        ..Default::default()
    };
    let restored = apply_mirror_to_live(config, &switched, target_profile, true)?;

    // 3. Record the new baseline so the next sync sees "nothing changed".
    let tree: BTreeMap<String, String> = crate::git_ops::list_tree(&config.local_repo_path, "HEAD")
        .into_iter()
        .collect();
    let live = live_state(config, &switched);
    let (digest, _) = mirror_digest(&tree, config, &switched, target_profile);
    switched.sync_state.live_fingerprint = Some(live.fingerprint);
    switched.sync_state.mirror_digest = Some(digest);
    switched.active_save_label = None;
    switched.last_sync_status = Some(format!("Switched to profile \"{to_name}\" ({restored} file(s))"));
    switched.last_synced_at = Some(Utc::now());

    let slug = format!("{}/{}", switched.provider.slug(), switched.game_slug);
    if let Some(slot) = config.games.iter_mut().find(|g| g.id == game_id) {
        *slot = switched;
    }
    commit(config, &format!("Switch {slug} from \"{from_name}\" to \"{to_name}\""))?;

    if use_remote {
        // Push the captured outgoing profile; failures only delay the upload.
        let _ = sync_games(
            config,
            &[game_id.to_string()],
            SyncOptions {
                use_remote: true,
                ..Default::default()
            },
        );
    }
    Ok(restored)
}

/// Renames a profile ("" = the main profile).
pub fn rename_profile(config: &mut AppConfig, game_id: &str, profile_id: &str, new_name: &str) -> Result<()> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        bail!("Give the profile a name.");
    }
    let game = config
        .games
        .iter_mut()
        .find(|g| g.id == game_id)
        .with_context(|| format!("Game '{game_id}' not found"))?;
    if profile_id.is_empty() {
        game.main_profile_name = Some(new_name.to_string());
    } else {
        let p = game
            .profiles
            .iter_mut()
            .find(|p| p.id == profile_id)
            .context("Profile not found.")?;
        p.name = new_name.to_string();
    }
    if config.local_repo_path.join(".git").exists() {
        commit(config, &format!("Rename profile to \"{new_name}\""))?;
    }
    Ok(())
}

/// Deletes a profile and its repository folder. The main profile and the active profile
/// cannot be deleted. Earlier snapshots stay in history.
pub fn delete_profile(config: &mut AppConfig, game_id: &str, profile_id: &str) -> Result<()> {
    if profile_id.is_empty() {
        bail!("The main profile cannot be deleted.");
    }
    let game = find_game(config, game_id)?.clone();
    if game.active_profile == profile_id {
        bail!("Switch to another profile before deleting this one.");
    }
    let name = game.profile_name(profile_id);
    let dir = config
        .local_repo_path
        .join(profile_base_dir(&game, config.separate_folder_per_provider, profile_id));
    if dir.is_dir() {
        fs::remove_dir_all(&dir)
            .with_context(|| format!("Failed to remove {}", dir.display()))?;
    }
    if let Some(g) = config.games.iter_mut().find(|g| g.id == game_id) {
        g.profiles.retain(|p| p.id != profile_id);
    }
    if config.local_repo_path.join(".git").exists() {
        let slug = format!("{}/{}", game.provider.slug(), game.game_slug);
        commit(config, &format!("Delete profile \"{name}\" of {slug}"))?;
    }
    Ok(())
}

/// Lists a game's profiles with their most recent snapshot.
pub fn profile_summaries(config: &AppConfig, game_id: &str) -> Vec<ProfileSummary> {
    let Ok(game) = find_game(config, game_id) else {
        return Vec::new();
    };
    game.profile_ids()
        .into_iter()
        .map(|id| ProfileSummary {
            name: game.profile_name(&id),
            note: game
                .profiles
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.note.clone())
                .unwrap_or_default(),
            active: game.active_profile == id,
            last_snapshot: list_snapshots(config, game_id, &id, 1)
                .ok()
                .and_then(|l| l.into_iter().next()),
            id,
        })
        .collect()
}
