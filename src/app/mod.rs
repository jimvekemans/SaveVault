//! Desktop GUI.
//!
//! The UI thread never blocks on disk or network: every operation runs as a background job
//! on a copy of the configuration and reports back over a channel. A watcher thread reports
//! play sessions and save-file changes so snapshots happen on their own.

mod editor;
mod games;
mod import;
mod kbm_ui;
mod logs;
mod settings;

use kbm_ui::{KbmRebindScope, KbmRebindState, render_kbm_profile_editor};

use crate::auth::{
    RepoEnsureResult, ensure_remote_repo_exists, generate_ssh_keypair, public_key_path_for,
    read_public_key_for_private_key, remote_url_for_auth, upload_ssh_public_key_to_github,
    verify_github_token,
};
use crate::autostart::set_system_autostart;
use crate::config::{default_config_path, load_config, save_config};
use crate::detect::db::{SaveDb, UpdateOutcome, default_db_dir, update_database};
use crate::detect::heuristics::{Candidate, learn_from_session};
use crate::detect::mods::{ModDiff, diff_fingerprints, scan_mod_fingerprint};
use crate::detect::template::{PathContext, tokenize_path};
use crate::detect::{
    AUTO_ENABLE_CONFIDENCE, Detection, ScanSummary, apply_detection, merge_scan_results,
    scan_installed_games,
};
use crate::git_ops::{init_or_open_local_repo, list_commits_for_path, unpushed_commit_count};
use crate::logo::{APP_NAME, APP_SUBTITLE, WINDOW_TITLE, build_app_icon, paint_logo_mark};
use crate::models::{
    AppConfig, AuthMethod, CommitInfo, ConflictPolicy, DetectionInfo, DetectionSource, GameEntry,
    LargeFileWarning, LogEntry, LogLevel, ModSource, ModSourceKind, Platform, Provider, RootKind,
    SaveRoot, SyncRate, SyncSelectionMode, ThemeMode,
};
use crate::portability::{
    ExternalImportRequest, OnlineSaveImportRequest, RepoInspection,
    format_repo_entry_summary, import_game_from_external_repo, import_online_save,
    inspect_repository, prepare_external_repo, reinstate_full_structure,
};
use crate::profiles::{
    ProfileStart, ProfileSummary, create_profile, delete_profile, profile_summaries, rename_profile,
    switch_profile,
};
use crate::providers::{default_library_paths, find_multi_provider_groups, slugify_game_name};
use crate::safety::{
    BackupInfo, SafetyBackup, create_safety_backup, list_safety_backups, restore_roots_zip,
    safety_root, write_roots_zip,
};
use crate::save_editor::{
    LoadedSaveFile, SaveFileFormat, format_json_content, inspect_save_file, save_edited_file,
    validate_save_content,
};
use crate::sync_engine::{
    GameHealth, LiveState, RestorePreview, SyncOptions, SyncOutcome,
    game_health, games_due_periodic, list_all_save_files, list_snapshots, live_state,
    preview_restore, pull_cloud_catalog, remove_game_entry,
    repo_subpath_for_game, resolve_conflict_keep_local, resolve_conflict_use_cloud,
    rollback_game_to_commit, set_save_state_label, sync_games,
};
use crate::theme::{
    self, JizihubPalette, accent_button, glass_frame_1, glass_frame_2, glass_frame_3,
    outline_orange_button, paint_ambient_backdrop, render_badge,
};
use crate::watcher::{SessionEvent, WatchEvent, WatcherHandle, spawn_watcher};
use anyhow::Result;
use chrono::{Local, Utc};
use eframe::egui;
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

const LOG_ROTATE_BYTES: u64 = 2 * 1024 * 1024;
/// Wait after a game closes before snapshotting, so it (and Steam Cloud) finish writing.
const POST_SESSION_DELAY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppTab {
    Games,
    Import,
    SaveEditor,
    Logs,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsHighlight {
    CloudSignIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    Overview,
    Controls,
    Timeline,
    Profiles,
    Folders,
    Advanced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GameFilter {
    All,
    Attention,
    Store(Provider),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthKindChoice {
    ExistingSshKey,
    NewSshKey,
    PersonalAccessToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinaryViewMode {
    HexDump,
    Strings,
}

struct SaveEditorState {
    loaded: Option<LoadedSaveFile>,
    edit_buffer: String,
    dirty: bool,
    create_backup_on_save: bool,
    binary_view: BinaryViewMode,
    string_filter: String,
    last_message: Option<(String, bool)>,
}

impl Default for SaveEditorState {
    fn default() -> Self {
        Self {
            loaded: None,
            edit_buffer: String::new(),
            dirty: false,
            create_backup_on_save: true,
            binary_view: BinaryViewMode::HexDump,
            string_filter: String::new(),
            last_message: None,
        }
    }
}

/// What a finished background job hands back to the UI.
enum JobPayload {
    None,
    Sync(SyncOutcome),
    Scan(ScanSummary),
    Db(Option<Arc<SaveDb>>),
    RepoEnsure(RepoEnsureResult, Option<String>),
    TokenUser(String),
    Inspect(Box<(RepoInspection, HashMap<String, Vec<CommitInfo>>)>),
    Preview {
        game_id: String,
        commit: String,
        preview: Box<RestorePreview>,
        mod_diff: ModDiff,
    },
    Learned {
        game_id: String,
        found: Option<Candidate>,
    },
    /// Timeline / file lists for the selected game should be reloaded.
    Refresh,
}

struct JobOutput {
    message: String,
    is_error: bool,
    payload: JobPayload,
}

impl JobOutput {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            is_error: false,
            payload: JobPayload::None,
        }
    }

    fn with(message: impl Into<String>, payload: JobPayload) -> Self {
        Self {
            message: message.into(),
            is_error: false,
            payload,
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum WorkerEvent {
    Log {
        level: LogLevel,
        source: String,
        message: String,
    },
    Done {
        title: String,
        config: Option<Box<AppConfig>>,
        result: Result<JobOutput>,
    },
}

/// Handed to job closures.
struct JobCtx {
    tx: Sender<WorkerEvent>,
    db: Option<Arc<SaveDb>>,
    running: BTreeSet<String>,
}

impl JobCtx {
    fn log(&self, level: LogLevel, source: &str, message: impl Into<String>) {
        let _ = self.tx.send(WorkerEvent::Log {
            level,
            source: source.to_string(),
            message: message.into(),
        });
    }
}

/// Work waiting for the current job to finish (or for a delay to pass).
enum Pending {
    Sync {
        not_before: Instant,
        ids: Vec<String>,
        title: String,
    },
    Learn {
        not_before: Instant,
        game_id: String,
        since: SystemTime,
    },
    Scan,
}

struct AddGameDraft {
    open: bool,
    name: String,
    provider: Provider,
    save_dir: String,
}

impl Default for AddGameDraft {
    fn default() -> Self {
        Self {
            open: false,
            name: String::new(),
            provider: Provider::Other,
            save_dir: String::new(),
        }
    }
}

#[derive(Default)]
struct NewProfileDraft {
    name: String,
    note: String,
    copy_current: bool,
    /// Branch from this snapshot instead (set from the timeline).
    from_commit: Option<(String, String)>,
    switch_after: bool,
}

struct RestoreDialog {
    game_id: String,
    commit: CommitInfo,
    preview: Option<RestorePreview>,
    mod_diff: ModDiff,
}

struct ImportTabState {
    reinstate_source: String,
    reinstate_fallback_dir: String,

    ext_repo_source: String,
    ext_inspection: Option<RepoInspection>,
    ext_game_commits: HashMap<String, Vec<CommitInfo>>,
    ext_selected_game_id: Option<String>,
    ext_selected_commit: Option<String>,
    ext_target_provider: Provider,
    ext_target_save_dir: String,
    ext_custom_label: String,

    online_source_path: String,
    online_target_existing_id: Option<String>,
    online_game_name: String,
    online_provider: Provider,
    online_target_save_dir: String,
    online_label: String,
    online_backup_existing: bool,
}

impl Default for ImportTabState {
    fn default() -> Self {
        let default_fallback = crate::config::default_data_dir().join("restored_saves");
        Self {
            reinstate_source: String::new(),
            reinstate_fallback_dir: default_fallback.display().to_string(),
            ext_repo_source: String::new(),
            ext_inspection: None,
            ext_game_commits: HashMap::new(),
            ext_selected_game_id: None,
            ext_selected_commit: None,
            ext_target_provider: Provider::Steam,
            ext_target_save_dir: default_fallback.join("imported").display().to_string(),
            ext_custom_label: String::new(),
            online_source_path: String::new(),
            online_target_existing_id: None,
            online_game_name: String::new(),
            online_provider: Provider::Steam,
            online_target_save_dir: String::new(),
            online_label: "Online Import".to_string(),
            online_backup_existing: true,
        }
    }
}

/// Desktop GUI state for SaveVault.
pub struct SaveGameManagerApp {
    config: AppConfig,
    config_path: PathBuf,
    active_tab: AppTab,

    // Games view
    search: String,
    filter: GameFilter,
    selected_game_id: Option<String>,
    detail_tab: DetailTab,
    available_files_cache: Vec<String>,
    cached_files_for_game: Option<String>,
    sync_label_input: String,
    commits: Vec<CommitInfo>,
    commits_loaded_for: Option<(String, String)>,
    commit_filter: String,
    editing_label_commit: Option<String>,
    editing_label_text: String,
    restore_dialog: Option<RestoreDialog>,
    pending_remove_game_id: Option<String>,
    new_profile: NewProfileDraft,
    renaming_profile: Option<(String, String)>,
    profile_cache: Option<(String, Vec<ProfileSummary>)>,
    safety_cache: Option<(String, Vec<SafetyBackup>)>,
    mods_cache: Option<(String, Option<usize>)>,
    save_states_cache: Option<(String, Vec<PathBuf>)>,
    kbm_rebind: Option<KbmRebindState>,
    kbm_dirty_game_id: Option<String>,
    kbm_dirty_default: bool,
    kbm_last_live_sync: Instant,
    add_root_kind: RootKind,
    exclude_buffer: Option<(String, String)>,
    suggestions: HashMap<String, Detection>,
    alternatives: HashMap<String, Vec<Candidate>>,
    scan_banner: Option<String>,
    /// Installed games a scan found that are not tracked yet.
    untracked: Vec<GameEntry>,
    /// The local repository has a remote but no usable sign-in.
    cloud_needs_sign_in: bool,
    settings_highlight: Option<SettingsHighlight>,
    scroll_to_settings_highlight: bool,
    live_states: HashMap<String, LiveState>,
    running: BTreeSet<String>,
    add_game_draft: AddGameDraft,

    import_state: ImportTabState,
    save_editor: SaveEditorState,

    warnings: Vec<LargeFileWarning>,
    status_message: String,
    is_error_status: bool,

    logs: Vec<LogEntry>,
    log_filter_level: Option<LogLevel>,
    log_search: String,
    log_file_path: PathBuf,

    busy: bool,
    busy_title: String,
    rx: Option<Receiver<WorkerEvent>>,
    pending: Vec<Pending>,

    db: Option<Arc<SaveDb>>,
    /// The database load has finished (successfully or not), so scans may proceed.
    db_checked: bool,
    watcher: Option<WatcherHandle>,
    watch_rx: Option<Receiver<WatchEvent>>,

    ssh_pub_key_preview: String,
    custom_minutes_buffer: u32,
    last_periodic_check: Instant,
    startup_sync_has_run: bool,
    start_minimized: bool,
    first_frame: bool,
    applied_theme: Option<ThemeMode>,
    unpushed: usize,
    overlay_mode: bool,
    overlay_compact: bool,
    overlay_ipc: Option<crate::overlay::OverlayIpcServer>,
    overlay_hotkey_capturing: Option<u8>,
    config_mtime: Option<SystemTime>,
    /// Developer aid: capture the window to a PPM file and exit (see `SAVEVAULT_SCREENSHOT`).
    capture: Option<CaptureRequest>,
}

/// Set by `SAVEVAULT_SCREENSHOT=<file.ppm>`; optional `SAVEVAULT_VIEW=<tab>[:<detail tab>]`
/// and `SAVEVAULT_SELECT=<game id>` choose what is on screen. Used to check the UI in CI /
/// headless sessions without a desktop screenshot tool.
struct CaptureRequest {
    path: PathBuf,
    started: Instant,
    requested: bool,
    done: Arc<std::sync::atomic::AtomicBool>,
}

fn default_log_file_path() -> PathBuf {
    crate::config::default_data_dir().join("savevault.log")
}

fn open_external_url(url: &str) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .spawn();
}

fn open_folder(path: &Path) {
    let target = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.to_path_buf())
    };
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(&target).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&target).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(&target).spawn();
}

fn render_external_link(ui: &mut egui::Ui, pal: &JizihubPalette, label: &str, url: &str) {
    let resp = ui
        .add(
            egui::Button::new(egui::RichText::new(label).color(pal.accent_text).underline())
                .frame(false),
        )
        .on_hover_text(url)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        open_external_url(url);
    }
}

