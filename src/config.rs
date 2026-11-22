use crate::models::{
    AppConfig, AuthMethod, CONFIG_SCHEMA_VERSION, ConflictPolicy, KbmProfile, Platform, Provider,
    SyncRate, ThemeMode, default_overlay_hotkey, default_overlay_opacity, default_retroarch_binary,
};
use crate::providers::{default_provider_libraries, scan_provider_libraries};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Returns the platform-standard configuration file path (`.../save_game_manager/config.json`).
pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("save_game_manager")
        .join("config.json")
}

/// Returns the platform-standard local git clone directory (`.../save_game_manager/repo`).
pub fn default_local_repo_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("save_game_manager")
        .join("repo")
}

/// Constructs a fresh `AppConfig` populated with platform-specific provider library defaults.
pub fn default_config() -> AppConfig {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let platform = Platform::current();
    AppConfig {
        repo_name: "save_game_manager".to_string(),
        local_repo_path: default_local_repo_path(),
        auth: AuthMethod::default(),
        separate_folder_per_provider: true,
        global_sync_rate: SyncRate::OnStartup,
        split_large_saves: true,
        lfs_warning_threshold_mb: 95,
        chunk_size_mb: 45,
        provider_libraries: default_provider_libraries(platform, &home),
        games: Vec::new(),
        theme_mode: ThemeMode::Dark,
        sync_enabled: false,
        run_on_system_startup: false,
        save_state_labels: Vec::new(),
        schema_version: CONFIG_SCHEMA_VERSION,
        machine_name: default_machine_name(),
        auto_update_detection_db: true,
        auto_track_new_games: true,
        backup_on_game_exit: true,
        safety_backups: true,
        safety_backup_keep: 10,
        global_exclude_globs: Vec::new(),
        conflict_policy: ConflictPolicy::Ask,
        include_config_roots: false,
        onboarding_done: false,
        custom_remote_url: None,
        ignored_game_ids: Vec::new(),
        default_kbm_profile: KbmProfile::default(),
        retroarch_binary: default_retroarch_binary(),
        overlay_enabled: false,
        overlay_hotkey: default_overlay_hotkey(),
        overlay_opacity: default_overlay_opacity(),
    }
}

/// Folder for SaveVault's own data (repository clone, safety backups, detection database).
pub fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("save_game_manager")
}

/// Hostname-based default for `AppConfig::machine_name`.
pub fn default_machine_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this-pc".to_string())
}

/// Secrets are kept out of `config.json` in a sibling file readable only by the user.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct SecretStore {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    github_token: Option<String>,
}

fn secrets_path_for(config_path: &Path) -> PathBuf {
    config_path.with_file_name("secrets.json")
}

/// Removes credentials from `cfg`, returning them for separate storage.
fn take_secrets(cfg: &mut AppConfig) -> SecretStore {
    let mut store = SecretStore::default();
    match &mut cfg.auth {
        AuthMethod::PersonalAccessToken { token, .. } => {
            if !token.trim().is_empty() {
                store.github_token = Some(std::mem::take(token));
            }
        }
        AuthMethod::ExistingSshKey {
            pat_for_api,
            passphrase,
            ..
        }
        | AuthMethod::NewSshKey {
            pat_for_api,
            passphrase,
            ..
        } => {
            store.github_token = pat_for_api.take().filter(|t| !t.trim().is_empty());
            // Only needed while generating a key; never written to disk.
            passphrase.clear();
        }
    }
    store
}

fn put_secrets(cfg: &mut AppConfig, store: SecretStore) {
    let Some(tok) = store.github_token else {
        return;
    };
    match &mut cfg.auth {
        AuthMethod::PersonalAccessToken { token, .. } => {
            if token.trim().is_empty() {
                *token = tok;
            }
        }
        AuthMethod::ExistingSshKey { pat_for_api, .. } | AuthMethod::NewSshKey { pat_for_api, .. } => {
            if pat_for_api.as_deref().is_none_or(|t| t.trim().is_empty()) {
                *pat_for_api = Some(tok);
            }
        }
    }
}

/// Writes `contents` to `path` atomically (temp file + rename) with user-only permissions.
pub fn write_private_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.tmp{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("savevault"),
        std::process::id()
    ));
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&tmp)
            .with_context(|| format!("Failed to write {}", tmp.display()))?;
        use std::io::Write;
        file.write_all(contents)?;
        file.sync_all().ok();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, path).with_context(|| format!("Failed to replace {}", path.display()))?;
    Ok(())
}

/// Brings a configuration written by an older version up to the current schema.
/// Returns true when anything changed.
fn migrate_loaded_config(cfg: &mut AppConfig) -> bool {
    let mut changed = false;
    if cfg.machine_name.trim().is_empty() {
        cfg.machine_name = default_machine_name();
        changed = true;
    }
    if cfg.safety_backup_keep == 0 {
        cfg.safety_backup_keep = 10;
        changed = true;
    }
    // Launchers added after the config was first written.
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    for lib in default_provider_libraries(Platform::current(), &home) {
        if !cfg.provider_libraries.iter().any(|l| l.provider == lib.provider) {
            cfg.provider_libraries.push(lib);
            changed = true;
        }
    }
    if cfg.schema_version < CONFIG_SCHEMA_VERSION {
        if cfg.schema_version == 0 && !cfg.games.is_empty() {
            // An existing setup has already been through first-run, and its game list was
            // curated by hand: newly found games are offered, not added silently.
            cfg.onboarding_done = true;
            cfg.auto_track_new_games = false;
        }
        cfg.schema_version = CONFIG_SCHEMA_VERSION;
        changed = true;
    }
    changed
}

