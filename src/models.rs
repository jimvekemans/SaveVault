pub use crate::kbm::keys::{
    KbmAction, KbmProfile, RetroArchGameMeta, RetroArchHotkey, default_kbm_bindings,
    default_kbm_hotkeys, default_mouse_deadzone, default_mouse_dpi, default_mouse_sensitivity,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Supported game storefronts / launchers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Provider {
    Steam,
    Gog,
    EpicGames,
    Amazon,
    Lutris,
    Bottles,
    RetroArch,
    /// Standalone installs, sideloaded Heroic apps, non-Steam shortcuts, emulators, and
    /// anything the user added by hand.
    #[default]
    Other,
}

impl Provider {
    /// Stable lowercase directory slug used inside the sync repository.
    pub fn slug(&self) -> &'static str {
        match self {
            Provider::Steam => "steam",
            Provider::Gog => "gog",
            Provider::EpicGames => "epic_games",
            Provider::Amazon => "amazon",
            Provider::Lutris => "lutris",
            Provider::Bottles => "bottles",
            Provider::RetroArch => "retroarch",
            Provider::Other => "other",
        }
    }

    /// Human-friendly provider display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Provider::Steam => "Steam",
            Provider::Gog => "GOG",
            Provider::EpicGames => "Epic Games",
            Provider::Amazon => "Amazon Games",
            Provider::Lutris => "Lutris",
            Provider::Bottles => "Bottles",
            Provider::RetroArch => "RetroArch",
            Provider::Other => "Other",
        }
    }

    /// All supported game providers in canonical order.
    pub fn all() -> &'static [Provider] {
        &[
            Provider::Steam,
            Provider::Gog,
            Provider::EpicGames,
            Provider::Amazon,
            Provider::Lutris,
            Provider::Bottles,
            Provider::RetroArch,
            Provider::Other,
        ]
    }

    /// Store identifier used by the PCGamingWiki / Ludusavi manifest `when.store` constraint.
    pub fn manifest_store(&self) -> Option<&'static str> {
        match self {
            Provider::Steam => Some("steam"),
            Provider::Gog => Some("gog"),
            Provider::EpicGames => Some("epic"),
            Provider::Amazon
            | Provider::Lutris
            | Provider::Bottles
            | Provider::RetroArch
            | Provider::Other => None,
        }
    }
}

/// Host operating system used for default game library discovery.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Windows,
    MacOs,
}

impl Platform {
    /// Detects the platform for which the application was compiled.
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }

    /// Human-friendly platform name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Platform::Linux => "Linux",
            Platform::Windows => "Windows",
            Platform::MacOs => "macOS",
        }
    }
}

/// Authentication mechanism used to connect to GitHub and manage the remote repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuthMethod {
    ExistingSshKey {
        private_key_path: PathBuf,
        public_key_path: Option<PathBuf>,
        passphrase: String,
        github_username: String,
        pat_for_api: Option<String>,
    },
    NewSshKey {
        private_key_path: PathBuf,
        public_key_path: PathBuf,
        passphrase: String,
        github_username: String,
        pat_for_api: Option<String>,
        key_generated: bool,
    },
    PersonalAccessToken {
        token: String,
        github_username: String,
    },
}

impl Default for AuthMethod {
    fn default() -> Self {
        let ssh_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ssh");
        AuthMethod::ExistingSshKey {
            private_key_path: ssh_dir.join("id_ed25519"),
            public_key_path: Some(ssh_dir.join("id_ed25519.pub")),
            passphrase: String::new(),
            github_username: String::new(),
            pat_for_api: None,
        }
    }
}

impl AuthMethod {
    /// Returns the configured GitHub username, if any.
    pub fn github_username(&self) -> &str {
        match self {
            AuthMethod::ExistingSshKey {
                github_username, ..
            }
            | AuthMethod::NewSshKey {
                github_username, ..
            }
            | AuthMethod::PersonalAccessToken {
                github_username, ..
            } => github_username,
        }
    }

    /// Returns a GitHub personal access token when available for API calls.
    pub fn pat_token(&self) -> Option<&str> {
        match self {
            AuthMethod::ExistingSshKey { pat_for_api, .. }
            | AuthMethod::NewSshKey { pat_for_api, .. } => pat_for_api
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            AuthMethod::PersonalAccessToken { token, .. } => {
                let trimmed = token.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }
        }
    }