fn detect_existing_ssh_keys() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let ssh_dir = home.join(".ssh");
    let mut keys = Vec::new();
    for candidate in ["id_ed25519", "id_rsa", "id_ecdsa", "save_game_manager_ed25519"] {
        let p = ssh_dir.join(candidate);
        if p.is_file() {
            keys.push(p);
        }
    }
    keys
}

/// "3 hours ago" style rendering of a Unix timestamp.
fn relative_time(unix: i64) -> String {
    if unix <= 0 {
        return String::new();
    }
    let secs = (Utc::now().timestamp() - unix).max(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=2_591_999 => format!("{} day(s) ago", secs / 86_400),
        _ => format!("{} month(s) ago", secs / 2_592_000),
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

impl SaveGameManagerApp {
    /// Initializes the GUI app, loads persisted settings, and starts background services.
    pub fn new(cc: &eframe::CreationContext<'_>, start_minimized: bool) -> Self {
        Self::new_with_mode(cc, start_minimized, false)
    }

    /// Initializes either the full desktop GUI (`overlay_mode = false`) or the always-on-top
    /// in-game screen overlay (`overlay_mode = true`).
    pub fn new_with_mode(
        cc: &eframe::CreationContext<'_>,
        start_minimized: bool,
        overlay_mode: bool,
    ) -> Self {
        let config_path = default_config_path();
        let config_mtime = fs::metadata(&config_path).and_then(|m| m.modified()).ok();
        let (config, load_msg, is_err) = match load_config(&config_path) {
            Ok(cfg) => (cfg, "Ready.".to_string(), false),
            Err(err) => (
                crate::config::default_config(),
                format!("Could not read your settings, using defaults: {err}"),
                true,
            ),
        };

        theme::apply_jizihub_theme(&cc.egui_ctx, config.theme_mode);

        let initial_probes = crate::watcher::probes_for(&config.games);
        let initial_running = crate::watcher::running_games(
            &initial_probes,
            &crate::watcher::ProcessLister::new().cmdlines(),
        );
        let selected_game_id = if overlay_mode {
            initial_running
                .iter()
                .next()
                .cloned()
                .or_else(|| {
                    config
                        .games
                        .iter()
                        .filter(|g| !g.is_placeholder())
                        .max_by_key(|g| g.sync_state.last_played_at.or(g.last_synced_at))
                        .map(|g| g.id.clone())
                })
                .or_else(|| config.games.first().map(|g| g.id.clone()))
        } else {
            config.games.first().map(|g| g.id.clone())
        };
        let custom_minutes_buffer = match config.global_sync_rate {
            SyncRate::EveryMinutes(m) => m.max(1),
            _ => 30,
        };

        let mut import_state = ImportTabState::default();
        if let Some(first) = config.games.first() {
            import_state.online_target_existing_id = Some(first.id.clone());
            import_state.online_game_name = first.name.clone();
            import_state.online_provider = first.provider;
            import_state.online_target_save_dir = first.save_dir.display().to_string();
        }

        let log_file_path = default_log_file_path();
        if fs::metadata(&log_file_path).map(|m| m.len()).unwrap_or(0) > LOG_ROTATE_BYTES {
            let _ = fs::rename(&log_file_path, log_file_path.with_extension("log.1"));
        }

        let (watch_tx, watch_rx) = mpsc::channel();
        let wake_ctx = cc.egui_ctx.clone();
        let watcher = spawn_watcher(&config, watch_tx, Box::new(move || wake_ctx.request_repaint()));

        let applied_theme = Some(config.theme_mode);
        let mut app = Self {
            config,
            config_path,
            active_tab: AppTab::Games,
            search: String::new(),
            filter: GameFilter::All,
            selected_game_id,
            detail_tab: DetailTab::Overview,
            available_files_cache: Vec::new(),
            cached_files_for_game: None,
            sync_label_input: String::new(),
            commits: Vec::new(),
            commits_loaded_for: None,
            commit_filter: String::new(),
            editing_label_commit: None,
            editing_label_text: String::new(),
            restore_dialog: None,
            pending_remove_game_id: None,
            new_profile: NewProfileDraft::default(),
            renaming_profile: None,
            profile_cache: None,
            safety_cache: None,
            mods_cache: None,
            save_states_cache: None,
            kbm_rebind: None,
            kbm_dirty_game_id: None,
            kbm_dirty_default: false,
            kbm_last_live_sync: Instant::now() - Duration::from_secs(1),
            add_root_kind: RootKind::Saves,
            exclude_buffer: None,
            suggestions: HashMap::new(),
            alternatives: HashMap::new(),
            scan_banner: None,
            untracked: Vec::new(),
            cloud_needs_sign_in: false,
            settings_highlight: None,
            scroll_to_settings_highlight: false,
            live_states: HashMap::new(),
            running: initial_running,
            add_game_draft: AddGameDraft::default(),
            import_state,
            save_editor: SaveEditorState::default(),
            warnings: Vec::new(),
            status_message: load_msg.clone(),
            is_error_status: is_err,
            logs: Vec::new(),
            log_filter_level: None,
            log_search: String::new(),
            log_file_path,
            busy: false,
            busy_title: String::new(),
            rx: None,
            pending: Vec::new(),
            db: None,
            db_checked: false,
            watcher: Some(watcher),
            watch_rx: Some(watch_rx),
            ssh_pub_key_preview: String::new(),
            custom_minutes_buffer,
            last_periodic_check: Instant::now(),
            startup_sync_has_run: false,
            start_minimized,
            first_frame: true,
            applied_theme,
            unpushed: 0,
            overlay_mode,
            overlay_compact: overlay_mode,
            overlay_ipc: None,
            overlay_hotkey_capturing: None,
            config_mtime,
            capture: std::env::var_os("SAVEVAULT_SCREENSHOT").map(|p| CaptureRequest {
                path: PathBuf::from(p),
                started: Instant::now(),
                requested: false,
                done: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }),
        };
        if app.capture.is_some() {
            if let Ok(sel) = std::env::var("SAVEVAULT_SELECT")
                && app.config.games.iter().any(|g| g.id == sel)
            {
                app.selected_game_id = Some(sel);
            }
            if let Ok(view) = std::env::var("SAVEVAULT_VIEW") {
                let mut parts = view.split(':');
                app.active_tab = match parts.next().unwrap_or("") {
                    "import" => AppTab::Import,
                    "editor" => AppTab::SaveEditor,
                    "logs" => AppTab::Logs,
                    "settings" => AppTab::Settings,
                    _ => AppTab::Games,
                };
                app.detail_tab = match parts.next().unwrap_or("") {
                    "controls" | "kbm" => DetailTab::Controls,
                    "timeline" => DetailTab::Timeline,
                    "profiles" => DetailTab::Profiles,
                    "folders" => DetailTab::Folders,
                    "more" => DetailTab::Advanced,
                    _ => DetailTab::Overview,
                };
            }
        }

        app.push_log(
            if is_err { LogLevel::Error } else { LogLevel::Info },
            "startup",
            format!("SaveVault {} started. {load_msg}", env!("CARGO_PKG_VERSION")),
        );
        app.refresh_ssh_preview();
        app.refresh_selected_game_files();
        app.refresh_cloud_facts();

        if overlay_mode {
            app.db = SaveDb::load_from_dir(&default_db_dir())
                .ok()
                .flatten()
                .map(Arc::new);
            app.db_checked = true;
            app.refresh_live_states();
            app.refresh_timeline();
            let wake_ctx = cc.egui_ctx.clone();
            app.overlay_ipc = crate::overlay::OverlayIpcServer::start(Box::new(move || {
                wake_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                wake_ctx.request_repaint();
            }));
        } else {
            if app.config.overlay_enabled {
                let _ = crate::overlay::sync_os_overlay_hotkey(true, &app.config.overlay_hotkey);
            }
            // Load (and if allowed refresh) the save location database, then look for games.
            app.start_db_load();
            if app.config.onboarding_done {
                app.pending.push(Pending::Scan);
                let due = app.startup_ids();
                if !due.is_empty() {
                    app.pending.push(Pending::Sync {
                        not_before: Instant::now(),
                        ids: due,
                        title: "Startup check".to_string(),
                    });
                }
                app.startup_sync_has_run = true;
            }
        }
        app
    }

    /// Enabled games scheduled for the startup pass.
    fn startup_ids(&self) -> Vec<String> {
        self.config
            .games
            .iter()
            .filter(|g| {
                g.enabled
                    && !g.is_placeholder()
                    && g.effective_sync_rate(&self.config.global_sync_rate) == SyncRate::OnStartup
            })
            .map(|g| g.id.clone())
            .collect()
    }

    fn persist_config(&mut self) {
        if let Err(err) = save_config(&self.config_path, &self.config) {
            self.set_status(format!("Failed to save settings: {err}"), true);
        } else {
            self.config_mtime = fs::metadata(&self.config_path).and_then(|m| m.modified()).ok();
            let _ = crate::retroarch::sync_kbm_registry_for_config(&self.config_path, &self.config);
        }
        if let Some(w) = &self.watcher {
            w.update_config(&self.config);
        }
        self.refresh_cloud_facts();
    }

    fn check_external_config_reload(&mut self) {
        if self.busy
            || self.kbm_dirty_game_id.is_some()
            || self.kbm_dirty_default
            || self.kbm_rebind.is_some()
            || self.overlay_hotkey_capturing.is_some()
        {
            return;
        }
        let disk_mtime = fs::metadata(&self.config_path).and_then(|m| m.modified()).ok();
        if disk_mtime.is_some()
            && disk_mtime != self.config_mtime
            && let Ok(fresh) = load_config(&self.config_path)
        {
            self.config = fresh;
            self.config_mtime = disk_mtime;
            if self
                .selected_game_id
                .as_deref()
                .is_none_or(|sel| !self.config.games.iter().any(|g| g.id == sel))
            {
                self.selected_game_id = self.config.games.first().map(|g| g.id.clone());
            }
            self.invalidate_game_caches();
            self.refresh_selected_game_files();
            self.refresh_live_states();
            self.refresh_cloud_facts();
            if let Some(w) = &self.watcher {
                w.update_config(&self.config);
            }
        }
    }

    fn preview_live_kbm_game(&mut self, game_id: &str) {
        self.kbm_dirty_game_id = Some(game_id.to_string());
        if self.kbm_last_live_sync.elapsed() >= Duration::from_millis(150) {
            self.kbm_last_live_sync = Instant::now();
            let _ = crate::retroarch::sync_kbm_registry_for_config(&self.config_path, &self.config);
        }
    }

    fn commit_kbm_game(&mut self, game_id: &str, ensure_bridge: bool) {
        if self.kbm_dirty_game_id.as_deref() == Some(game_id) {
            self.kbm_dirty_game_id = None;
        }
        if let Some(game) = self.config.games.iter().find(|g| g.id == game_id).cloned()
            && let Err(err) = crate::retroarch::apply_kbm_profile_to_retroarch(&game)
        {
            self.set_status(format!("Saved in app, but RetroArch .cfg failed: {err}"), true);
        }
        self.persist_config();
        if ensure_bridge {
            let reg_path = crate::retroarch::kbm_profiles_path_for(&self.config_path);
            crate::retroarch::ensure_mkb_bridge_running(&reg_path);
        }
    }

    fn launch_retroarch_game_action(&mut self, game_id: &str) {
        let Some(game) = self.config.games.iter().find(|g| g.id == game_id).cloned() else {
            return;
        };
        match crate::retroarch::launch_retroarch_game(&self.config_path, &self.config, &game) {
            Ok(pid) => {
                self.set_status(
                    format!("Launched {} in RetroArch (PID {pid}).", game.name),
                    false,
                );
            }
            Err(err) => {
                self.set_status(format!("Failed to launch {}: {err:#}", game.name), true);
            }
        }
    }

    fn push_log(&mut self, level: LogLevel, source: impl Into<String>, message: impl Into<String>) {
        let source = source.into();
        let message = message.into();
        let ts_short = Local::now().format("%H:%M:%S").to_string();
        let ts_full = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

        self.logs.push(LogEntry {
            timestamp: ts_short,
            level,
            source: source.clone(),
            message: message.clone(),
        });
        if self.logs.len() > 2000 {
            let excess = self.logs.len() - 2000;
            self.logs.drain(0..excess);
        }

        if let Some(parent) = self.log_file_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file_path)
        {
            let _ = writeln!(file, "[{ts_full}] [{}] [{source}] {message}", level.label());
        }
    }

    fn set_status(&mut self, msg: impl Into<String>, is_error: bool) {
        let message = msg.into();
        self.push_log(
            if is_error { LogLevel::Error } else { LogLevel::Info },
            "status",
            message.clone(),
        );
        self.status_message = message;
        self.is_error_status = is_error;
    }

    fn selected_game(&self) -> Option<&GameEntry> {
        let id = self.selected_game_id.as_deref()?;
        self.config.games.iter().find(|g| g.id == id)
    }

    fn health_of(&self, game: &GameEntry) -> GameHealth {
        game_health(game, self.running.contains(&game.id), self.live_states.get(&game.id))
    }

    fn cloud_enabled(&self) -> bool {
        self.config.sync_enabled && self.config.remote_configured()
    }

    fn open_file_in_editor(&mut self, path: &Path) {
        match inspect_save_file(path) {
            Ok(loaded) => {
                self.save_editor.edit_buffer = loaded.text_content.clone();
                self.save_editor.dirty = false;
                let msg = format!(
                    "Loaded {} ({} bytes, {})",
                    loaded.path.display(),
                    loaded.size_bytes,
                    loaded.format.display_name()
                );
                self.save_editor.last_message = Some((msg.clone(), false));
                self.save_editor.loaded = Some(loaded);
                self.active_tab = AppTab::SaveEditor;
                self.set_status(msg, false);
            }
            Err(err) => {
                let err_msg = format!("Failed to open save file in editor: {err}");
                self.save_editor.last_message = Some((err_msg.clone(), true));
                self.set_status(err_msg, true);
            }
        }
    }

    fn refresh_ssh_preview(&mut self) {
        self.ssh_pub_key_preview = match &self.config.auth {
            AuthMethod::ExistingSshKey {
                private_key_path, ..
            }
            | AuthMethod::NewSshKey {
                private_key_path, ..
            } => read_public_key_for_private_key(private_key_path).unwrap_or_default(),
            AuthMethod::PersonalAccessToken { .. } => String::new(),
        };
    }

    fn refresh_selected_game_files(&mut self) {
        let Some(id) = self.selected_game_id.clone() else {
            self.available_files_cache.clear();
            self.cached_files_for_game = None;
            return;
        };
        if let Some(game) = self.config.games.iter().find(|g| g.id == id) {
            self.available_files_cache = if game.is_placeholder() {
                Vec::new()
            } else {
                list_all_save_files(&game.save_dir)
            };
            self.cached_files_for_game = Some(id);
        }
    }

    fn invalidate_game_caches(&mut self) {
        self.commits_loaded_for = None;
        self.profile_cache = None;
        self.safety_cache = None;
        self.mods_cache = None;
        self.save_states_cache = None;
        self.cached_files_for_game = None;
    }

    fn select_game(&mut self, id: String) {
        if self.selected_game_id.as_deref() != Some(id.as_str()) {
            self.selected_game_id = Some(id);
            self.invalidate_game_caches();
            self.kbm_rebind = None;
            self.editing_label_commit = None;
            self.renaming_profile = None;
            self.exclude_buffer = None;
            self.sync_label_input.clear();
            self.refresh_selected_game_files();
        }
    }

    fn refresh_timeline(&mut self) {
        let Some(game) = self.selected_game().cloned() else {
            self.commits.clear();
            self.commits_loaded_for = None;
            return;
        };
        let key = (game.id.clone(), game.active_profile.clone());
        match list_snapshots(&self.config, &game.id, &game.active_profile, 200) {
            Ok(list) => self.commits = list,
            Err(err) => {
                self.commits.clear();
                self.set_status(format!("Could not read the snapshot history: {err}"), true);
            }
        }
        self.commits_loaded_for = Some(key);
    }

    // ---- Background jobs --------------------------------------------------------------

    /// Runs `work` on a background thread with a copy of the configuration.
    /// Returns false (and does nothing) while another job is running.
    fn run_job<F>(&mut self, title: impl Into<String>, work: F) -> bool
    where
        F: FnOnce(&mut AppConfig, &JobCtx) -> Result<JobOutput> + Send + 'static,
    {
        if self.busy {
            return false;
        }
        let title = title.into();
        let mut cfg = self.config.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = JobCtx {
            tx: tx.clone(),
            db: self.db.clone(),
            running: self.running.clone(),
        };
        self.busy = true;
        self.busy_title = title.clone();
        self.rx = Some(rx);
        self.status_message = format!("{title}...");
        self.is_error_status = false;
        self.push_log(LogLevel::Info, "job", format!("{title}..."));

        std::thread::spawn(move || {
            let result = work(&mut cfg, &ctx);
            let _ = tx.send(WorkerEvent::Done {
                title,
                config: Some(Box::new(cfg)),
                result,
            });
        });
        true
    }

    /// Takes the fields a job may have changed, keeping settings the user may have edited
    /// in the meantime.
    fn merge_job_config(&mut self, job: AppConfig) {
        self.config.games = job.games;
        self.config.save_state_labels = job.save_state_labels;
        self.config.separate_folder_per_provider = job.separate_folder_per_provider;
        self.config.ignored_game_ids = job.ignored_game_ids;
        if self
            .selected_game_id
            .as_deref()
            .is_none_or(|sel| !self.config.games.iter().any(|g| g.id == sel))
        {
            self.selected_game_id = self.config.games.first().map(|g| g.id.clone());
        }
        self.persist_config();
        self.invalidate_game_caches();
        self.refresh_selected_game_files();
        // Health badges follow the new bookkeeping right away.
        for g in &self.config.games {
            if !g.is_placeholder() && !self.live_states.contains_key(&g.id) {
                self.live_states.insert(g.id.clone(), LiveState::default());
            }
        }
    }

    fn poll_jobs(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(WorkerEvent::Log {
                    level,
                    source,
                    message,
                }) => {
                    self.push_log(level, source, message.clone());
                    self.status_message = message;
                    self.is_error_status = level == LogLevel::Error;
                }
                Ok(WorkerEvent::Done {
                    title,
                    config,
                    result,
                }) => {
                    self.busy = false;
                    keep = false;
                    if let Some(cfg) = config {
                        self.merge_job_config(*cfg);
                    }
                    match result {
                        Ok(out) => {
                            self.handle_payload(out.payload);
                            if !out.message.is_empty() {
                                self.set_status(out.message, out.is_error);
                            }
                        }
                        Err(err) => {
                            // A failed database load must not hold back scanning.
                            self.db_checked = true;
                            self.set_status(format!("{title} failed: {err:#}"), true)
                        }
                    }
                    self.refresh_cloud_facts();
                    self.refresh_live_states();
                    ctx.request_repaint();
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(120));
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.busy = false;
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.rx = Some(rx);
        }
    }

    fn refresh_cloud_facts(&mut self) {
        let repo = &self.config.local_repo_path;
        self.unpushed = unpushed_commit_count(repo);
        self.cloud_needs_sign_in = !self.config.remote_configured()
            && repo.join(".git").exists()
            && crate::git_ops::has_origin_remote(repo);
    }

    fn refresh_live_states(&mut self) {
        // Cheap (stat only); keeps badges honest right after a job without waiting for the watcher.
        self.live_states = self
            .config
            .games
            .iter()
            .filter(|g| !g.is_placeholder())
            .map(|g| (g.id.clone(), live_state(&self.config, g)))
            .collect();
    }

    fn handle_payload(&mut self, payload: JobPayload) {
        match payload {
            JobPayload::None | JobPayload::Refresh => {}
            JobPayload::Sync(outcome) => {
                for note in &outcome.notes {
                    self.push_log(LogLevel::Info, "sync", note.clone());
                }
                for w in outcome.warnings {
                    if !self
                        .warnings
                        .iter()
                        .any(|e| e.file_path == w.file_path && e.game_name == w.game_name)
                    {
                        self.warnings.push(w);
                    }
                }
                if !outcome.adopted_from_cloud.is_empty() {
                    self.scan_banner = Some(format!(
                        "{} game(s) from your other PC appeared. Install them here and their saves follow.",
                        outcome.adopted_from_cloud.len()
                    ));
                    self.pending.push(Pending::Scan);
                }
            }
            JobPayload::Scan(summary) => {
                for (id, det) in &summary.suggestions {
                    self.suggestions.insert(id.clone(), det.clone());
                }
                self.untracked = summary.untracked.clone();
                let mut parts = Vec::new();
                if !summary.added.is_empty() {
                    parts.push(format!("{} new game(s) found", summary.added.len()));
                }
                if !summary.located.is_empty() {
                    parts.push(format!("{} game(s) located", summary.located.len()));
                }
                if !self.suggestions.is_empty() {
                    parts.push(format!(
                        "{} game(s) have a more precise save location available",
                        self.suggestions.len()
                    ));
                }
                if !summary.needs_review.is_empty() {
                    parts.push(format!("{} need a look", summary.needs_review.len()));
                }
                if !self.untracked.is_empty() {
                    parts.push(format!("{} installed game(s) are not protected yet", self.untracked.len()));
                }
                self.scan_banner = if parts.is_empty() { None } else { Some(parts.join(" · ")) };
                // Newly located games get their first snapshot / cloud restore right away.
                let mut ids = summary.located.clone();
                ids.extend(summary.added.iter().cloned());
                let ids: Vec<String> = ids
                    .into_iter()
                    .filter(|id| {
                        self.config
                            .games
                            .iter()
                            .any(|g| &g.id == id && g.enabled && !g.is_placeholder())
                    })
                    .collect();
                if !ids.is_empty() && self.config.onboarding_done {
                    self.pending.push(Pending::Sync {
                        not_before: Instant::now(),
                        ids,
                        title: "Protecting newly found games".to_string(),
                    });
                }
            }
            JobPayload::Db(db) => {
                self.db_checked = true;
                if db.is_some() {
                    self.db = db;
                }
            }
            JobPayload::RepoEnsure(result, remote_url_opt) => match result {
                RepoEnsureResult::AlreadyExists { remote_url }
                | RepoEnsureResult::CreatedPrivate { remote_url } => {
                    let _ = init_or_open_local_repo(&self.config.local_repo_path, Some(&remote_url));
                }
                RepoEnsureResult::NeedsPatOrManual { .. } => {
                    if let Some(url) = remote_url_opt {
                        let _ = init_or_open_local_repo(&self.config.local_repo_path, Some(&url));
                    }
                }
            },
            JobPayload::TokenUser(username) => {
                match &mut self.config.auth {
                    AuthMethod::PersonalAccessToken {
                        github_username, ..
                    }
                    | AuthMethod::ExistingSshKey {
                        github_username, ..
                    }
                    | AuthMethod::NewSshKey {
                        github_username, ..
                    } => *github_username = username,
                }
                self.persist_config();
            }
            JobPayload::Inspect(boxed) => {
                let (inspection, commits_map) = *boxed;
                let first_game = inspection.games.first().cloned();
                self.import_state.ext_selected_game_id = first_game.as_ref().map(|g| g.id.clone());
                self.import_state.ext_selected_commit = None;
                if let Some(fg) = &first_game {
                    self.import_state.ext_target_provider = fg.provider;
                    self.import_state.ext_target_save_dir =
                        PathBuf::from(self.import_state.reinstate_fallback_dir.trim())
                            .join(fg.provider.slug())
                            .join(&fg.game_slug)
                            .display()
                            .to_string();
                }
                self.import_state.ext_inspection = Some(inspection);
                self.import_state.ext_game_commits = commits_map;
            }
            JobPayload::Preview {
                game_id,
                commit,
                preview,
                mod_diff,
            } => {
                if let Some(dialog) = self.restore_dialog.as_mut()
                    && dialog.game_id == game_id
                    && dialog.commit.commit_hash == commit
                {
                    dialog.preview = Some(*preview);
                    dialog.mod_diff = mod_diff;
                }
            }
            JobPayload::Learned { game_id, found } => {
                if let Some(c) = found {
                    self.push_log(
                        LogLevel::Success,
                        "detect",
                        format!("Learned save location for {game_id}: {} ({})", c.path.display(), c.reason),
                    );
                    self.alternatives.remove(&game_id);
                    self.pending.push(Pending::Sync {
                        not_before: Instant::now(),
                        ids: vec![game_id],
                        title: "First snapshot".to_string(),
                    });
                }
            }
        }
    }

    fn poll_watcher(&mut self) {
        let Some(rx) = self.watch_rx.take() else {
            return;
        };
        while let Ok(event) = rx.try_recv() {
            match event {
                WatchEvent::LiveStates(states) => self.live_states = states,
                WatchEvent::Session(SessionEvent::Started { game_id }) => {
                    if let Some(name) = self.config.games.iter().find(|g| g.id == game_id).map(|g| g.name.clone()) {
                        self.push_log(LogLevel::Info, "watch", format!("{name} started."));
                    }
                    self.running.insert(game_id);
                }
                WatchEvent::Session(SessionEvent::Ended { game_id, started_at }) => {
                    self.running.remove(&game_id);
                    let Some(game) = self.config.games.iter_mut().find(|g| g.id == game_id) else {
                        continue;
                    };
                    game.sync_state.last_played_at = Some(Utc::now());
                    let game = game.clone();
                    self.push_log(LogLevel::Info, "watch", format!("{} closed.", game.name));
                    let unsure = game.is_placeholder()
                        || game.detection.as_ref().is_some_and(|d| {
                            d.source != DetectionSource::Manual && d.confidence < AUTO_ENABLE_CONFIDENCE
                        });
                    let not_before = Instant::now() + POST_SESSION_DELAY;
                    if unsure {
                        self.pending.push(Pending::Learn {
                            not_before,
                            game_id,
                            since: started_at,
                        });
                    } else if game.enabled && game.backup_on_exit.unwrap_or(self.config.backup_on_game_exit) {
                        self.pending.push(Pending::Sync {
                            not_before,
                            ids: vec![game_id],
                            title: format!("Snapshot after playing {}", game.name),
                        });
                    }
                }
            }
        }
        self.watch_rx = Some(rx);
    }

    fn pump_pending(&mut self, ctx: &egui::Context) {
        if self.pending.is_empty() {
            return;
        }
        if self.busy {
            return;
        }
        let now = Instant::now();
        let ready = self.pending.iter().position(|p| match p {
            Pending::Sync { not_before, .. } | Pending::Learn { not_before, .. } => *not_before <= now,
            Pending::Scan => self.db_checked,
        });
        match ready.map(|i| self.pending.remove(i)) {
            Some(Pending::Sync { ids, title, .. }) => self.start_sync(ids, None, title, true),
            Some(Pending::Learn { game_id, since, .. }) => self.start_learn(game_id, since),
            Some(Pending::Scan) => self.start_scan(true),
            None => ctx.request_repaint_after(Duration::from_millis(500)),
        }
    }

    fn check_periodic_sync(&mut self) {
        self.check_external_config_reload();
        if self.busy || self.last_periodic_check.elapsed() < Duration::from_secs(10) {
            return;
        }
        self.last_periodic_check = Instant::now();
        // Schedules apply to local snapshots too; the cloud is only used when enabled.
        let mut probe = self.config.clone();
        probe.sync_enabled = true;
        let due: Vec<String> = games_due_periodic(&probe, Utc::now())
            .into_iter()
            .filter(|id| !self.running.contains(id))
            .collect();
        if !due.is_empty() {
            self.start_sync(due, None, "Scheduled check".to_string(), true);
        }
    }

    /// Snapshots `ids`. `automatic` passes leave running games for later.
    fn start_sync(&mut self, ids: Vec<String>, label: Option<String>, title: String, automatic: bool) {
        if ids.is_empty() {
            return;
        }
        let use_remote = self.cloud_enabled();
        self.run_job(title.clone(), move |cfg, ctx| {
            let progress = |level: LogLevel, msg: &str| ctx.log(level, "sync", msg);
            let outcome = sync_games(
                cfg,
                &ids,
                SyncOptions {
                    label: label.as_deref(),
                    use_remote,
                    progress: Some(&progress),
                    running: if automatic { ctx.running.clone() } else { BTreeSet::new() },
                    ..Default::default()
                },
            )?;
            let mut parts = Vec::new();
            if !outcome.uploaded.is_empty() {
                parts.push(format!("{} saved", outcome.uploaded.len()));
            }
            if !outcome.downloaded.is_empty() {
                parts.push(format!("{} updated from the cloud", outcome.downloaded.len()));
            }
            if !outcome.unchanged.is_empty() {
                parts.push(format!("{} already up to date", outcome.unchanged.len()));
            }
            if !outcome.conflicts.is_empty() {
                parts.push(format!("{} need your choice", outcome.conflicts.len()));
            }
            if parts.is_empty() {
                parts.push("nothing to do".to_string());
            }
            let cloud = if outcome.pushed_to_remote {
                " Cloud copy is up to date.".to_string()
            } else if let Some(err) = &outcome.push_error {
                format!(" Cloud: {err}")
            } else {
                String::new()
            };
            Ok(JobOutput {
                message: format!("{title}: {}.{cloud}", parts.join(", ")),
                is_error: outcome.push_error.is_some() || !outcome.conflicts.is_empty(),
                payload: JobPayload::Sync(outcome),
            })
        });
    }

    fn start_scan(&mut self, quiet: bool) {
        self.run_job("Looking for games", move |cfg, ctx| {
            let scanned = scan_installed_games(
                &cfg.provider_libraries,
                ctx.db.as_deref(),
                cfg.include_config_roots,
            );
            let summary = merge_scan_results(cfg, scanned);
            let message = if quiet && summary.added.is_empty() && summary.located.is_empty() {
                String::new()
            } else {
                format!(
                    "Found {} installed game(s): {} new, {} tracked in total.",
                    summary.installed,
                    summary.added.len(),
                    cfg.games.len()
                )
            };
            Ok(JobOutput::with(message, JobPayload::Scan(summary)))
        });
    }

    fn scan_libraries_into_config(&mut self) {
        self.start_scan(false);
    }

    fn start_db_load(&mut self) {
        let allow_download = self.config.auto_update_detection_db;
        self.run_job("Loading save location database", move |_, ctx| {
            let dir = default_db_dir();
            let mut note = String::new();
            if allow_download {
                match update_database(&dir, false) {
                    Ok(UpdateOutcome::Updated { games }) => {
                        ctx.log(LogLevel::Success, "detect", format!("Save location database updated ({games} games)."));
                    }
                    Ok(_) => {}
                    Err(err) => note = format!("Save location database could not be refreshed: {err}"),
                }
            }
            let db = SaveDb::load_from_dir(&dir).ok().flatten().map(Arc::new);
            Ok(JobOutput {
                is_error: db.is_none() && !note.is_empty(),
                message: note,
                payload: JobPayload::Db(db),
            })
        });
    }

    fn start_db_update(&mut self) {
        self.run_job("Updating save location database", move |_, _| {
            let dir = default_db_dir();
            let outcome = update_database(&dir, true)?;
            let db = SaveDb::load_from_dir(&dir)?.map(Arc::new);
            let message = match outcome {
                UpdateOutcome::Updated { games } => format!("Save location database updated: {games} games."),
                _ => "Save location database is already current.".to_string(),
            };
            Ok(JobOutput::with(message, JobPayload::Db(db)))
        });
    }

    fn start_learn(&mut self, game_id: String, since: SystemTime) {
        self.run_job("Learning where the game saves", move |cfg, _| {
            let Some(game) = cfg.games.iter_mut().find(|g| g.id == game_id) else {
                return Ok(JobOutput::ok(""));
            };
            let pctx = PathContext::for_game(game);
            let found = learn_from_session(game, &pctx, since);
            let message = match &found {
                Some(c) => {
                    game.save_dir = c.path.clone();
                    game.save_dir_template = tokenize_path(&c.path, &pctx);
                    game.sync_selection = SyncSelectionMode::EntireFolder;
                    game.detection = Some(DetectionInfo {
                        source: DetectionSource::PlaySession,
                        confidence: c.score,
                        reason: format!("Learned while you played: {}.", c.reason),
                        database_title: None,
                    });
                    game.enabled = true;
                    format!("Found where {} keeps its saves. It is now protected.", game.name)
                }
                None => String::new(),
            };
            Ok(JobOutput::with(message, JobPayload::Learned { game_id, found }))
        });
    }

    fn start_restore_preview(&mut self, game_id: String, commit: CommitInfo) {
        let hash = commit.commit_hash.clone();
        self.restore_dialog = Some(RestoreDialog {
            game_id: game_id.clone(),
            commit,
            preview: None,
            mod_diff: ModDiff::default(),
        });
        self.run_job("Comparing snapshot", move |cfg, _| {
            let preview = preview_restore(cfg, &game_id, &hash)?;
            let game = cfg.games.iter().find(|g| g.id == game_id);
            let current = game.and_then(|g| scan_mod_fingerprint(&g.mod_sources));
            let mod_diff = if preview.snapshot_mods.is_some() || current.is_some() {
                diff_fingerprints(preview.snapshot_mods.as_ref(), current.as_ref())
            } else {
                ModDiff::default()
            };
            Ok(JobOutput::with(
                "",
                JobPayload::Preview {
                    game_id,
                    commit: hash,
                    preview: Box::new(preview),
                    mod_diff,
                },
            ))
        });
    }

    fn start_rollback(&mut self, game_id: String, commit_hash: String) {
        let short = commit_hash[..commit_hash.len().min(7)].to_string();
        let use_remote = self.cloud_enabled();
        self.run_job(format!("Restoring snapshot {short}"), move |cfg, _| {
            let count = rollback_game_to_commit(cfg, &game_id, &commit_hash, use_remote)?;
            Ok(JobOutput::with(
                format!("Restored {count} file(s). Your previous files are kept under Safety backups."),
                JobPayload::Refresh,
            ))
        });
    }

    fn start_resolve_conflict(&mut self, game_id: String, use_cloud: bool) {
        let use_remote = self.cloud_enabled();
        self.run_job("Resolving save conflict", move |cfg, _| {
            if use_cloud {
                let n = resolve_conflict_use_cloud(cfg, &game_id, use_remote)?;
                Ok(JobOutput::with(
                    format!("Now using the cloud saves ({n} file(s)). This PC's version is kept under Safety backups."),
                    JobPayload::Refresh,
                ))
            } else {
                resolve_conflict_keep_local(cfg, &game_id, use_remote)?;
                Ok(JobOutput::with(
                    "Kept this PC's saves. The other version stays in the timeline.",
                    JobPayload::Refresh,
                ))
            }
        });
    }

    fn start_switch_profile(&mut self, game_id: String, profile_id: String) {
        if self.running.contains(&game_id) {
            self.set_status("Close the game before switching profiles.", true);
            return;
        }
        let use_remote = self.cloud_enabled();
        self.run_job("Switching profile", move |cfg, _| {
            let n = switch_profile(cfg, &game_id, &profile_id, use_remote)?;
            let name = cfg
                .games
                .iter()
                .find(|g| g.id == game_id)
                .map(|g| g.profile_name(&profile_id))
                .unwrap_or_default();
            Ok(JobOutput::with(
                format!("Now playing as \"{name}\" ({n} file(s) put in place)."),
                JobPayload::Refresh,
            ))
        });
    }

    fn start_create_profile(&mut self, game_id: String) {
        let draft = std::mem::take(&mut self.new_profile);
        let start = match (&draft.from_commit, draft.copy_current) {
            (Some((profile, commit)), _) => ProfileStart::FromSnapshot {
                profile: profile.clone(),
                commit: commit.clone(),
            },
            (None, true) => ProfileStart::CopyOfCurrent,
            (None, false) => ProfileStart::Empty,
        };
        if draft.switch_after && self.running.contains(&game_id) {
            self.set_status("Close the game before switching profiles.", true);
            self.new_profile = draft;
            return;
        }
        let use_remote = self.cloud_enabled();
        self.run_job("Creating profile", move |cfg, _| {
            let id = create_profile(cfg, &game_id, &draft.name, &draft.note, start)?;
            if draft.switch_after {
                switch_profile(cfg, &game_id, &id, use_remote)?;
            }
            Ok(JobOutput::with(
                format!("Profile \"{}\" created.", draft.name.trim()),
                JobPayload::Refresh,
            ))
        });
    }

    fn start_delete_profile(&mut self, game_id: String, profile_id: String) {
        self.run_job("Deleting profile", move |cfg, _| {
            delete_profile(cfg, &game_id, &profile_id)?;
            Ok(JobOutput::with("Profile deleted.", JobPayload::Refresh))
        });
    }

    fn start_undo_from_safety(&mut self, game_id: String, zip: PathBuf) {
        if self.running.contains(&game_id) {
            self.set_status("Close the game first.", true);
            return;
        }
        self.run_job("Putting safety backup back", move |cfg, _| {
            let game = cfg
                .games
                .iter()
                .find(|g| g.id == game_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Game not found"))?;
            create_safety_backup(cfg, &game, "before undo")?;
            let n = restore_roots_zip(&zip, &game.active_roots())?;
            Ok(JobOutput::with(
                format!("Put back {n} file(s) from the safety backup."),
                JobPayload::Refresh,
            ))
        });
    }

    fn start_export_zip(&mut self, game_id: String, dst: PathBuf) {
        self.run_job("Exporting saves", move |cfg, _| {
            let game = cfg
                .games
                .iter()
                .find(|g| g.id == game_id)
                .ok_or_else(|| anyhow::anyhow!("Game not found"))?;
            let info = BackupInfo {
                game_id: game.id.clone(),
                game_name: game.name.clone(),
                profile: game.active_profile.clone(),
                created_at: Utc::now(),
                reason: "export".to_string(),
                machine: cfg.machine_name.clone(),
                roots: Vec::new(),
            };
            let n = write_roots_zip(&dst, &game.active_roots(), &game.exclude_globs, &cfg.global_exclude_globs, &info)?;
            Ok(JobOutput::ok(format!("Exported {n} file(s) to {}.", dst.display())))
        });
    }

    fn confirm_remove_game(&mut self, game_id: String, remove_from_git: bool) {
        self.pending_remove_game_id = None;
        let push_remote = self.cloud_enabled();
        self.run_job("Removing game", move |cfg, _| {
            let outcome = remove_game_entry(cfg, &game_id, remove_from_git, push_remote)?;
            if !cfg.ignored_game_ids.contains(&game_id) {
                cfg.ignored_game_ids.push(game_id.clone());
            }
            let message = if outcome.removed_from_git {
                format!(
                    "Removed '{}' and its snapshots folder '{}'. Your save files on disk were not touched.",
                    outcome.game_name,
                    outcome.repo_subpath.display()
                )
            } else {
                format!(
                    "Removed '{}' from the list (its snapshots are kept).",
                    outcome.game_name
                )
            };
            Ok(JobOutput {
                is_error: outcome.push_error.is_some(),
                message,
                payload: JobPayload::Refresh,
            })
        });
    }

    fn start_pull_from_remote(&mut self) {
        self.run_job("Checking the cloud", move |cfg, _| {
            let added = pull_cloud_catalog(cfg)?;
            Ok(JobOutput::with(
                if added.is_empty() {
                    "Cloud checked. No games from other PCs are missing here.".to_string()
                } else {
                    format!("{} game(s) saved on another PC were added to your list.", added.len())
                },
                JobPayload::Refresh,
            ))
        });
    }

    fn start_reinstate(&mut self, source: Option<String>, fallback_dir: PathBuf) {
        self.run_job("Restoring everything from the repository", move |cfg, _| {
            let src_opt = source.as_deref().map(str::trim).filter(|s| !s.is_empty());
            let out = reinstate_full_structure(cfg, src_opt, &fallback_dir)?;
            Ok(JobOutput::with(
                format!(
                    "Re-instated {} game(s), restored {} file(s), and imported {} save label(s).",
                    out.games_reinstated, out.files_restored, out.labels_restored
                ),
                JobPayload::Refresh,
            ))
        });
    }

    fn start_inspect_external(&mut self, source: String) {
        self.run_job("Inspecting shared repository", move |cfg, _| {
            let cache_dir = crate::config::default_data_dir().join("external_repo_cache");
            let repo_path = prepare_external_repo(&source, &cache_dir, &cfg.auth)?;
            let inspection = inspect_repository(&repo_path)?;
            let mut commits_map = HashMap::new();
            for g in &inspection.games {
                let mut list = list_commits_for_path(&repo_path, Path::new(&g.repo_subpath)).unwrap_or_default();
                for c in &mut list {
                    if c.label.is_none() {
                        c.label = inspection
                            .labels
                            .iter()
                            .find(|l| {
                                l.game_id == g.id
                                    && (l.commit_hash == c.commit_hash || c.commit_hash.starts_with(&l.commit_hash))
                            })
                            .map(|l| l.label.clone());
                    }
                }
                commits_map.insert(g.id.clone(), list);
            }
            let count = inspection.games.len();
            Ok(JobOutput::with(
                format!("Inspected shared repository: found {count} game(s)."),
                JobPayload::Inspect(Box::new((inspection, commits_map))),
            ))
        });
    }

    fn start_external_import(&mut self, ext_repo_path: PathBuf, req: ExternalImportRequest) {
        let title = format!("Import {} from shared repo", req.source_game.name);
        let use_remote = self.cloud_enabled();
        self.run_job(title.clone(), move |cfg, _| {
            let out = import_game_from_external_repo(cfg, &ext_repo_path, &req, use_remote)?;
            Ok(JobOutput::with(format!("{title}: done."), JobPayload::Sync(out)))
        });
    }

    fn start_external_import_all(&mut self, ext_repo_path: PathBuf, inspection: RepoInspection) {
        let fallback_root = PathBuf::from(self.import_state.reinstate_fallback_dir.trim());
        let use_remote = self.cloud_enabled();
        self.run_job("Importing all games from shared repository", move |cfg, _| {
            let mut imported = 0usize;
            for entry in inspection.games {
                let target_dir = cfg
                    .games
                    .iter()
                    .find(|g| g.id == entry.id && !g.is_placeholder())
                    .map(|g| g.save_dir.clone())
                    .unwrap_or_else(|| fallback_root.join(entry.provider.slug()).join(&entry.game_slug));
                let req = ExternalImportRequest {
                    target_provider: entry.provider,
                    target_save_dir: target_dir,
                    custom_label: entry.active_save_label.clone(),
                    source_game: entry,
                    commit_hash: None,
                };
                import_game_from_external_repo(cfg, &ext_repo_path, &req, use_remote)?;
                imported += 1;
            }
            Ok(JobOutput::with(
                format!("Imported {imported} game(s) from the shared repository."),
                JobPayload::Refresh,
            ))
        });
    }

    fn start_online_import(&mut self, req: OnlineSaveImportRequest) {
        let title = format!("Import save for {}", req.game_name);
        let use_remote = self.cloud_enabled();
        self.run_job(title.clone(), move |cfg, _| {
            let out = import_online_save(cfg, &req, use_remote)?;
            Ok(JobOutput::with(format!("{title}: done."), JobPayload::Sync(out)))
        });
    }

    fn start_verify_token(&mut self, token: String) {
        self.run_job("Verifying GitHub token", move |_, _| {
            let user = verify_github_token(&token)?;
            Ok(JobOutput::with(
                format!("Token verified for GitHub user '{user}'."),
                JobPayload::TokenUser(user),
            ))
        });
    }

    fn start_upload_ssh_key(&mut self, token: String, pub_key: String) {
        self.run_job("Uploading SSH key to GitHub", move |_, _| {
            upload_ssh_public_key_to_github(&token, "SaveVault", &pub_key)?;
            Ok(JobOutput::ok("Uploaded SSH public key to GitHub."))
        });
    }

    fn start_ensure_repo(&mut self) {
        self.settings_highlight = None;
        if let Some(custom) = self.config.custom_remote().map(str::to_string) {
            self.run_job("Testing the custom remote", move |cfg, _| {
                init_or_open_local_repo(&cfg.local_repo_path, Some(&custom))?;
                crate::git_ops::fetch_remote(&cfg.local_repo_path, &cfg.auth)?;
                Ok(JobOutput::ok(format!("Connected to {custom}.")))
            });
            return;
        }
        let owner = self.config.auth.github_username().to_string();
        let repo_name = self.config.repo_name.clone();
        self.run_job(format!("Checking GitHub repository '{repo_name}'"), move |cfg, _| {
            let remote_url_opt = remote_url_for_auth(&owner, &repo_name, &cfg.auth);
            let res = ensure_remote_repo_exists(&owner, &repo_name, cfg.auth.pat_token(), &cfg.auth)?;
            let (message, is_error) = match &res {
                RepoEnsureResult::AlreadyExists { remote_url } => (format!("Connected. Repository found: {remote_url}"), false),
                RepoEnsureResult::CreatedPrivate { remote_url } => (format!("Created private GitHub repository: {remote_url}"), false),
                RepoEnsureResult::NeedsPatOrManual { message, .. } => (message.clone(), true),
            };
            Ok(JobOutput {
                message,
                is_error,
                payload: JobPayload::RepoEnsure(res, remote_url_opt),
            })
        });
    }

    // ---- Chrome ------------------------------------------------------------------------

    fn render_top_bar(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        glass_frame_3(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                paint_logo_mark(ui, pal);

                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(egui::RichText::new(APP_NAME).strong().size(16.0).color(pal.text_1));
                    ui.label(egui::RichText::new(APP_SUBTITLE).small().monospace().color(pal.text_3));
                });

                ui.separator();

                let attention = self
                    .config
                    .games
                    .iter()
                    .filter(|g| matches!(self.health_of(g), GameHealth::Conflict | GameHealth::NeedsReview))
                    .count();
                let games_label = if attention > 0 {
                    format!("Games ({attention} need you)")
                } else {
                    "Games".to_string()
                };
                for (tab, label) in [
                    (AppTab::Games, games_label.as_str()),
                    (AppTab::Import, "Import & Restore"),
                    (AppTab::SaveEditor, "Save Editor"),
                    (AppTab::Logs, "Activity"),
                    (AppTab::Settings, "Settings"),
                ] {
                    let active = self.active_tab == tab;
                    let text = egui::RichText::new(label)
                        .strong()
                        .color(if active { pal.accent_text } else { pal.text_2 });
                    let btn = egui::Button::new(text)
                        .fill(if active { pal.accent_soft } else { egui::Color32::TRANSPARENT })
                        .stroke(egui::Stroke::new(
                            1.0,
                            if active { pal.accent_border } else { egui::Color32::TRANSPARENT },
                        ))
                        .corner_radius(egui::CornerRadius::same(4));
                    if ui.add(btn).clicked() {
                        self.active_tab = tab;
                        if tab != AppTab::Settings {
                            self.settings_highlight = None;
                            self.scroll_to_settings_highlight = false;
                        }
                    }
                }

                let mut toggle_overlay_clicked = false;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let protectable: Vec<String> = self
                        .config
                        .games
                        .iter()
                        .filter(|g| g.enabled && !g.is_placeholder())
                        .map(|g| g.id.clone())
                        .collect();
                    if accent_button(ui, pal, !self.busy && !protectable.is_empty(), "Back up everything")
                        .on_hover_text("Snapshot every tracked game now (only what changed is stored)")
                        .clicked()
                    {
                        self.start_sync(protectable, None, "Back up everything".to_string(), false);
                    }
                    if !self.overlay_mode
                        && outline_orange_button(ui, pal, true, "Overlay")
                            .on_hover_text(format!(
                                "Toggle the in-game screen overlay ({})",
                                self.config.overlay_hotkey
                            ))
                            .clicked()
                    {
                        toggle_overlay_clicked = true;
                    }
                    if self.busy {
                        ui.label(egui::RichText::new(&self.busy_title).small().color(pal.accent_text));
                        ui.spinner();
                    }
                });
                if toggle_overlay_clicked && let Err(err) = crate::overlay::toggle_overlay_command() {
                    self.set_status(format!("Failed to toggle overlay: {err}"), true);
                }
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.set_width(ui.available_width());
                let (cloud_text, cloud_on) = if self.cloud_needs_sign_in {
                    ("CLOUD: SIGN-IN NEEDED".to_string(), false)
                } else if !self.config.remote_configured() {
                    ("ON THIS PC ONLY".to_string(), false)
                } else if !self.config.sync_enabled {
                    ("CLOUD: PAUSED".to_string(), false)
                } else if self.unpushed > 0 {
                    (format!("CLOUD: {} TO UPLOAD", self.unpushed), true)
                } else {
                    ("CLOUD: IN SYNC".to_string(), true)
                };
                render_badge(ui, pal, &cloud_text, cloud_on);
                if self.cloud_needs_sign_in
                    && ui
                        .small_button("Fix in Settings")
                        .on_hover_text("Your snapshots have a cloud repository, but SaveVault has no working sign-in for it. Snapshots are kept on this PC until that is fixed.")
                        .clicked()
                {
                    self.active_tab = AppTab::Settings;
                    self.settings_highlight = Some(SettingsHighlight::CloudSignIn);
                    self.scroll_to_settings_highlight = true;
                }
                let protected = self
                    .config
                    .games
                    .iter()
                    .filter(|g| matches!(self.health_of(g), GameHealth::Protected))
                    .count();
                render_badge(
                    ui,
                    pal,
                    &format!("{protected}/{} PROTECTED", self.config.games.iter().filter(|g| g.enabled).count()),
                    false,
                );
                if !self.running.is_empty() {
                    render_badge(ui, pal, &format!("{} PLAYING", self.running.len()), true);
                }
                let status_color = if self.is_error_status { pal.error_fg } else { pal.ok_fg };
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&self.status_message).small().color(status_color))
                            .truncate(),
                    )
                    .on_hover_text(&self.status_message);
                });
            });
        });
    }

    fn render_overlay_top_bar(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        let mut quick_backup: Option<(String, String)> = None;
        let mut next_selected: Option<String> = None;
        let mut toggle_compact = false;
        let mut close_overlay = false;

        glass_frame_3(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let brand = ui.horizontal(|ui| {
                    paint_logo_mark(ui, pal);
                    ui.label(egui::RichText::new(APP_NAME).strong().size(15.0).color(pal.text_1));
                    render_badge(ui, pal, "OVERLAY", true);
                    render_badge(ui, pal, &self.config.overlay_hotkey, false);
                });
                let brand_drag = ui.interact(
                    brand.response.rect,
                    ui.scope_id().with("overlay_brand_drag"),
                    egui::Sense::drag(),
                );
                if brand_drag.drag_started_by(egui::PointerButton::Primary) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                ui.separator();

                let selected_label = match self.selected_game() {
                    Some(g) if self.running.contains(&g.id) => format!("[PLAYING] {}", g.name),
                    Some(g) => g.name.clone(),
                    None => "Select a game...".to_string(),
                };
                let mut sorted_games: Vec<(String, String, &'static str, bool)> = self
                    .config
                    .games
                    .iter()
                    .map(|g| {
                        (
                            g.id.clone(),
                            g.name.clone(),
                            g.provider.display_name(),
                            self.running.contains(&g.id),
                        )
                    })
                    .collect();
                sorted_games.sort_by(|a, b| b.3.cmp(&a.3).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));

                egui::ComboBox::from_id_salt("overlay_game_selector")
                    .selected_text(selected_label)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (id, name, provider, is_running) in sorted_games {
                            let label = if is_running {
                                format!("[PLAYING] {name}")
                            } else {
                                format!("{name} ({provider})")
                            };
                            let active = self.selected_game_id.as_deref() == Some(id.as_str());
                            if ui.selectable_label(active, label).clicked() {
                                next_selected = Some(id);
                            }
                        }
                    });

                if let Some(game) = self.selected_game()
                    && !game.is_placeholder()
                    && accent_button(ui, pal, !self.busy, "Back up now").clicked()
                {
                    quick_backup = Some((game.id.clone(), game.name.clone()));
                }

                let right_reserved = 215.0;
                let drag_width = (ui.available_width() - right_reserved).max(16.0);
                let spacer = ui.allocate_response(egui::vec2(drag_width, 26.0), egui::Sense::drag());
                if spacer.drag_started_by(egui::PointerButton::Primary) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if outline_orange_button(ui, pal, true, "Hide (Esc)").clicked() {
                        close_overlay = true;
                    }
                    let view_label = if self.overlay_compact {
                        "Full App View"
                    } else {
                        "Compact HUD"
                    };
                    if ui.button(view_label).clicked() {
                        toggle_compact = true;
                    }
                });
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.set_width(ui.available_width());
                if !self.running.is_empty() {
                    render_badge(ui, pal, &format!("{} PLAYING", self.running.len()), true);
                }
                if self.busy {
                    ui.spinner();
                    ui.label(egui::RichText::new(&self.busy_title).small().color(pal.accent_text));
                }
                let status_color = if self.is_error_status { pal.error_fg } else { pal.ok_fg };
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&self.status_message).small().color(status_color))
                            .truncate(),
                    )
                    .on_hover_text(&self.status_message);
                });
            });
        });

        if let Some(id) = next_selected {
            self.select_game(id);
        }
        if let Some((id, name)) = quick_backup {
            self.start_sync(vec![id], None, format!("Back up {name}"), false);
        }
        if toggle_compact {
            self.overlay_compact = !self.overlay_compact;
        }
        if close_overlay {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn render_overlay_compact_body(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        self.render_warnings_banner(ui, pal);
        let avail = ui.available_size();
        glass_frame_1(pal).show(ui, |ui| {
            ui.set_min_size(egui::vec2((avail.x - 34.0).max(200.0), (avail.y - 34.0).max(160.0)));
            if self.selected_game().is_some() {
                self.render_game_detail(ui, pal);
            } else {
                ui.heading("No games tracked yet");
                ui.label(
                    egui::RichText::new("Scan for installed games or open the full view to add a game.")
                        .color(pal.text_2),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if accent_button(ui, pal, !self.busy, "Find games now").clicked() {
                        self.start_scan(false);
                    }
                    if ui.button("Full App View").clicked() {
                        self.overlay_compact = false;
                    }
                });
            }
        });
    }

    fn render_warnings_banner(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        if self.warnings.is_empty() {
            return;
        }
        egui::Frame::new()
            .fill(pal.accent_soft)
            .stroke(egui::Stroke::new(1.0, pal.warn_fg))
            .corner_radius(egui::CornerRadius::same(5))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.colored_label(
                        pal.warn_fg,
                        format!("Very large save files ({})", self.warnings.len()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Dismiss").clicked() {
                            self.warnings.clear();
                        }
                        if ui.button("Splitting settings").clicked() {
                            self.active_tab = AppTab::Settings;
                        }
                    });
                });
                for w in &self.warnings {
                    ui.label(
                        egui::RichText::new(format!(
                            "- [{} / {}] {}",
                            w.provider.display_name(),
                            w.game_name,
                            w.message
                        ))
                        .small(),
                    );
                }
            });
        ui.add_space(6.0);
    }

    fn render_remove_game_prompt(&mut self, ctx: &egui::Context, pal: &JizihubPalette) {
        let Some(target_id) = self.pending_remove_game_id.clone() else {
            return;
        };
        let Some(game) = self.config.games.iter().find(|g| g.id == target_id).cloned() else {
            self.pending_remove_game_id = None;
            return;
        };
        let subpath = repo_subpath_for_game(&game, self.config.separate_folder_per_provider);

        let mut action: Option<Option<bool>> = None;
        egui::Window::new("Remove Game")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.strong(format!("Stop tracking {}?", game.name));
                ui.add_space(6.0);
                ui.label("Your save files on disk are never deleted by this.");
                ui.label(
                    egui::RichText::new(format!("Snapshots folder: {}", subpath.display()))
                        .monospace()
                        .small()
                        .color(pal.text_3),
                );
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    if outline_orange_button(ui, pal, !self.busy, "Stop tracking, keep snapshots").clicked() {
                        action = Some(Some(false));
                    }
                    if accent_button(ui, pal, !self.busy, "Stop tracking and delete snapshots").clicked() {
                        action = Some(Some(true));
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(None);
                    }
                });
            });

        match action {
            Some(Some(remove_from_git)) => self.confirm_remove_game(target_id, remove_from_git),
            Some(None) => self.pending_remove_game_id = None,
            None => {}
        }
    }

    fn render_restore_dialog(&mut self, ctx: &egui::Context, pal: &JizihubPalette) {
        let Some(dialog) = self.restore_dialog.as_ref() else {
            return;
        };
        let game_name = self
            .config
            .games
            .iter()
            .find(|g| g.id == dialog.game_id)
            .map(|g| g.name.clone())
            .unwrap_or_default();
        let running = self.running.contains(&dialog.game_id);
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Restore snapshot")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.strong(format!("Restore {game_name} to this point?"));
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} ({})",
                            dialog.commit.timestamp,
                            relative_time(dialog.commit.unix_time)
                        ))
                        .color(pal.text_2),
                    );
                    if let Some(lbl) = &dialog.commit.label {
                        render_badge(ui, pal, lbl, true);
                    }
                    if let Some(m) = &dialog.commit.machine {
                        render_badge(ui, pal, &format!("from {m}"), false);
                    }
                });
                ui.add_space(6.0);
                match &dialog.preview {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Comparing with your current saves...");
                        });
                    }
                    Some(p) => {
                        let nothing = p.will_add.is_empty() && p.will_replace.is_empty() && p.will_remove.is_empty();
                        if nothing {
                            ui.colored_label(pal.ok_fg, "Your current saves already match this snapshot.");
                        } else {
                            ui.label(format!(
                                "{} file(s) will be replaced, {} brought back, {} removed.",
                                p.will_replace.len(),
                                p.will_add.len(),
                                p.will_remove.len()
                            ));
                            egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                                for (sign, list, color) in [
                                    ("~", &p.will_replace, pal.text_2),
                                    ("+", &p.will_add, pal.ok_fg),
                                    ("-", &p.will_remove, pal.warn_fg),
                                ] {
                                    for f in list.iter().take(40) {
                                        ui.label(egui::RichText::new(format!("{sign} {f}")).monospace().small().color(color));
                                    }
                                }
                            });
                        }
                        if !dialog.mod_diff.is_empty() {
                            ui.add_space(4.0);
                            egui::Frame::new()
                                .fill(pal.accent_soft)
                                .stroke(egui::Stroke::new(1.0, pal.warn_fg))
                                .corner_radius(egui::CornerRadius::same(4))
                                .inner_margin(egui::Margin::same(8))
                                .show(ui, |ui| {
                                    ui.colored_label(pal.warn_fg, "This snapshot was made with a different mod setup");
                                    ui.small(dialog.mod_diff.summary());
                                    egui::ScrollArea::vertical().id_salt("mod_diff").max_height(90.0).show(ui, |ui| {
                                        for m in dialog.mod_diff.missing_now.iter().take(30) {
                                            ui.label(egui::RichText::new(format!("missing now: {m}")).small().monospace());
                                        }
                                        for m in dialog.mod_diff.added_since.iter().take(30) {
                                            ui.label(egui::RichText::new(format!("added since: {m}")).small().monospace().color(pal.text_3));
                                        }
                                    });
                                });
                        }
                    }
                }
                ui.add_space(6.0);
                ui.small(
                    egui::RichText::new("Your current files are zipped into Safety backups first, so this can be undone.")
                        .color(pal.text_3),
                );
                if running {
                    ui.colored_label(pal.warn_fg, "The game is running. Close it before restoring.");
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if accent_button(ui, pal, !self.busy && !running && dialog.preview.is_some(), "Restore").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if confirm {
            if let Some(d) = self.restore_dialog.take() {
                self.start_rollback(d.game_id, d.commit.commit_hash);
            }
        } else if cancel {
            self.restore_dialog = None;
        }
    }
}