/// Repairs legacy/broken entries (such as Heroic/Legendary metadata dirs, `save_dir == install_dir`,
/// or stale missing save dirs) and reports whether any change was made.
fn repair_loaded_config(cfg: &mut AppConfig) -> bool {
    let mut changed = false;

    // Drop bogus `epic_games:metadata` entry produced by older scans of `~/.config/heroic/.../legendary/metadata`.
    let prev_len = cfg.games.len();
    cfg.games.retain(|g| {
        !(g.id == "epic_games:metadata"
            && g.save_dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == "metadata")
            && g.save_dir
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == "legendary"))
    });
    if cfg.games.len() != prev_len {
        changed = true;
    }

    // Narrow any Rocket League folder pointing at root wrapper down to TAGame/SaveData if present.
    for g in &mut cfg.games {
        let rl_savedata = g.save_dir.join("TAGame").join("SaveData");
        if rl_savedata.is_dir() {
            g.save_dir = rl_savedata;
            changed = true;
        }
    }

    let needs_rescan = cfg
        .games
        .iter()
        .any(|g| !g.is_placeholder() && g.install_dir.as_deref() == Some(g.save_dir.as_path()));
    if !needs_rescan {
        return changed;
    }

    let discovered: HashMap<(Provider, String), PathBuf> =
        scan_provider_libraries(&cfg.provider_libraries)
            .into_iter()
            .map(|d| ((d.provider, d.game_slug), d.save_dir))
            .collect();

    for g in &mut cfg.games {
        if g.install_dir.as_deref() == Some(g.save_dir.as_path()) {
            if let Some(better) = discovered.get(&(g.provider, g.game_slug.clone())) {
                g.save_dir = better.clone();
            } else {
                g.save_dir = g.save_dir.join("saves");
            }
            if g.last_sync_status
                .as_deref()
                .is_some_and(|s| s.starts_with("Skipped:") || s.starts_with("Synced"))
            {
                g.last_sync_status = None;
            }
            g.last_synced_at = None;
            changed = true;
        }
    }

    changed
}

/// Loads `AppConfig` from `path`, returning `default_config()` if the file does not exist yet.
///
/// Older configurations are migrated in place; the pre-migration file is kept once as
/// `config.json.v<old>.bak`.
pub fn load_config(path: &Path) -> Result<AppConfig> {
    if !path.exists() {
        return Ok(default_config());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file {}", path.display()))?;
    let mut cfg: AppConfig = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse config JSON {}", path.display()))?;
    if cfg.repo_name.trim().is_empty() {
        cfg.repo_name = "save_game_manager".to_string();
    }
    if cfg.lfs_warning_threshold_mb == 0 {
        cfg.lfs_warning_threshold_mb = 95;
    }
    if cfg.chunk_size_mb == 0 {
        cfg.chunk_size_mb = 45;
    }
    if cfg.overlay_hotkey.trim().is_empty() {
        cfg.overlay_hotkey = default_overlay_hotkey();
    }
    if !(0.55..=1.0).contains(&cfg.overlay_opacity) {
        cfg.overlay_opacity = default_overlay_opacity();
    }

    let old_schema = cfg.schema_version;
    // Inline credentials from older versions are picked up here and moved out on save.
    let had_inline_secret = {
        let mut probe = cfg.clone();
        take_secrets(&mut probe) != SecretStore::default()
    };
    if let Ok(raw_secrets) = fs::read_to_string(secrets_path_for(path))
        && let Ok(store) = serde_json::from_str::<SecretStore>(&raw_secrets)
    {
        put_secrets(&mut cfg, store);
    }

    let migrated = migrate_loaded_config(&mut cfg);
    let repaired = repair_loaded_config(&mut cfg);
    if migrated || repaired || had_inline_secret {
        if old_schema < CONFIG_SCHEMA_VERSION {
            let backup = path.with_file_name(format!(
                "{}.v{old_schema}.bak",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("config.json")
            ));
            if !backup.exists() {
                let _ = write_private_atomic(&backup, raw.as_bytes());
            }
        }
        let _ = save_config(path, &cfg);
    }
    Ok(cfg)
}

/// Persists `AppConfig` as pretty-printed JSON (atomically, user-readable only). Credentials
/// are written to a separate `secrets.json` next to it, never into the config itself.
pub fn save_config(path: &Path, config: &AppConfig) -> Result<()> {
    let mut public = config.clone();
    let secrets = take_secrets(&mut public);
    let serialized = serde_json::to_string_pretty(&public).context("Failed to serialize config")?;
    write_private_atomic(path, serialized.as_bytes())
        .with_context(|| format!("Failed to write config file {}", path.display()))?;

    let secrets_path = secrets_path_for(path);
    if secrets == SecretStore::default() {
        if secrets_path.exists() {
            let _ = fs::remove_file(&secrets_path);
        }
    } else {
        let raw = serde_json::to_string_pretty(&secrets).context("Failed to serialize secrets")?;
        write_private_atomic(&secrets_path, raw.as_bytes())
            .with_context(|| format!("Failed to write {}", secrets_path.display()))?;
    }
    Ok(())
}