    /// Returns the SSH private key path when using an SSH authentication method.
    pub fn ssh_private_key_path(&self) -> Option<&PathBuf> {
        match self {
            AuthMethod::ExistingSshKey {
                private_key_path, ..
            }
            | AuthMethod::NewSshKey {
                private_key_path, ..
            } => Some(private_key_path),
            AuthMethod::PersonalAccessToken { .. } => None,
        }
    }

    /// Returns true if enough credentials are provided to target a GitHub remote.
    pub fn has_remote_credentials(&self) -> bool {
        if self.github_username().trim().is_empty() {
            return false;
        }
        match self {
            AuthMethod::ExistingSshKey {
                private_key_path, ..
            } => private_key_path.exists(),
            AuthMethod::NewSshKey {
                private_key_path,
                key_generated,
                ..
            } => *key_generated && private_key_path.exists(),
            AuthMethod::PersonalAccessToken { token, .. } => !token.trim().is_empty(),
        }
    }
}

/// Sync schedule policy, configurable globally and optionally overridden per game.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum SyncRate {
    #[default]
    OnStartup,
    Manual,
    EveryMinutes(u32),
}

impl SyncRate {
    /// Human-readable description for UI display.
    pub fn display_label(&self) -> String {
        match self {
            SyncRate::OnStartup => "On Startup".to_string(),
            SyncRate::Manual => "Manual Only".to_string(),
            SyncRate::EveryMinutes(m) => format!("Every {m} min"),
        }
    }
}

/// Controls whether a game syncs its entire save directory or only chosen relative paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum SyncSelectionMode {
    #[default]
    EntireFolder,
    SelectedFiles(Vec<String>),
}

/// Theme color mode inspired by the jizihub Smoked Obsidian (dark) and Warm Sandstone (light) palettes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
}

impl ThemeMode {
    /// Human-friendly theme colorway label.
    pub fn display_name(&self) -> &'static str {
        match self {
            ThemeMode::Dark => "Dark",
            ThemeMode::Light => "Light",
        }
    }

    /// Returns the opposite theme mode.
    pub fn toggle(&self) -> Self {
        match self {
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
        }
    }
}

/// Severity level for entries in the application activity log.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl LogLevel {
    /// Short uppercase badge text for a log level.
    pub fn label(&self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Success => "OK",
            LogLevel::Warning => "WARN",
            LogLevel::Error => "ERR",
        }
    }
}

/// Timestamped entry rendered in the Logs tab and persisted to the local log file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub source: String,
    pub message: String,
}

/// User-assigned label attached to a specific game save state commit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SaveStateLabel {
    pub game_id: String,
    pub commit_hash: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
}

/// Portable catalog entry stored inside `sgm_repo_metadata.json` so a repository can
/// reconstruct games, providers, and labels after a system reset or when shared.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RepoGameCatalogEntry {
    pub id: String,
    pub name: String,
    pub game_slug: String,
    pub provider: Provider,
    pub app_id: Option<String>,
    pub repo_subpath: String,
    pub sync_selection: SyncSelectionMode,
    pub sync_across_providers: bool,
    pub active_save_label: Option<String>,
    #[serde(default)]
    pub save_dir_template: Option<String>,
    #[serde(default)]
    pub extra_roots: Vec<CatalogRoot>,
    #[serde(default)]
    pub exclude_globs: Vec<String>,
    #[serde(default)]
    pub profiles: Vec<SaveProfile>,
    #[serde(default)]
    pub main_profile_name: Option<String>,
}

/// Machine-independent description of an extra root, stored in the repository catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CatalogRoot {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub kind: RootKind,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub shared_across_profiles: bool,
}

/// Root portable metadata file stored in the save repository (`sgm_repo_metadata.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RepoMetadataManifest {
    pub version: u32,
    pub separate_folder_per_provider: bool,
    pub games: Vec<RepoGameCatalogEntry>,
    pub save_state_labels: Vec<SaveStateLabel>,
}

