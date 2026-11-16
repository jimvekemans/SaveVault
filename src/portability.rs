use crate::chunker::{normalize_rel_path, reassemble_or_copy_dir};
use crate::git_ops::{
    checkout_path_at_commit, init_or_open_local_repo, list_commits_for_path, pull_from_remote,
};
use crate::models::{
    AppConfig, AuthMethod, GameEntry, Provider, RepoGameCatalogEntry, SaveStateLabel,
    SyncSelectionMode,
};
use crate::providers::{scan_provider_libraries, slugify_game_name};
use crate::safety::{create_safety_backup, extract_archive, is_supported_archive};
use crate::sync_engine::{
    SyncOutcome, read_repo_metadata_manifest, sync_game_with_label,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use walkdir::WalkDir;

/// Summary of games, directory layout, and save state labels discovered in a repository.
#[derive(Debug, Clone)]
pub struct RepoInspection {
    pub repo_path: PathBuf,
    pub separate_folder_per_provider: bool,
    pub games: Vec<RepoGameCatalogEntry>,
    pub labels: Vec<SaveStateLabel>,
}

/// Summary of a full post-reset structure re-instatement.
#[derive(Debug, Clone, Default)]
pub struct ReinstateOutcome {
    pub games_reinstated: usize,
    pub files_restored: usize,
    pub labels_restored: usize,
    pub game_ids: Vec<String>,
}

/// Request to import a game's latest or historical save state from another accessible repository.
#[derive(Debug, Clone)]
pub struct ExternalImportRequest {
    pub source_game: RepoGameCatalogEntry,
    pub commit_hash: Option<String>,
    pub target_provider: Provider,
    pub target_save_dir: PathBuf,
    pub custom_label: Option<String>,
}

/// Request to import a save file, folder, or archive downloaded from online sources.
#[derive(Debug, Clone)]
pub struct OnlineSaveImportRequest {
    pub source_path: PathBuf,
    pub game_name: String,
    pub provider: Provider,
    pub target_save_dir: PathBuf,
    pub label: Option<String>,
    pub backup_existing_before_import: bool,
}

fn title_from_slug(slug: &str) -> String {
    slug.split('_')
        .filter(|s| !s.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn provider_from_slug(slug: &str) -> Option<Provider> {
    Provider::all().iter().copied().find(|p| p.slug() == slug)
}

/// Inspects a local save repository directory, reading `sgm_repo_metadata.json`, scanning
/// provider/game subfolders, and collecting commit labels.
pub fn inspect_repository(repo_path: &Path) -> Result<RepoInspection> {
    if !repo_path.is_dir() {
        bail!("Repository folder does not exist: {}", repo_path.display());
    }

    let manifest = read_repo_metadata_manifest(repo_path);
    let mut separate_folder_per_provider = manifest
        .as_ref()
        .map(|m| m.separate_folder_per_provider)
        .unwrap_or(true);

    let mut games: Vec<RepoGameCatalogEntry> = manifest
        .as_ref()
        .map(|m| m.games.clone())
        .unwrap_or_default();
    let mut labels: Vec<SaveStateLabel> = manifest
        .as_ref()
        .map(|m| m.save_state_labels.clone())
        .unwrap_or_default();

    let mut seen_subpaths: BTreeSet<String> =
        games.iter().map(|g| g.repo_subpath.clone()).collect();

    // Also scan disk directories in case folders were added without updating metadata.
    let mut detected_provider_layout = false;
    if let Ok(top_entries) = fs::read_dir(repo_path) {
        for entry in top_entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if dir_name.starts_with('.') || dir_name.starts_with('@') {
                continue;
            }
            if let Some(provider) = provider_from_slug(dir_name) {
                detected_provider_layout = true;
                if let Ok(sub_entries) = fs::read_dir(&path) {
                    for game_entry in sub_entries.flatten() {
                        let gpath = game_entry.path();
                        if !gpath.is_dir() {
                            continue;
                        }
                        let slug = gpath
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_string();
                        if slug.starts_with('.') || slug.starts_with('@') || slug.is_empty() {
                            continue;
                        }
                        let subpath = format!("{}/{}", provider.slug(), slug);
                        if seen_subpaths.insert(subpath.clone()) {
                            games.push(RepoGameCatalogEntry {
                                id: format!("{}:{}", provider.slug(), slug),
                                name: title_from_slug(&slug),
                                game_slug: slug,
                                provider,
                                app_id: None,
                                repo_subpath: subpath,
                                sync_selection: SyncSelectionMode::EntireFolder,
                                ..Default::default()
                            });
                        }
                    }
                }
            } else if manifest.is_none() {
                // Flat `<game_slug>` layout
                let slug = slugify_game_name(dir_name);
                let subpath = dir_name.to_string();
                if seen_subpaths.insert(subpath.clone()) {
                    games.push(RepoGameCatalogEntry {
                        id: format!("{}:{}", Provider::Steam.slug(), slug),
                        name: title_from_slug(&slug),
                        game_slug: slug,
                        provider: Provider::Steam,
                        app_id: None,
                        repo_subpath: subpath,
                        sync_selection: SyncSelectionMode::EntireFolder,
                        ..Default::default()
                    });
                }
            }
        }
    }

    if manifest.is_none() {
        separate_folder_per_provider = detected_provider_layout;
    }

    // Collect commit message labels from git history if not already listed in metadata.
    let mut known_label_keys: BTreeSet<(String, String)> = labels
        .iter()
        .map(|l| (l.game_id.clone(), l.commit_hash.clone()))
        .collect();

    for g in &mut games {
        if let Ok(commits) = list_commits_for_path(repo_path, Path::new(&g.repo_subpath)) {
            if g.active_save_label.is_none()
                && let Some(first) = commits.first()
                && let Some(lbl) = &first.label
            {
                g.active_save_label = Some(lbl.clone());
            }
            for c in commits {
                if let Some(lbl) = c.label
                    && known_label_keys.insert((g.id.clone(), c.commit_hash.clone()))
                {
                    labels.push(SaveStateLabel {
                        game_id: g.id.clone(),
                        commit_hash: c.commit_hash,
                        label: lbl,
                        created_at: Utc::now(),
                    });
                }
            }
        }
    }

    games.sort_by(|a, b| a.game_slug.cmp(&b.game_slug).then(a.provider.cmp(&b.provider)));

    Ok(RepoInspection {
        repo_path: repo_path.to_path_buf(),
        separate_folder_per_provider,
        games,
        labels,
    })
}

fn clone_or_sync_source_into_local(
    source_url_or_path: &str,
    local_repo_path: &Path,
    auth: &AuthMethod,
) -> Result<()> {
    let src = source_url_or_path.trim();
    if src.is_empty() {
        return Ok(());
    }

    let src_path = Path::new(src);
    if src_path.exists() {
        if src_path.canonicalize().ok() == local_repo_path.canonicalize().ok() {
            return Ok(());
        }
        if src_path.join(".git").exists() {
            if !local_repo_path.join(".git").exists() {
                if let Some(parent) = local_repo_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let out = Command::new("git")
                    .arg("clone")
                    .arg(src_path)
                    .arg(local_repo_path)
                    .output()
                    .context("Failed to clone local source repository")?;
                if !out.status.success() {
                    bail!(
                        "git clone from {} failed: {}",
                        src_path.display(),
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
            } else {
                init_or_open_local_repo(local_repo_path, Some(src))?;
                pull_from_remote(local_repo_path, auth)?;
            }
        } else {
            copy_dir_all(src_path, local_repo_path)?;
            init_or_open_local_repo(local_repo_path, None)?;
        }
    } else {
        // Remote URL (https://, git@, ssh://)
        init_or_open_local_repo(local_repo_path, Some(src))?;
        pull_from_remote(local_repo_path, auth)?;
    }

    Ok(())
}

/// Re-instates the full game configuration, save state labels, and local save directories
/// from a repository after a system reset (or onto a fresh machine).
pub fn reinstate_full_structure(
    config: &mut AppConfig,
    source_repo_url_or_path: Option<&str>,
    fallback_saves_root: &Path,
) -> Result<ReinstateOutcome> {
    if let Some(src) = source_repo_url_or_path {
        clone_or_sync_source_into_local(src, &config.local_repo_path, &config.auth)?;
    }

    let inspection = inspect_repository(&config.local_repo_path)?;
    config.separate_folder_per_provider = inspection.separate_folder_per_provider;

    // Discover locally installed games on the newly reset system to map save paths automatically when present.
    let discovered = scan_provider_libraries(&config.provider_libraries);
    let mut discovered_save_map: HashMap<(Provider, String), PathBuf> = HashMap::new();
    for d in discovered {
        discovered_save_map.insert((d.provider, d.game_slug), d.save_dir);
    }

    let mut outcome = ReinstateOutcome::default();

    for entry in &inspection.games {
        let repo_game_dir = config.local_repo_path.join(&entry.repo_subpath);
        if !repo_game_dir.is_dir() {
            continue;
        }

        let target_save_dir = if let Some(existing) = config.games.iter().find(|g| g.id == entry.id) {
            existing.save_dir.clone()
        } else if let Some(found) = discovered_save_map.get(&(entry.provider, entry.game_slug.clone())) {
            found.clone()
        } else {
            fallback_saves_root
                .join(entry.provider.slug())
                .join(&entry.game_slug)
        };

        let restored = reassemble_or_copy_dir(
            &repo_game_dir,
            &target_save_dir,
            &entry.sync_selection,
        )?;
        outcome.files_restored += restored;

        if let Some(existing) = config.games.iter_mut().find(|g| g.id == entry.id) {
            existing.save_dir = target_save_dir;
            existing.sync_selection = entry.sync_selection.clone();
            existing.sync_across_providers = entry.sync_across_providers;
            if entry.active_save_label.is_some() {
                existing.active_save_label = entry.active_save_label.clone();
            }
            existing.last_synced_at = Some(Utc::now());
            existing.last_sync_status = Some(format!("Re-instated {restored} file(s)"));
        } else {
            config.games.push(GameEntry {
                id: entry.id.clone(),
                name: entry.name.clone(),
                game_slug: entry.game_slug.clone(),
                provider: entry.provider,
                app_id: entry.app_id.clone(),
                install_dir: None,
                save_dir: target_save_dir,
                enabled: true,
                sync_selection: entry.sync_selection.clone(),
                sync_rate_override: None,
                sync_across_providers: entry.sync_across_providers,
                last_synced_at: Some(Utc::now()),
                last_sync_status: Some(format!("Re-instated {restored} file(s)")),
                active_save_label: entry.active_save_label.clone(),

                ..Default::default()
            });
        }

        outcome.games_reinstated += 1;
        outcome.game_ids.push(entry.id.clone());
    }

    let mut existing_label_keys: BTreeSet<(String, String)> = config
        .save_state_labels
        .iter()
        .map(|l| (l.game_id.clone(), l.commit_hash.clone()))
        .collect();
    for lbl in inspection.labels {
        if existing_label_keys.insert((lbl.game_id.clone(), lbl.commit_hash.clone())) {
            config.save_state_labels.push(lbl);
            outcome.labels_restored += 1;
        }
    }

    Ok(outcome)
}

/// Ensures an accessible external repository (local path or remote URL) is ready to inspect on disk.
pub fn prepare_external_repo(
    source_url_or_path: &str,
    cache_dir: &Path,
    auth: &AuthMethod,
) -> Result<PathBuf> {
    let src = source_url_or_path.trim();
    if src.is_empty() {
        bail!("External repository URL or folder path cannot be empty.");
    }
    let as_path = Path::new(src);
    if as_path.is_dir() {
        return Ok(as_path.to_path_buf());
    }
    init_or_open_local_repo(cache_dir, Some(src))?;
    pull_from_remote(cache_dir, auth)?;
    Ok(cache_dir.to_path_buf())
}

/// Imports a game's latest or historical commit save state from an external/shared repository
/// into the current user's save directory and own sync repository.
pub fn import_game_from_external_repo(
    config: &mut AppConfig,
    external_repo_path: &Path,
    req: &ExternalImportRequest,
    push_remote: bool,
) -> Result<SyncOutcome> {
    let temp_holder: Option<TempDir>;
    let source_game_dir = if let Some(commit_hash) = req.commit_hash.as_deref() {
        let tmp = TempDir::new()?;
        if external_repo_path.join(".git").exists() {
            let clone_out = Command::new("git")
                .arg("clone")
                .arg("--quiet")
                .arg(external_repo_path)
                .arg(tmp.path())
                .output()
                .context("Failed to clone external repository for historical checkout")?;
            if !clone_out.status.success() {
                bail!(
                    "Failed to stage external repository: {}",
                    String::from_utf8_lossy(&clone_out.stderr).trim()
                );
            }
            checkout_path_at_commit(tmp.path(), commit_hash, Path::new(&req.source_game.repo_subpath))?;
        } else {
            copy_dir_all(external_repo_path, tmp.path())?;
        }
        let dir = tmp.path().join(&req.source_game.repo_subpath);
        temp_holder = Some(tmp);
        dir
    } else {
        temp_holder = None;
        external_repo_path.join(&req.source_game.repo_subpath)
    };

    if !source_game_dir.is_dir() {
        bail!(
            "Game save directory '{}' not found in external repository",
            source_game_dir.display()
        );
    }

    reassemble_or_copy_dir(
        &source_game_dir,
        &req.target_save_dir,
        &SyncSelectionMode::EntireFolder,
    )?;
    drop(temp_holder);

    let target_id = format!(
        "{}:{}",
        req.target_provider.slug(),
        req.source_game.game_slug
    );
    if let Some(existing) = config.games.iter_mut().find(|g| g.id == target_id) {
        existing.save_dir = req.target_save_dir.clone();
        existing.enabled = true;
    } else {
        config.games.push(GameEntry {
            id: target_id.clone(),
            name: req.source_game.name.clone(),
            game_slug: req.source_game.game_slug.clone(),
            provider: req.target_provider,
            app_id: req.source_game.app_id.clone(),
            install_dir: None,
            save_dir: req.target_save_dir.clone(),
            enabled: true,
            sync_selection: SyncSelectionMode::EntireFolder,
            sync_rate_override: None,
            sync_across_providers: false,
            last_synced_at: None,
            last_sync_status: None,
            active_save_label: None,

            ..Default::default()
        });
    }

    let resolved_label = req
        .custom_label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| req.source_game.active_save_label.clone())
        .unwrap_or_else(|| "Imported from shared repo".to_string());

    sync_game_with_label(config, &target_id, Some(&resolved_label), push_remote)
}

/// Imports a save file, folder, or archive from an online source into a game's local save
/// directory, optionally committing a pre-import backup first, then syncing with a label.
pub fn import_online_save(
    config: &mut AppConfig,
    req: &OnlineSaveImportRequest,
    push_remote: bool,
) -> Result<SyncOutcome> {
    if !req.source_path.exists() {
        bail!(
            "Online save source does not exist: {}",
            req.source_path.display()
        );
    }
    let clean_name = req.game_name.trim();
    if clean_name.is_empty() {
        bail!("Game title is required when importing an online save.");
    }

    let slug = slugify_game_name(clean_name);
    let game_id = format!("{}:{}", req.provider.slug(), slug);

    if let Some(existing) = config.games.iter_mut().find(|g| g.id == game_id) {
        existing.save_dir = req.target_save_dir.clone();
        existing.enabled = true;
    } else {
        config.games.push(GameEntry {
            id: game_id.clone(),
            name: clean_name.to_string(),
            game_slug: slug,
            provider: req.provider,
            app_id: None,
            install_dir: None,
            save_dir: req.target_save_dir.clone(),
            enabled: true,
            sync_selection: SyncSelectionMode::EntireFolder,
            sync_rate_override: None,
            sync_across_providers: false,
            last_synced_at: None,
            last_sync_status: None,
            active_save_label: None,

            ..Default::default()
        });
    }

    // Create a safety backup commit of any existing local saves before overwriting.
    if req.backup_existing_before_import && dir_has_any_file(&req.target_save_dir) {
        sync_game_with_label(config, &game_id, Some("Pre-import backup"), false)?;
    }
    if let Some(game) = config.games.iter().find(|g| g.id == game_id) {
        create_safety_backup(config, game, "before import")?;
    }

    fs::create_dir_all(&req.target_save_dir).with_context(|| {
        format!(
            "Failed to create target save directory {}",
            req.target_save_dir.display()
        )
    })?;

    if req.source_path.is_dir() {
        reassemble_or_copy_dir(
            &req.source_path,
            &req.target_save_dir,
            &SyncSelectionMode::EntireFolder,
        )?;
    } else if req.source_path.is_file() {
        let lower_name = req
            .source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();

        let _ = &lower_name;
        if is_supported_archive(&req.source_path) {
            extract_archive(&req.source_path, &req.target_save_dir)?;
        } else {
            let file_name = req
                .source_path
                .file_name()
                .context("Invalid online save filename")?;
            fs::copy(&req.source_path, req.target_save_dir.join(file_name))?;
        }
    }

    let label = req
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Online Import");

    sync_game_with_label(config, &game_id, Some(label), push_remote)
}

fn dir_has_any_file(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .any(|e| e.file_type().is_file())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in WalkDir::new(src)
        .into_iter()
        .filter_entry(|e| e.file_name().to_str() != Some(".git"))
        .filter_map(|e| e.ok())
    {
        let rel = entry.path().strip_prefix(src)?;
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(p) = target.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Helper to build a relative preview string for repository entries.
pub fn format_repo_entry_summary(entry: &RepoGameCatalogEntry) -> String {
    let lbl = entry
        .active_save_label
        .as_deref()
        .map(|s| format!(" [{s}]"))
        .unwrap_or_default();
    format!(
        "[{}] {} ({}){}",
        entry.provider.display_name(),
        entry.name,
        normalize_rel_path(Path::new(&entry.repo_subpath)),
        lbl
    )
}
