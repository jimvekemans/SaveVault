//! RetroArch configuration parsing, core/directory resolution, and atomic per-game
//! `.cfg` / `.opt` override management.

use crate::config::write_private_atomic;
use crate::kbm::keys::{KbmAction, RetroArchHotkey, binding_to_retroarch_cfg};
use crate::kbm::mapping::hotkey_virtual_btn_index;
use crate::models::{GameEntry, Platform};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const BEGIN_OVERRIDE_MARKER: &str = "# --- BEGIN SAVEVAULT KBM OVERRIDE ---";
const END_OVERRIDE_MARKER: &str = "# --- END SAVEVAULT KBM OVERRIDE ---";
const SAVEVAULT_PREV_AXIS_SCALE_PREFIX: &str = "# savevault_prev_pcsx2_axis_scale1 = ";

/// Maps a controller action to RetroArch's `input_player1_<suffix>` key suffix.
pub fn action_retroarch_cfg_suffix(action: KbmAction) -> &'static str {
    match action {
        KbmAction::MoveUp => "l_y_minus",
        KbmAction::MoveDown => "l_y_plus",
        KbmAction::MoveLeft => "l_x_minus",
        KbmAction::MoveRight => "l_x_plus",
        KbmAction::FireR1 => "r",
        KbmAction::AimL1 => "l",
        KbmAction::GrenadeR2 => "r2",
        KbmAction::CrouchL2 => "l2",
        KbmAction::CrossB => "b",
        KbmAction::SquareY => "y",
        KbmAction::TriangleX => "x",
        KbmAction::CircleA => "a",
        KbmAction::SprintL3 => "l3",
        KbmAction::ZoomR3 => "r3",
        KbmAction::Select => "select",
        KbmAction::Start => "start",
        KbmAction::DpadUp => "up",
        KbmAction::DpadDown => "down",
        KbmAction::DpadLeft => "left",
        KbmAction::DpadRight => "right",
    }
}

/// Maps a hotkey function to RetroArch's `input_<key>` setting name (without `_btn`).
pub fn hotkey_retroarch_cfg_key(hotkey: RetroArchHotkey) -> &'static str {
    match hotkey {
        RetroArchHotkey::FastForwardHold => "input_hold_fast_forward",
        RetroArchHotkey::FastForwardToggle => "input_toggle_fast_forward",
        RetroArchHotkey::SlowMotionHold => "input_hold_slowmotion",
        RetroArchHotkey::SlowMotionToggle => "input_toggle_slowmotion",
        RetroArchHotkey::SaveState => "input_save_state",
        RetroArchHotkey::LoadState => "input_load_state",
        RetroArchHotkey::StateSlotPlus => "input_state_slot_increase",
        RetroArchHotkey::StateSlotMinus => "input_state_slot_decrease",
        RetroArchHotkey::MenuToggle => "input_menu_toggle",
        RetroArchHotkey::PauseToggle => "input_pause_toggle",
        RetroArchHotkey::MouseGrabToggle => "input_grab_mouse_toggle",
        RetroArchHotkey::Screenshot => "input_screenshot",
        RetroArchHotkey::AudioMute => "input_audio_mute",
        RetroArchHotkey::FpsToggle => "input_fps_toggle",
        RetroArchHotkey::ResetGame => "input_reset",
        RetroArchHotkey::ExitEmulator => "input_exit_emulator",
    }
}

/// Returns the host platform's default RetroArch configuration directory.
pub fn default_retroarch_config_dir() -> PathBuf {
    let mut candidates = Vec::new();
    match Platform::current() {
        Platform::Linux => {
            if let Some(cfg) = dirs::config_dir() {
                candidates.push(cfg.join("retroarch"));
            }
            if let Some(home) = dirs::home_dir() {
                candidates.push(home.join(".var/app/org.libretro.RetroArch/config/retroarch"));
            }
        }
        Platform::MacOs => {
            if let Some(home) = dirs::home_dir() {
                candidates.push(home.join("Library/Application Support/RetroArch"));
            }
        }
        Platform::Windows => {
            if let Some(roaming) = dirs::config_dir() {
                candidates.push(roaming.join("RetroArch"));
            }
            candidates.push(PathBuf::from(r"C:\RetroArch-Win64"));
        }
    }
    candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .or_else(|| candidates.into_iter().next())
        .unwrap_or_else(|| PathBuf::from(".retroarch"))
}