/// Tracked game entry with save directory, provider metadata, and sync options.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GameEntry {
    pub id: String,
    pub name: String,
    pub game_slug: String,
    pub provider: Provider,
    pub app_id: Option<String>,
    pub install_dir: Option<PathBuf>,
    pub save_dir: PathBuf,
    pub enabled: bool,
    pub sync_selection: SyncSelectionMode,
    pub sync_rate_override: Option<SyncRate>,
    pub sync_across_providers: bool,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_sync_status: Option<String>,
    #[serde(default)]
    pub active_save_label: Option<String>,

    // ---- v2 -----------------------------------------------------------------
    /// Wine / Proton prefix root (the folder that contains `drive_c`), when the game runs
    /// through a compatibility layer.
    #[serde(default)]
    pub prefix_dir: Option<PathBuf>,
    /// Root of the launcher library the game was found in (for Steam: the folder that
    /// contains `steamapps` and `userdata`).
    #[serde(default)]
    pub library_root: Option<PathBuf>,
    /// Portable form of `save_dir` (for example `<winAppData>/EldenRing`) so another machine
    /// can resolve the same location inside its own prefix / home.
    #[serde(default)]
    pub save_dir_template: Option<String>,
    /// Additional tracked folders (config, mod lists, secondary save locations).
    #[serde(default)]
    pub extra_roots: Vec<SaveRoot>,
    /// Glob patterns (relative to each root) that are never backed up for this game.
    #[serde(default)]
    pub exclude_globs: Vec<String>,
    /// How the save location was determined, and how sure SaveVault is about it.
    #[serde(default)]
    pub detection: Option<DetectionInfo>,
    /// Extra save profiles (characters / playthroughs) besides the implicit main profile.
    #[serde(default)]
    pub profiles: Vec<SaveProfile>,
    /// Id of the profile whose saves are currently live in the game folder ("" = main profile).
    #[serde(default)]
    pub active_profile: String,
    /// Display name of the implicit main profile.
    #[serde(default)]
    pub main_profile_name: Option<String>,
    /// Places SaveVault reads the installed mod list from, recorded with every snapshot.
    #[serde(default)]
    pub mod_sources: Vec<ModSource>,
    /// Per-machine bookkeeping used for two-way sync and conflict detection.
    #[serde(default)]
    pub sync_state: GameSyncState,
    /// Per-game override for "snapshot when the game closes" (`None` = follow global setting).
    #[serde(default)]
    pub backup_on_exit: Option<bool>,
    /// RetroArch launcher, core, save-state, and per-game Mouse & Keyboard override settings.
    #[serde(default)]
    pub retroarch: Option<RetroArchGameMeta>,
}

impl Default for GameEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            game_slug: String::new(),
            provider: Provider::Other,
            app_id: None,
            install_dir: None,
            save_dir: PathBuf::new(),
            enabled: true,
            sync_selection: SyncSelectionMode::EntireFolder,
            sync_rate_override: None,
            sync_across_providers: false,
            last_synced_at: None,
            last_sync_status: None,
            active_save_label: None,
            prefix_dir: None,
            library_root: None,
            save_dir_template: None,
            extra_roots: Vec::new(),
            exclude_globs: Vec::new(),
            detection: None,
            profiles: Vec::new(),
            active_profile: String::new(),
            main_profile_name: None,
            mod_sources: Vec::new(),
            sync_state: GameSyncState::default(),
            backup_on_exit: None,
            retroarch: None,
        }
    }
}

/// Id of the primary root (backed by `GameEntry::save_dir`).
pub const PRIMARY_ROOT_ID: &str = "saves";
/// Display name used for the implicit main profile when the user has not renamed it.
pub const DEFAULT_MAIN_PROFILE_NAME: &str = "Main";

/// What a tracked folder holds; drives defaults and how it is presented.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, Hash, PartialOrd, Ord)]
pub enum RootKind {
    #[default]
    Saves,
    Config,
    Mods,
}

impl RootKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            RootKind::Saves => "Saves",
            RootKind::Config => "Settings",
            RootKind::Mods => "Mod setup",
        }
    }
}

fn default_true() -> bool {
    true
}

/// One tracked folder of a game (saves, settings, or mod configuration).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SaveRoot {
    /// Stable id, unique within the game; used as the folder name inside the repository.
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub kind: RootKind,
    pub path: PathBuf,
    /// Glob patterns relative to `path`; empty means "everything".
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Shared roots are not swapped when switching profiles (e.g. graphics settings).
    #[serde(default)]
    pub shared_across_profiles: bool,
    /// Portable form of `path`.
    #[serde(default)]
    pub template: Option<String>,
}

/// How a save location was found.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum DetectionSource {
    /// Matched the PCGamingWiki-derived save location database.
    Database,
    /// Steam Cloud `userdata/<user>/<appid>` folder.
    SteamCloud,
    /// Ranked guess from scanning the game's prefix / install folder.
    Heuristic,
    /// Learned by watching which files changed while the game was running.
    PlaySession,
    /// Chosen by the user.
    Manual,
    /// Carried over from a configuration written by an older SaveVault version.
    #[default]
    Legacy,
    /// Restored from the cloud catalog written by another machine.
    Cloud,
}