impl SaveGameManagerApp {
    fn service_capture(&mut self, ctx: &egui::Context) {
        let Some(capture) = self.capture.as_mut() else {
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(200));
        if capture.done.load(std::sync::atomic::Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // Wait for startup jobs to settle so the capture shows real content.
        let settled = !self.busy && self.pending.is_empty() && capture.started.elapsed() > Duration::from_secs(4);
        if !capture.requested && (settled || capture.started.elapsed() > Duration::from_secs(40)) {
            capture.requested = true;
            let path = capture.path.clone();
            let done = capture.done.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                move |image: Arc<egui::ColorImage>| {
                    let mut data = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
                    for px in &image.pixels {
                        data.extend_from_slice(&[px.r(), px.g(), px.b()]);
                    }
                    let _ = fs::write(&path, data);
                    done.store(true, std::sync::atomic::Ordering::Relaxed);
                },
            )));
        }
    }
}

impl eframe::App for SaveGameManagerApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.overlay_mode {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            let pal = theme::palette(self.config.theme_mode);
            egui::Rgba::from(pal.bg_canvas).to_array()
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.applied_theme != Some(self.config.theme_mode) {
            theme::apply_jizihub_theme(&ctx, self.config.theme_mode);
            self.applied_theme = Some(self.config.theme_mode);
        }
        if self.first_frame {
            self.first_frame = false;
            if self.start_minimized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }

        self.poll_jobs(&ctx);
        self.poll_watcher();
        self.pump_pending(&ctx);
        self.check_periodic_sync();
        // Keep ticking so delayed work and the watcher are serviced while idle.
        ctx.request_repaint_after(Duration::from_secs(2));

        if self.overlay_mode {
            if self
                .overlay_ipc
                .as_ref()
                .is_some_and(|ipc| ipc.close_requested())
            {
                if !self.busy {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }
            }
            let capturing = self.kbm_rebind.is_some() || self.overlay_hotkey_capturing.is_some();
            if !capturing
                && !ctx.egui_wants_keyboard_input()
                && (ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    || crate::overlay::overlay_hotkey_pressed_in_egui(
                        &ctx,
                        &self.config.overlay_hotkey,
                    ))
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        let pal = theme::palette(self.config.theme_mode);

        egui::CentralPanel::no_frame().show(ui, |ui| {
            if self.overlay_mode {
                let opacity = self.config.overlay_opacity.clamp(0.55, 1.0);
                let bg_alpha = (opacity * 255.0).round() as u8;
                let outer_bg = egui::Color32::from_rgba_unmultiplied(
                    pal.bg_canvas.r(),
                    pal.bg_canvas.g(),
                    pal.bg_canvas.b(),
                    bg_alpha,
                );
                egui::Frame::new()
                    .fill(outer_bg)
                    .stroke(egui::Stroke::new(1.5, pal.accent_border))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        self.render_overlay_top_bar(ui, &pal);
                        ui.add_space(6.0);

                        if self.overlay_compact {
                            self.render_overlay_compact_body(ui, &pal);
                        } else {
                            self.render_top_bar(ui, &pal);
                            ui.add_space(8.0);
                            match self.active_tab {
                                AppTab::Games => self.render_games_tab(ui, &pal),
                                AppTab::SaveEditor => self.render_save_editor_tab(ui, &pal),
                                AppTab::Import => self.render_import_tab(ui, &pal),
                                AppTab::Logs => self.render_logs_tab(ui, &pal),
                                AppTab::Settings => self.render_settings_tab(ui, &pal),
                            }
                        }
                    });
            } else {
                paint_ambient_backdrop(ui, &pal);

                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        self.render_top_bar(ui, &pal);
                        ui.add_space(8.0);

                        match self.active_tab {
                            AppTab::Games => self.render_games_tab(ui, &pal),
                            AppTab::SaveEditor => self.render_save_editor_tab(ui, &pal),
                            AppTab::Import => self.render_import_tab(ui, &pal),
                            AppTab::Logs => self.render_logs_tab(ui, &pal),
                            AppTab::Settings => self.render_settings_tab(ui, &pal),
                        }
                    });
            }
        });

        self.render_remove_game_prompt(&ctx, &pal);
        self.render_restore_dialog(&ctx, &pal);
        if !ctx.input(|i| i.pointer.any_down()) {
            if let Some(dirty_id) = self.kbm_dirty_game_id.take() {
                self.commit_kbm_game(&dirty_id, false);
            }
            if self.kbm_dirty_default {
                self.kbm_dirty_default = false;
                self.persist_config();
            }
        }
        self.service_capture(&ctx);
    }
}