/// Subset of `retroarch.cfg` settings relevant to core, save, and save-state resolution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetroArchConfig {
    pub libretro_directory: Option<PathBuf>,
    pub savefile_directory: Option<PathBuf>,
    pub savestate_directory: Option<PathBuf>,
    pub sort_savefiles_enable: Option<bool>,
    pub sort_savefiles_by_content_enable: Option<bool>,
    pub sort_savestates_enable: Option<bool>,
    pub sort_savestates_by_content_enable: Option<bool>,
}

fn unquote_cfg_val(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

fn expand_ra_path(val: &str, ra_root: &Path) -> Option<PathBuf> {
    let v = unquote_cfg_val(val).trim();
    if v.is_empty() || v.eq_ignore_ascii_case("default") || v == "nul" {
        return None;
    }
    if let Some(rest) = v.strip_prefix(":/").or_else(|| v.strip_prefix(":\\")) {
        return Some(ra_root.join(rest));
    }
    if v == "~" {
        return dirs::home_dir();
    }
    if let Some(rest) = v.strip_prefix("~/").or_else(|| v.strip_prefix("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return Some(home.join(rest));
    }
    Some(PathBuf::from(v))
}

fn parse_ra_bool(val: &str) -> Option<bool> {
    match unquote_cfg_val(val).to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl RetroArchConfig {
    /// Parses `ra_root/retroarch.cfg` if present; falls back to RetroArch defaults when absent.
    pub fn load(ra_root: &Path) -> Self {
        let mut cfg = Self::default();
        let Ok(raw) = fs::read_to_string(ra_root.join("retroarch.cfg")) else {
            return cfg;
        };
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((k, v)) = trimmed.split_once('=') else {
                continue;
            };
            let key = k.trim();
            match key {
                "libretro_directory" => cfg.libretro_directory = expand_ra_path(v, ra_root),
                "savefile_directory" => cfg.savefile_directory = expand_ra_path(v, ra_root),
                "savestate_directory" => cfg.savestate_directory = expand_ra_path(v, ra_root),
                "sort_savefiles_enable" => cfg.sort_savefiles_enable = parse_ra_bool(v),
                "sort_savefiles_by_content_enable" => {
                    cfg.sort_savefiles_by_content_enable = parse_ra_bool(v);
                }
                "sort_savestates_enable" => cfg.sort_savestates_enable = parse_ra_bool(v),
                "sort_savestates_by_content_enable" => {
                    cfg.sort_savestates_by_content_enable = parse_ra_bool(v);
                }
                _ => {}
            }
        }
        cfg
    }

    fn resolve_dir(
        ra_root: &Path,
        configured_base: Option<&PathBuf>,
        default_sub: &str,
        core_name: &str,
        rom_path: &Path,
        sort_by_core: Option<bool>,
        sort_by_content: Option<bool>,
    ) -> PathBuf {
        let mut dir = configured_base
            .cloned()
            .unwrap_or_else(|| ra_root.join(default_sub));
        if sort_by_core != Some(false) && !core_name.is_empty() {
            dir = dir.join(core_name);
        }
        if sort_by_content == Some(true)
            && let Some(content_dir) = rom_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .filter(|s| !s.is_empty())
        {
            dir = dir.join(content_dir);
        }
        dir
    }

    /// Resolves the RetroArch save directory for a core and ROM path.
    pub fn saves_dir_for(&self, ra_root: &Path, core_name: &str, rom_path: &Path) -> PathBuf {
        Self::resolve_dir(
            ra_root,
            self.savefile_directory.as_ref(),
            "saves",
            core_name,
            rom_path,
            self.sort_savefiles_enable,
            self.sort_savefiles_by_content_enable,
        )
    }

    /// Resolves the RetroArch save-state directory for a core and ROM path.
    pub fn states_dir_for(&self, ra_root: &Path, core_name: &str, rom_path: &Path) -> PathBuf {
        Self::resolve_dir(
            ra_root,
            self.savestate_directory.as_ref(),
            "states",
            core_name,
            rom_path,
            self.sort_savestates_enable,
            self.sort_savestates_by_content_enable,
        )
    }
}

/// Candidate libretro core directories across Arch, Debian/Ubuntu, Fedora, Flatpak, macOS, and Windows.
pub fn core_search_directories(ra_root: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(root) = ra_root {
        let cfg = RetroArchConfig::load(root);
        if let Some(lib_dir) = cfg.libretro_directory {
            dirs.push(lib_dir);
        }
        dirs.push(root.join("cores"));
    }
    let default_ra = default_retroarch_config_dir();
    let default_cfg = RetroArchConfig::load(&default_ra);
    if let Some(lib_dir) = default_cfg.libretro_directory {
        dirs.push(lib_dir);
    }
    dirs.push(default_ra.join("cores"));

    match Platform::current() {
        Platform::Linux => {
            if let Some(home) = dirs::home_dir() {
                dirs.push(home.join(".var/app/org.libretro.RetroArch/config/retroarch/cores"));
                dirs.push(home.join(".config/retroarch/cores"));
            }
            for sys in [
                "/usr/lib/libretro",
                "/usr/lib64/libretro",
                "/usr/lib/x86_64-linux-gnu/libretro",
                "/usr/lib/aarch64-linux-gnu/libretro",
                "/usr/local/lib/libretro",
            ] {
                dirs.push(PathBuf::from(sys));
            }
        }
        Platform::MacOs => {
            if let Some(home) = dirs::home_dir() {
                dirs.push(home.join("Library/Application Support/RetroArch/cores"));
            }
        }
        Platform::Windows => {
            if let Some(roaming) = dirs::config_dir() {
                dirs.push(roaming.join("RetroArch/cores"));
            }
            dirs.push(PathBuf::from(r"C:\RetroArch-Win64\cores"));
        }
    }

    let mut deduped = Vec::new();
    for d in dirs {
        if !deduped.contains(&d) {
            deduped.push(d);
        }
    }
    deduped
}

/// Locates `<core_stem><DLL_SUFFIX>` across configured and standard RetroArch core directories.
pub fn find_core_library(ra_root: Option<&Path>, core_stem: &str) -> PathBuf {
    let filename = format!("{core_stem}{}", std::env::consts::DLL_SUFFIX);
    let candidates = core_search_directories(ra_root);
    for dir in &candidates {
        let candidate = dir.join(&filename);
        if candidate.is_file() {
            return candidate;
        }
    }
    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| default_retroarch_config_dir().join("cores"))
        .join(filename)
}

fn rom_has_dir_component(rom_path: &Path, targets: &[&str]) -> bool {
    rom_path.parent().is_some_and(|parent| {
        parent.components().any(|c| {
            let s = c.as_os_str().to_string_lossy().to_ascii_lowercase();
            targets.iter().any(|t| s == *t)
        })
    })
}

fn infer_generic_core_name(core_name_hint: Option<&str>, core_path: &Path) -> String {
    if let Some(hint) = core_name_hint.map(str::trim).filter(|s| !s.is_empty() && *s != "DETECT")
    {
        if let (Some(start), Some(end)) = (hint.rfind('('), hint.rfind(')'))
            && end > start + 1
        {
            let inner = hint[start + 1..end].trim();
            if !inner.is_empty() {
                return inner.to_string();
            }
        }
        return hint.to_string();
    }
    core_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim_end_matches("_libretro").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "RetroArch".to_string())
}