impl DetectionSource {
    pub fn display_name(&self) -> &'static str {
        match self {
            DetectionSource::Database => "Save database",
            DetectionSource::SteamCloud => "Steam Cloud folder",
            DetectionSource::Heuristic => "Smart scan",
            DetectionSource::PlaySession => "Learned while playing",
            DetectionSource::Manual => "Set by you",
            DetectionSource::Legacy => "Earlier version",
            DetectionSource::Cloud => "From your cloud catalog",
        }
    }
}

/// Provenance and confidence for a game's detected save location.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DetectionInfo {
    pub source: DetectionSource,
    /// 0–100.
    pub confidence: u8,
    /// One plain-language sentence explaining the match.
    pub reason: String,
    #[serde(default)]
    pub database_title: Option<String>,
}

/// A named set of saves for one game (a character, a playthrough, a modded run…).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SaveProfile {
    /// Stable slug used as the folder name in the repository.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub note: String,
    pub created_at: DateTime<Utc>,
}

/// Where to read a game's installed-mod list from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ModSourceKind {
    /// Text file with one entry per line (`plugins.txt`, `modlist.txt`, `loadorder.txt`).
    ListFile,
    /// Folder whose direct children are mods (`Mods/`, `BepInEx/plugins/`).
    #[default]
    Folder,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModSource {
    pub label: String,
    pub path: PathBuf,
    #[serde(default)]
    pub kind: ModSourceKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Summary of the mods installed when a snapshot was taken.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ModFingerprint {
    /// SHA-256 over the sorted entry list.
    pub hash: String,
    pub entries: Vec<String>,
}

/// Unresolved difference between this machine's saves and the cloud copy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConflictInfo {
    pub detected_at: DateTime<Utc>,
    /// Commit that holds the cloud side.
    pub cloud_commit: String,
    #[serde(default)]
    pub cloud_machine: Option<String>,
    #[serde(default)]
    pub cloud_time: Option<String>,
}

/// Per-machine sync bookkeeping for the active profile of a game.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GameSyncState {
    /// Cheap stat-based fingerprint of the live save files after the last sync / restore.
    #[serde(default)]
    pub live_fingerprint: Option<String>,
    /// Content digest of the repository copy after the last sync / restore.
    #[serde(default)]
    pub mirror_digest: Option<String>,
    #[serde(default)]
    pub conflict: Option<ConflictInfo>,
    /// Last time SaveVault saw the game process exit.
    #[serde(default)]
    pub last_played_at: Option<DateTime<Utc>>,
}

/// What to do when both this machine and the cloud changed a game's saves.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ConflictPolicy {
    /// Keep both, leave live saves untouched, and ask.
    #[default]
    Ask,
    /// This machine wins; the cloud version stays in history.
    PreferLocal,
    /// The cloud wins; this machine's saves go to a safety backup first.
    PreferCloud,
}

impl ConflictPolicy {
    pub fn display_name(&self) -> &'static str {
        match self {
            ConflictPolicy::Ask => "Ask me",
            ConflictPolicy::PreferLocal => "Keep this PC's saves",
            ConflictPolicy::PreferCloud => "Use the cloud saves",
        }
    }
}

impl GameEntry {
    /// The primary root expressed as a `SaveRoot`.
    pub fn primary_root(&self) -> SaveRoot {
        SaveRoot {
            id: PRIMARY_ROOT_ID.to_string(),
            label: "Saves".to_string(),
            kind: RootKind::Saves,
            path: self.save_dir.clone(),
            include: match &self.sync_selection {
                SyncSelectionMode::EntireFolder => Vec::new(),
                SyncSelectionMode::SelectedFiles(list) => list.clone(),
            },
            exclude: Vec::new(),
            enabled: true,
            shared_across_profiles: false,
            template: self.save_dir_template.clone(),
        }
    }

    /// All enabled roots, primary first.
    pub fn active_roots(&self) -> Vec<SaveRoot> {
        let mut out = vec![self.primary_root()];
        out.extend(self.extra_roots.iter().filter(|r| r.enabled).cloned());
        out
    }

    /// Display name of the profile with `id` ("" = main).
    pub fn profile_name(&self, id: &str) -> String {
        if id.is_empty() {
            return self
                .main_profile_name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_MAIN_PROFILE_NAME.to_string());
        }
        self.profiles
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// Ids of every profile, main ("") first.
    pub fn profile_ids(&self) -> Vec<String> {
        let mut ids = vec![String::new()];
        ids.extend(self.profiles.iter().map(|p| p.id.clone()));
        ids
    }