/// Opens the desktop window.
pub fn run(start_minimized: bool) -> Result<()> {
    crate::autostart::ensure_linux_desktop_integration();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(WINDOW_TITLE)
            .with_app_id("savevault")
            .with_icon(Arc::new(build_app_icon()))
            .with_inner_size([1220.0, 820.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(move |cc| Ok(Box::new(SaveGameManagerApp::new(cc, start_minimized)))),
    )
    .map_err(|e| anyhow::anyhow!("Failed to launch GUI: {e}"))?;
    Ok(())
}

/// Opens SaveVault as an always-on-top, translucent in-game screen overlay (`--overlay`).
pub fn run_overlay() -> Result<()> {
    crate::autostart::ensure_linux_desktop_integration();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SaveVault Overlay")
            .with_app_id("savevault-overlay")
            .with_icon(Arc::new(build_app_icon()))
            .with_always_on_top()
            .with_decorations(false)
            .with_transparent(true)
            .with_inner_size([860.0, 560.0])
            .with_min_inner_size([640.0, 380.0]),
        ..Default::default()
    };

    eframe::run_native(
        "SaveVault Overlay",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(SaveGameManagerApp::new_with_mode(
                cc, false, true,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Failed to launch overlay GUI: {e}"))?;
    Ok(())
}