/// Maps a libretro core hint and/or ROM path (when an explicit core or unambiguous system exists)
/// to `(core_library_path, core_dir_name, system_name)`.
pub fn resolve_core_for_rom_with_root(
    ra_root: Option<&Path>,
    core_path_hint: Option<&str>,
    core_name_hint: Option<&str>,
    rom_path: &Path,
) -> Option<(PathBuf, String, String)> {
    let valid_core_hint = core_path_hint
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("DETECT"));
    let hint_lower = format!(
        "{} {}",
        valid_core_hint.unwrap_or(""),
        core_name_hint.unwrap_or("")
    )
    .to_lowercase();
    let ext = rom_path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    let is_nds = hint_lower.contains("melonds")
        || hint_lower.contains("desmume")
        || matches!(ext.as_str(), "nds" | "dsi" | "ids");
    if is_nds {
        let so = valid_core_hint
            .map(PathBuf::from)
            .unwrap_or_else(|| find_core_library(ra_root, "melondsds_libretro"));
        return Some((
            so,
            "melonDS DS".to_string(),
            "Nintendo - Nintendo DS".to_string(),
        ));
    }

    let is_3ds = hint_lower.contains("azahar")
        || hint_lower.contains("citra")
        || matches!(ext.as_str(), "3ds" | "cci" | "cxi" | "3dsx");
    if is_3ds {
        let so = valid_core_hint
            .map(PathBuf::from)
            .unwrap_or_else(|| find_core_library(ra_root, "azahar_libretro"));
        return Some((
            so,
            "Azahar".to_string(),
            "Nintendo - Nintendo 3DS".to_string(),
        ));
    }

    let is_ps2 = hint_lower.contains("pcsx2")
        || hint_lower.contains("lrps2")
        || hint_lower.contains("parallel ps2")
        || hint_lower.contains("playstation 2")
        || rom_has_dir_component(rom_path, &["ps2", "playstation 2", "pcsx2"]);
    if is_ps2 {
        let so = valid_core_hint
            .map(PathBuf::from)
            .unwrap_or_else(|| find_core_library(ra_root, "pcsx2_libretro"));
        return Some((
            so,
            "ParaLLEl PS2".to_string(),
            "Sony - PlayStation 2".to_string(),
        ));
    }

    if let Some(explicit_core) = valid_core_hint {
        let so = PathBuf::from(explicit_core);
        let core_dir = infer_generic_core_name(core_name_hint, &so);
        let sys = core_name_hint
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("DETECT"))
            .unwrap_or(core_dir.as_str())
            .to_string();
        return Some((so, core_dir, sys));
    }

    None
}