    /// True when the game has no usable save location on this machine yet.
    pub fn is_placeholder(&self) -> bool {
        self.save_dir.as_os_str().is_empty()
    }

    /// Resolves the effective sync rate by applying a per-game override over the global rate.
    pub fn effective_sync_rate(&self, global: &SyncRate) -> SyncRate {
        self.sync_rate_override.unwrap_or(*global)
    }
}

/// Configured game library directories for a single provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderLibraryConfig {
    pub provider: Provider,
    pub library_paths: Vec<PathBuf>,
    pub enabled: bool,
}

/// Warning emitted when a save file exceeds the GitHub / LFS threshold.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LargeFileWarning {
    pub game_name: String,
    pub provider: Provider,
    pub file_path: PathBuf,
    pub relative_path: String,
    pub size_bytes: u64,
    pub threshold_bytes: u64,
    pub split_applied: bool,
    pub message: String,
}

/// Git commit metadata for rollback inspection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitInfo {
    pub commit_hash: String,
    pub short_hash: String,
    pub timestamp: String,
    pub message: String,
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// Machine that produced the snapshot, parsed from the commit trailer.
    #[serde(default)]
    pub machine: Option<String>,
    /// Seconds since the Unix epoch (for "3 hours ago" style display).
    #[serde(default)]
    pub unix_time: i64,
}

/// Root persisted configuration for Save Game Manager.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    pub repo_name: String,
    pub local_repo_path: PathBuf,
    pub auth: AuthMethod,
    pub separate_folder_per_provider: bool,
    pub global_sync_rate: SyncRate,
    pub split_large_saves: bool,
    pub lfs_warning_threshold_mb: u64,
    pub chunk_size_mb: u64,
    pub provider_libraries: Vec<ProviderLibraryConfig>,
    pub games: Vec<GameEntry>,
    #[serde(default)]
    pub theme_mode: ThemeMode,
    #[serde(default)]
    pub sync_enabled: bool,
    #[serde(default)]
    pub run_on_system_startup: bool,
    #[serde(default)]
    pub save_state_labels: Vec<SaveStateLabel>,

    // ---- v2 -----------------------------------------------------------------
    #[serde(default)]
    pub schema_version: u32,
    /// Friendly name of this computer, recorded with every snapshot.
    #[serde(default)]
    pub machine_name: String,
    /// Download and refresh the PCGamingWiki-derived save location database automatically.
    #[serde(default = "default_true")]
    pub auto_update_detection_db: bool,
    /// Add newly installed games with known save locations without asking.
    #[serde(default = "default_true")]
    pub auto_track_new_games: bool,
    /// Take a snapshot when a tracked game closes.
    #[serde(default = "default_true")]
    pub backup_on_game_exit: bool,
    /// Keep a zipped copy of the live saves before anything overwrites them.
    #[serde(default = "default_true")]
    pub safety_backups: bool,
    #[serde(default = "default_safety_keep")]
    pub safety_backup_keep: u32,
    /// Glob patterns never backed up for any game.
    #[serde(default)]
    pub global_exclude_globs: Vec<String>,
    #[serde(default)]
    pub conflict_policy: ConflictPolicy,
    /// Also track settings folders reported by the save database.
    #[serde(default)]
    pub include_config_roots: bool,
    #[serde(default)]
    pub onboarding_done: bool,
    /// Any git remote (URL or folder, e.g. a NAS share or USB drive) used instead of GitHub.
    #[serde(default)]
    pub custom_remote_url: Option<String>,
    /// Games the user removed or declined; scans do not add them again.
    #[serde(default)]
    pub ignored_game_ids: Vec<String>,
    /// Default Mouse & Keyboard override settings seeded when enabling KBM on a new game.
    #[serde(default)]
    pub default_kbm_profile: KbmProfile,
    /// RetroArch binary name or full path used by the RetroArch game launcher.
    #[serde(default = "default_retroarch_binary")]
    pub retroarch_binary: String,
}

fn default_safety_keep() -> u32 {
    10
}

pub fn default_retroarch_binary() -> String {
    "retroarch".to_string()
}

impl AppConfig {
    /// The custom remote, if one is set and non-empty.
    pub fn custom_remote(&self) -> Option<&str> {
        self.custom_remote_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// True when a cloud / remote copy can be used (custom remote or GitHub credentials).
    pub fn remote_configured(&self) -> bool {
        self.custom_remote().is_some() || self.auth.has_remote_credentials()
    }
}

/// Current on-disk configuration schema.
pub const CONFIG_SCHEMA_VERSION: u32 = 2;