/// Convenience wrapper for `resolve_core_for_rom_with_root(None, ...)`.
pub fn resolve_core_for_rom(
    core_path_hint: Option<&str>,
    core_name_hint: Option<&str>,
    rom_path: &Path,
) -> Option<(PathBuf, String, String)> {
    resolve_core_for_rom_with_root(None, core_path_hint, core_name_hint, rom_path)
}

/// Returns true when `cfg_path` contains SaveVault's KBM override marker block.
pub fn infer_kbm_from_existing_cfg(cfg_path: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(cfg_path) else {
        return false;
    };
    raw.lines()
        .any(|line| line.trim() == BEGIN_OVERRIDE_MARKER)
}

/// Returns the RetroArch per-game override `.cfg` path for `game`.
pub fn game_override_cfg_path(game: &GameEntry) -> Option<PathBuf> {
    let meta = game.retroarch.as_ref()?;
    let ra_root = game
        .library_root
        .clone()
        .unwrap_or_else(default_retroarch_config_dir);
    let stem = meta
        .rom_path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())?;
    Some(
        ra_root
            .join("config")
            .join(&meta.core_name)
            .join(format!("{stem}.cfg")),
    )
}

fn strip_savevault_override_block(raw: &str) -> Vec<String> {
    let mut kept = Vec::new();
    let mut inside_block = false;
    let mut inside_legacy = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed == BEGIN_OVERRIDE_MARKER {
            inside_block = true;
            continue;
        }
        if trimmed == END_OVERRIDE_MARKER {
            inside_block = false;
            continue;
        }
        if !inside_block && !inside_legacy && trimmed.starts_with("input_auto_mouse_grab = ")
            && raw.contains("# Mouse & Keyboard Override Bindings (managed by SaveVault)")
        {
            inside_legacy = true;
            continue;
        }
        if inside_legacy {
            if trimmed == "input_netplay_player_chat = \"nul\"" {
                inside_legacy = false;
            }
            continue;
        }
        if !inside_block {
            kept.push(line.to_string());
        }
    }
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    kept
}

fn sync_opt_axis_scale(opt_path: &Path, force_100: bool) -> Result<()> {
    if !opt_path.is_file() {
        return Ok(());
    }
    let opt_raw = fs::read_to_string(opt_path)
        .with_context(|| format!("Failed to read core options {}", opt_path.display()))?;
    let mut saved_prev: Option<String> = None;
    for line in opt_raw.lines() {
        if let Some(rest) = line.trim_start().strip_prefix(SAVEVAULT_PREV_AXIS_SCALE_PREFIX) {
            saved_prev = Some(rest.trim().to_string());
            break;
        }
    }

    let mut next_lines = Vec::new();
    let mut modified = false;

    if force_100 {
        let mut current_val: Option<String> = None;
        for line in opt_raw.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with(SAVEVAULT_PREV_AXIS_SCALE_PREFIX) {
                continue;
            }
            if trimmed.starts_with("pcsx2_axis_scale1")
                && let Some((_, val)) = trimmed.split_once('=')
            {
                let v = val.trim().to_string();
                current_val = Some(v.clone());
                if v != "\"100%\"" {
                    modified = true;
                    next_lines.push("pcsx2_axis_scale1 = \"100%\"".to_string());
                } else {
                    next_lines.push(line.to_string());
                }
            } else {
                next_lines.push(line.to_string());
            }
        }
        let backup_val = saved_prev.or_else(|| current_val.filter(|v| v != "\"100%\""));
        if let Some(prev) = backup_val {
            next_lines.insert(0, format!("{SAVEVAULT_PREV_AXIS_SCALE_PREFIX}{prev}"));
        }
    } else if let Some(prev) = saved_prev {
        modified = true;
        for line in opt_raw.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with(SAVEVAULT_PREV_AXIS_SCALE_PREFIX) {
                continue;
            }
            if trimmed.starts_with("pcsx2_axis_scale1") {
                next_lines.push(format!("pcsx2_axis_scale1 = {prev}"));
            } else {
                next_lines.push(line.to_string());
            }
        }
    }

    if modified {
        let out = format!("{}\n", next_lines.join("\n"));
        write_private_atomic(opt_path, out.as_bytes())
            .with_context(|| format!("Failed to update {}", opt_path.display()))?;
    }
    Ok(())
}

/// Atomically updates or removes SaveVault's fenced KBM override section in `<stem>.cfg` and
/// manages `pcsx2_axis_scale1` reversibly in sibling `<stem>.opt` files.
///
/// User lines outside SaveVault's `# --- BEGIN SAVEVAULT KBM OVERRIDE ---` block are always kept.
/// Disabling KBM never creates an empty `.cfg` file.
pub fn apply_kbm_profile_to_retroarch(game: &GameEntry) -> Result<Option<PathBuf>> {
    let Some(meta) = game.retroarch.as_ref() else {
        return Ok(None);
    };
    let Some(cfg_path) = game_override_cfg_path(game) else {
        return Ok(None);
    };

    let force_opt_100 = meta.kbm_enabled && meta.kbm_profile.aiming_overhaul_enabled;
    sync_opt_axis_scale(&cfg_path.with_extension("opt"), force_opt_100)?;

    let existing = fs::read_to_string(&cfg_path).ok();

    if !meta.kbm_enabled {
        let Some(raw) = existing else {
            return Ok(None);
        };
        let remaining = strip_savevault_override_block(&raw);
        if remaining.is_empty() {
            let _ = fs::remove_file(&cfg_path);
            return Ok(None);
        }
        let rendered = format!("{}\n", remaining.join("\n"));
        write_private_atomic(&cfg_path, rendered.as_bytes())
            .with_context(|| format!("Failed to write {}", cfg_path.display()))?;
        return Ok(Some(cfg_path));
    }

    if let Some(parent) = cfg_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create RetroArch override dir {}", parent.display()))?;
    }

    let mut out = existing
        .as_deref()
        .map(strip_savevault_override_block)
        .unwrap_or_default();
    if !out.is_empty() {
        out.push(String::new());
    }

    let p = &meta.kbm_profile;
    out.push(BEGIN_OVERRIDE_MARKER.to_string());
    out.push(format!(
        "input_auto_mouse_grab = \"{}\"",
        if p.auto_mouse_grab { "true" } else { "false" }
    ));
    out.push("input_player1_analog_dpad_mode = \"0\"".to_string());
    out.push("menu_disable_right_analog = \"true\"".to_string());

    for &action in KbmAction::all() {
        let suffix = action_retroarch_cfg_suffix(action);
        let mut key_val: Option<&'static str> = None;
        let mut mbtn_val: Option<&'static str> = None;
        if let Some(list) = p.bindings.get(&action) {
            for b in list {
                if b.contains('+') {
                    continue;
                }
                let (is_mbtn, mapped) = binding_to_retroarch_cfg(b);
                if let Some(m) = mapped {
                    if is_mbtn && mbtn_val.is_none() {
                        mbtn_val = Some(m);
                    } else if !is_mbtn && key_val.is_none() {
                        key_val = Some(m);
                    }
                }
            }
        }
        if let Some(kv) = key_val {
            out.push(format!("input_player1_{suffix} = \"{kv}\""));
        } else {
            out.push(format!("input_player1_{suffix} = \"nul\""));
        }
        if let Some(mv) = mbtn_val {
            out.push(format!("input_player1_{suffix}_mbtn = \"{mv}\""));
        }
    }

    out.push("input_enable_hotkey = \"nul\"".to_string());
    out.push("input_enable_hotkey_btn = \"nul\"".to_string());
    for &hk in RetroArchHotkey::all() {
        let key_name = hotkey_retroarch_cfg_key(hk);
        out.push(format!("{key_name} = \"nul\""));
        out.push(format!("{key_name}_btn = \"{}\"", hotkey_virtual_btn_index(hk)));
    }
    for k in [
        "input_frame_advance",
        "input_rewind",
        "input_shader_next",
        "input_shader_prev",
        "input_shader_toggle",
        "input_cheat_index_plus",
        "input_cheat_index_minus",
        "input_cheat_toggle",
        "input_desktop_menu_toggle",
        "input_netplay_game_watch",
        "input_netplay_player_chat",
    ] {
        out.push(format!("{k} = \"nul\""));
    }
    out.push(END_OVERRIDE_MARKER.to_string());

    let rendered = format!("{}\n", out.join("\n"));
    write_private_atomic(&cfg_path, rendered.as_bytes())
        .with_context(|| format!("Failed to write {}", cfg_path.display()))?;
    Ok(Some(cfg_path))
}
