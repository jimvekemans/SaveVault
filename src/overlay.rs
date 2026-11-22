//! In-game screen overlay hotkey parsing, OS shortcut passthrough, and toggle IPC.
//!
//! Instead of polling or grabbing keyboard devices in the background, SaveVault registers
//! the configured overlay toggle shortcut directly with the user's desktop environment /
//! compositor (COSMIC, GNOME, KDE Plasma, Hyprland, Sway, XFCE) to invoke
//! `save_game_manager --toggle-overlay`.

use crate::kbm::keys::{egui_key_to_binding_name, lookup_key_spec};
use crate::models::{AppConfig, Platform};
use anyhow::{Context, Result, bail};
use eframe::egui;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Preset hotkeys offered in Settings for quick selection.
pub const OVERLAY_HOTKEY_PRESETS: &[&str] = &[
    "LAlt+LShift+S",
    "LCtrl+LShift+S",
    "LAlt+F10",
    "LAlt+F12",
    "LCtrl+F9",
    "LAlt+Grave",
    "Super+LShift+S",
    "F10",
    "F12",
];

/// Parsed representation of the overlay toggle shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedOverlayHotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
    /// Canonical non-modifier key name from `KEY_SPECS` (e.g. `"S"`, `"F10"`, `"Grave"`).
    pub key: String,
}

impl ParsedOverlayHotkey {
    /// Formats the hotkey in SaveVault's canonical `+`-separated representation.
    pub fn canonical(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.super_key {
            parts.push("Super");
        }
        if self.ctrl {
            parts.push("LCtrl");
        }
        if self.alt {
            parts.push("LAlt");
        }
        if self.shift {
            parts.push("LShift");
        }
        parts.push(&self.key);
        parts.join("+")
    }

    /// Maps the non-modifier key to its `xkbcommon` keysym name (without `KEY_` prefix).
    pub fn to_xkb_keysym(&self) -> &'static str {
        match self.key.as_str() {
            "A" => "a",
            "B" => "b",
            "C" => "c",
            "D" => "d",
            "E" => "e",
            "F" => "f",
            "G" => "g",
            "H" => "h",
            "I" => "i",
            "J" => "j",
            "K" => "k",
            "L" => "l",
            "M" => "m",
            "N" => "n",
            "O" => "o",
            "P" => "p",
            "Q" => "q",
            "R" => "r",
            "S" => "s",
            "T" => "t",
            "U" => "u",
            "V" => "v",
            "W" => "w",
            "X" => "x",
            "Y" => "y",
            "Z" => "z",
            "0" => "0",
            "1" => "1",
            "2" => "2",
            "3" => "3",
            "4" => "4",
            "5" => "5",
            "6" => "6",
            "7" => "7",
            "8" => "8",
            "9" => "9",
            "F1" => "F1",
            "F2" => "F2",
            "F3" => "F3",
            "F4" => "F4",
            "F5" => "F5",
            "F6" => "F6",
            "F7" => "F7",
            "F8" => "F8",
            "F9" => "F9",
            "F10" => "F10",
            "F11" => "F11",
            "F12" => "F12",
            "Escape" => "Escape",
            "Tab" => "Tab",
            "Space" => "space",
            "Enter" => "Return",
            "Backspace" => "BackSpace",
            "Delete" => "Delete",
            "Insert" => "Insert",
            "Home" => "Home",
            "End" => "End",
            "PageUp" => "Page_Up",
            "PageDown" => "Page_Down",
            "Left" => "Left",
            "Right" => "Right",
            "Up" => "Up",
            "Down" => "Down",
            "Minus" => "minus",
            "Equal" => "equal",
            "Grave" => "grave",
            "LeftBrace" => "bracketleft",
            "RightBrace" => "bracketright",
            "Backslash" => "backslash",
            "Semicolon" => "semicolon",
            "Apostrophe" => "apostrophe",
            "Comma" => "comma",
            "Dot" => "period",
            "Slash" => "slash",
            "CapsLock" => "Caps_Lock",
            _ => "s",
        }
    }

    /// Returns the modifier list in COSMIC Desktop RON order.
    pub fn to_cosmic_modifiers(&self) -> Vec<&'static str> {
        let mut mods = Vec::new();
        if self.super_key {
            mods.push("Super");
        }
        if self.ctrl {
            mods.push("Ctrl");
        }
        if self.alt {
            mods.push("Alt");
        }
        if self.shift {
            mods.push("Shift");
        }
        mods
    }

    /// Formats the hotkey as a GTK / GNOME `gsettings` accelerator string (e.g. `<Alt><Shift>s`).
    pub fn to_gnome_accelerator(&self) -> String {
        let mut out = String::new();
        if self.super_key {
            out.push_str("<Super>");
        }
        if self.ctrl {
            out.push_str("<Control>");
        }
        if self.alt {
            out.push_str("<Alt>");
        }
        if self.shift {
            out.push_str("<Shift>");
        }
        out.push_str(self.to_xkb_keysym());
        out
    }

    /// Formats the hotkey as a KDE / Qt global shortcut string (e.g. `Alt+Shift+S`).
    pub fn to_kde_shortcut(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.super_key {
            parts.push("Meta");
        }
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(&self.key);
        parts.join("+")
    }

    /// Formats the hotkey for `hyprctl keyword bind` (e.g. `ALT SHIFT, s`).
    pub fn to_hyprland_bind(&self) -> String {
        let mut mods = Vec::new();
        if self.super_key {
            mods.push("SUPER");
        }
        if self.ctrl {
            mods.push("CTRL");
        }
        if self.alt {
            mods.push("ALT");
        }
        if self.shift {
            mods.push("SHIFT");
        }
        format!("{}, {}", mods.join(" "), self.to_xkb_keysym())
    }

    /// Formats the hotkey for Sway's `bindsym` (e.g. `Alt+Shift+s`).
    pub fn to_sway_bind(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.super_key {
            parts.push("Mod4");
        }
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(self.to_xkb_keysym());
        parts.join("+")
    }

    /// Formats the hotkey for XFCE `xfconf-query` (same accelerator syntax as GTK).
    pub fn to_xfce_shortcut(&self) -> String {
        self.to_gnome_accelerator()
    }
}

/// Parses a human-readable hotkey combo such as `"LAlt+LShift+S"` or `"ctrl+shift+f10"`.
/// Rejects empty strings, modifier-only combos, mouse buttons, or combos with multiple keys.
pub fn parse_overlay_hotkey(raw: &str) -> Option<ParsedOverlayHotkey> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut super_key = false;
    let mut key: Option<String> = None;

    for raw_part in trimmed.split('+') {
        let token = raw_part.trim();
        if token.is_empty() {
            return None;
        }
        match token.to_ascii_lowercase().as_str() {
            "lctrl" | "rctrl" | "ctrl" | "control" => ctrl = true,
            "lalt" | "ralt" | "alt" => alt = true,
            "lshift" | "rshift" | "shift" => shift = true,
            "super" | "meta" | "logo" | "win" | "cmd" | "command" => super_key = true,
            _ => {
                let spec = lookup_key_spec(token)?;
                if spec.is_mouse_button
                    || matches!(
                        spec.canonical,
                        "LCtrl" | "RCtrl" | "LAlt" | "RAlt" | "LShift" | "RShift"
                    )
                    || key.is_some()
                {
                    return None;
                }
                key = Some(spec.canonical.to_string());
            }
        }
    }

    let key = key?;
    Some(ParsedOverlayHotkey {
        ctrl,
        alt,
        shift,
        super_key,
        key,
    })
}

/// Validates and canonicalizes an overlay hotkey string.
pub fn normalize_overlay_hotkey(raw: &str) -> Option<String> {
    parse_overlay_hotkey(raw).map(|h| h.canonical())
}

/// Result of polling `egui` events while capturing a new overlay hotkey in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayCapturePoll {
    Pending,
    Captured(String),
    Cancelled,
}

/// Polls `egui` keyboard events to capture a single key or modifier+key combination.
pub fn capture_overlay_hotkey_from_egui(
    ctx: &egui::Context,
    armed_frame: &mut u8,
) -> OverlayCapturePoll {
    if *armed_frame == 0 {
        *armed_frame = 1;
        ctx.request_repaint();
        return OverlayCapturePoll::Pending;
    }

    let events = ctx.input(|i| i.events.clone());
    for ev in &events {
        match ev {
            egui::Event::Key {
                key: egui::Key::Escape,
                pressed: true,
                modifiers,
                ..
            } if !modifiers.ctrl
                && !modifiers.alt
                && !modifiers.shift
                && !modifiers.mac_cmd
                && !modifiers.command =>
            {
                return OverlayCapturePoll::Cancelled;
            }
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => {
                if let Some(name) = egui_key_to_binding_name(*key) {
                    let parsed = ParsedOverlayHotkey {
                        ctrl: modifiers.ctrl,
                        alt: modifiers.alt,
                        shift: modifiers.shift,
                        super_key: false,
                        key: name.to_string(),
                    };
                    return OverlayCapturePoll::Captured(parsed.canonical());
                }
            }
            _ => {}
        }
    }

    ctx.request_repaint();
    OverlayCapturePoll::Pending
}

/// Returns true when the configured overlay hotkey was just pressed inside the focused `egui` window.
pub fn overlay_hotkey_pressed_in_egui(ctx: &egui::Context, raw_hotkey: &str) -> bool {
    let Some(parsed) = parse_overlay_hotkey(raw_hotkey) else {
        return false;
    };
    // `egui::Modifiers` does not distinguish the Super/Meta key on Linux; skip matching here so
    // pressing e.g. Shift+S inside the overlay does not falsely match Super+LShift+S.
    if parsed.super_key {
        return false;
    }
    let Some(target_key) = lookup_key_spec(&parsed.key).and_then(|s| s.egui) else {
        return false;
    };
    ctx.input(|i| {
        i.events.iter().any(|ev| {
            matches!(
                ev,
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } if *key == target_key
                    && modifiers.ctrl == parsed.ctrl
                    && modifiers.alt == parsed.alt
                    && modifiers.shift == parsed.shift
            )
        })
    })
}

/// Splits top-level comma-separated RON entries inside `{ ... }` or `[ ... ]` while respecting
/// nested parentheses/brackets/braces, string literals, and `//` / `/* */` comments.
fn split_ron_top_level(inner: &str) -> Vec<&str> {
    let bytes = inner.as_bytes();
    let mut items = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    let mut in_line_comment = false;
    let mut block_comment_depth = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_comment_depth > 0 {
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                block_comment_depth += 1;
                i += 2;
                continue;
            }
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                block_comment_depth -= 1;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            block_comment_depth = 1;
            i += 2;
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                let slice = inner[start..i].trim();
                if !slice.is_empty() {
                    items.push(slice);
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }

    let tail = inner[start..].trim();
    if !tail.is_empty() {
        items.push(tail);
    }
    items
}

fn extract_ron_delimited_body(raw: &str, open: char, close: char) -> &str {
    let trimmed = raw.trim();
    if let (Some(start), Some(end)) = (trimmed.find(open), trimmed.rfind(close))
        && start < end
    {
        return &trimmed[start + open.len_utf8()..end];
    }
    ""
}

fn split_ron_map_entry_key(entry: &str) -> Option<&str> {
    let bytes = entry.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b':' if depth == 0 => return Some(entry[..i].trim()),
            _ => {}
        }
        i += 1;
    }
    None
}

fn normalize_ron_key_signature(key_part: &str) -> String {
    let compact: String = key_part.chars().filter(|c| !c.is_whitespace()).collect();
    compact.replace(",]", "]").replace(",)", ")")
}

/// Updates COSMIC Desktop's `com.system76.CosmicSettings.Shortcuts/v1/custom` RON map in-place,
/// preserving all user shortcuts while adding, updating, or removing SaveVault's `--toggle-overlay`
/// binding.
pub fn update_cosmic_shortcuts_ron(
    existing: &str,
    enabled: bool,
    hotkey: Option<&ParsedOverlayHotkey>,
    command: &str,
) -> String {
    let inner = extract_ron_delimited_body(existing, '{', '}');
    let mut entries: Vec<String> = split_ron_top_level(inner)
        .into_iter()
        .filter(|entry| {
            !entry.contains("--toggle-overlay") && !entry.contains("savevault-overlay")
        })
        .map(str::to_string)
        .collect();

    if enabled && let Some(hk) = hotkey {
        let mods = hk.to_cosmic_modifiers();
        let mods_body = if mods.is_empty() {
            "[]".to_string()
        } else {
            let items = mods
                .into_iter()
                .map(|m| format!("            {m},\n"))
                .collect::<String>();
            format!("[\n{items}        ]")
        };
        let escaped_cmd = command.replace('\\', "\\\\").replace('"', "\\\"");
        let new_entry = format!(
            "(\n        modifiers: {mods_body},\n        key: \"{}\",\n    ): Spawn(\"{escaped_cmd}\")",
            hk.to_xkb_keysym()
        );
        let new_sig = split_ron_map_entry_key(&new_entry).map(normalize_ron_key_signature);
        if let Some(target_sig) = new_sig {
            entries.retain(|e| {
                split_ron_map_entry_key(e).map(normalize_ron_key_signature)
                    != Some(target_sig.clone())
            });
        }
        entries.push(new_entry);
    }

    if entries.is_empty() {
        return "{\n}\n".to_string();
    }

    let mut out = String::from("{\n");
    for entry in entries {
        out.push_str("    ");
        out.push_str(entry.trim().trim_end_matches(','));
        out.push_str(",\n");
    }
    out.push_str("}\n");
    out
}

/// Updates COSMIC Desktop's `com.system76.CosmicSettings.WindowRules/v1/tiling_exception_custom`
/// RON list so `appid: "savevault-overlay"` always floats on top instead of being auto-tiled.
pub fn update_cosmic_tiling_exceptions_ron(existing: &str, enabled: bool) -> String {
    let inner = extract_ron_delimited_body(existing, '[', ']');
    let mut items: Vec<String> = split_ron_top_level(inner)
        .into_iter()
        .filter(|item| !item.contains("savevault-overlay"))
        .map(str::to_string)
        .collect();

    if enabled {
        items.push(
            "(\n        appid: \"savevault-overlay\",\n        title: \".*\",\n        enabled: true,\n    )"
                .to_string(),
        );
    }

    if items.is_empty() {
        return "[\n]\n".to_string();
    }

    let mut out = String::from("[\n");
    for item in items {
        out.push_str("    ");
        out.push_str(item.trim().trim_end_matches(','));
        out.push_str(",\n");
    }
    out.push_str("]\n");
    out
}

/// Result returned when registering or removing the overlay shortcut with the OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsHotkeyStatus {
    pub backend: String,
    pub registered: bool,
    pub message: String,
}

fn resolve_launch_exe() -> Result<PathBuf> {
    match std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    {
        Some(appimage) => Ok(appimage),
        None => std::env::current_exe().context("Could not determine current executable path"),
    }
}

/// Returns the shell-safe command registered with the OS to toggle the overlay.
pub fn resolve_overlay_toggle_command() -> Result<String> {
    let exe = resolve_launch_exe()?;
    Ok(format!("\"{}\" --toggle-overlay", exe.display()))
}

fn desktop_env_lower() -> String {
    let current = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let session = std::env::var("XDG_SESSION_DESKTOP").unwrap_or_default();
    format!("{current} {session}").to_ascii_lowercase()
}

fn sync_cosmic_hotkey(
    cosmic_dir: &Path,
    enabled: bool,
    hotkey: Option<&ParsedOverlayHotkey>,
    command: &str,
) -> Result<OsHotkeyStatus> {
    let shortcuts_path = cosmic_dir
        .join("com.system76.CosmicSettings.Shortcuts")
        .join("v1")
        .join("custom");
    let rules_path = cosmic_dir
        .join("com.system76.CosmicSettings.WindowRules")
        .join("v1")
        .join("tiling_exception_custom");

    let existing_shortcuts = fs::read_to_string(&shortcuts_path).unwrap_or_default();
    let next_shortcuts = update_cosmic_shortcuts_ron(&existing_shortcuts, enabled, hotkey, command);
    if let Some(parent) = shortcuts_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    fs::write(&shortcuts_path, next_shortcuts)
        .with_context(|| format!("Failed to write {}", shortcuts_path.display()))?;

    let existing_rules = fs::read_to_string(&rules_path).unwrap_or_default();
    let next_rules = update_cosmic_tiling_exceptions_ron(&existing_rules, enabled);
    if let Some(parent) = rules_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    fs::write(&rules_path, next_rules)
        .with_context(|| format!("Failed to write {}", rules_path.display()))?;

    let message = if enabled {
        let hk_str = hotkey.map(ParsedOverlayHotkey::canonical).unwrap_or_default();
        format!("Registered OS global hotkey {hk_str} with COSMIC Desktop.")
    } else {
        "Removed SaveVault overlay hotkey from COSMIC Desktop.".to_string()
    };
    Ok(OsHotkeyStatus {
        backend: "COSMIC Desktop".to_string(),
        registered: enabled,
        message,
    })
}

fn parse_gsettings_path_list(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "@as []" || trimmed == "[]" {
        return Vec::new();
    }
    let inner = trimmed
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    if inner.is_empty() {
        return Vec::new();
    }
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('\'').trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn sync_gnome_hotkey(
    enabled: bool,
    hotkey: Option<&ParsedOverlayHotkey>,
    command: &str,
) -> Result<OsHotkeyStatus> {
    const BASE_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
    const KEY_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
    const ENTRY_PATH: &str =
        "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/savevault-overlay/";

    let get_out = Command::new("gsettings")
        .args(["get", BASE_SCHEMA, "custom-keybindings"])
        .output()
        .context("Failed to execute gsettings")?;
    if !get_out.status.success() {
        bail!(
            "gsettings failed: {}",
            String::from_utf8_lossy(&get_out.stderr).trim()
        );
    }

    let mut paths = parse_gsettings_path_list(&String::from_utf8_lossy(&get_out.stdout));
    paths.retain(|p| p != ENTRY_PATH);

    if enabled {
        let hk = hotkey.context("Missing overlay hotkey")?;
        paths.push(ENTRY_PATH.to_string());
        let formatted_list = format!(
            "[{}]",
            paths
                .iter()
                .map(|p| format!("'{p}'"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let schema_with_path = format!("{KEY_SCHEMA}:{ENTRY_PATH}");
        let binding = hk.to_gnome_accelerator();
        for args in [
            vec![BASE_SCHEMA, "custom-keybindings", &formatted_list],
            vec![&schema_with_path, "name", "Toggle SaveVault Overlay"],
            vec![&schema_with_path, "command", command],
            vec![&schema_with_path, "binding", &binding],
        ] {
            let status = Command::new("gsettings")
                .arg("set")
                .args(&args)
                .status()
                .context("Failed to run gsettings set")?;
            if !status.success() {
                bail!("gsettings set failed for {:?}", args);
            }
        }
        Ok(OsHotkeyStatus {
            backend: "GNOME".to_string(),
            registered: true,
            message: format!("Registered OS global hotkey {} with GNOME.", hk.canonical()),
        })
    } else {
        let formatted_list = if paths.is_empty() {
            "@as []".to_string()
        } else {
            format!(
                "[{}]",
                paths
                    .iter()
                    .map(|p| format!("'{p}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let _ = Command::new("gsettings")
            .args(["set", BASE_SCHEMA, "custom-keybindings", &formatted_list])
            .status();
        let schema_with_path = format!("{KEY_SCHEMA}:{ENTRY_PATH}");
        for prop in ["name", "command", "binding"] {
            let _ = Command::new("gsettings")
                .args(["reset", &schema_with_path, prop])
                .status();
        }
        Ok(OsHotkeyStatus {
            backend: "GNOME".to_string(),
            registered: false,
            message: "Removed SaveVault overlay hotkey from GNOME.".to_string(),
        })
    }
}

fn sync_kde_hotkey(
    enabled: bool,
    hotkey: Option<&ParsedOverlayHotkey>,
    command: &str,
) -> Result<OsHotkeyStatus> {
    let data_dir = dirs::data_local_dir().context("Could not find XDG data directory")?;
    let desktop_path = data_dir
        .join("applications")
        .join("savevault-overlay.desktop");
    let config_dir = dirs::config_dir().context("Could not find XDG config directory")?;
    let kglobal_path = config_dir.join("kglobalshortcutsrc");

    if enabled {
        let hk = hotkey.context("Missing overlay hotkey")?;
        if let Some(parent) = desktop_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let desktop_body = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=SaveVault Overlay\n\
             Comment=Toggle SaveVault In-Game Screen Overlay\n\
             Exec={command}\n\
             Icon=savevault\n\
             NoDisplay=true\n\
             X-KDE-GlobalAccel-CommandShortcut=true\n"
        );
        fs::write(&desktop_path, desktop_body)?;

        let existing = fs::read_to_string(&kglobal_path).unwrap_or_default();
        let section_header = "[services][savevault-overlay.desktop]";
        let kde_shortcut = hk.to_kde_shortcut();
        let mut lines: Vec<String> = Vec::new();
        let mut in_target = false;
        for line in existing.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_target = trimmed == section_header;
                if in_target {
                    continue;
                }
            }
            if !in_target {
                lines.push(line.to_string());
            }
        }
        lines.push(section_header.to_string());
        lines.push(format!(
            "_launch={kde_shortcut},none,Toggle SaveVault Overlay"
        ));
        fs::write(&kglobal_path, format!("{}\n", lines.join("\n")))?;

        Ok(OsHotkeyStatus {
            backend: "KDE Plasma".to_string(),
            registered: true,
            message: format!(
                "Registered OS global hotkey {} with KDE Plasma ({}).",
                hk.canonical(),
                kglobal_path.display()
            ),
        })
    } else {
        if desktop_path.exists() {
            let _ = fs::remove_file(&desktop_path);
        }
        if let Ok(existing) = fs::read_to_string(&kglobal_path) {
            let section_header = "[services][savevault-overlay.desktop]";
            let mut lines: Vec<String> = Vec::new();
            let mut in_target = false;
            for line in existing.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('[') {
                    in_target = trimmed == section_header;
                    if in_target {
                        continue;
                    }
                }
                if !in_target {
                    lines.push(line.to_string());
                }
            }
            let _ = fs::write(&kglobal_path, format!("{}\n", lines.join("\n")));
        }
        Ok(OsHotkeyStatus {
            backend: "KDE Plasma".to_string(),
            registered: false,
            message: "Removed SaveVault overlay hotkey from KDE Plasma.".to_string(),
        })
    }
}

fn sync_hyprland_hotkey(
    enabled: bool,
    hotkey: Option<&ParsedOverlayHotkey>,
    command: &str,
) -> Result<OsHotkeyStatus> {
    if let Some(hk) = hotkey {
        let _ = Command::new("hyprctl")
            .args(["keyword", "unbind", &hk.to_hyprland_bind()])
            .status();
    }
    if enabled {
        let hk = hotkey.context("Missing overlay hotkey")?;
        let bind_arg = format!("{}, exec, {command}", hk.to_hyprland_bind());
        let out = Command::new("hyprctl")
            .args(["keyword", "bind", &bind_arg])
            .output()
            .context("Failed to execute hyprctl")?;
        if !out.status.success() {
            bail!(
                "hyprctl failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let _ = Command::new("hyprctl")
            .args([
                "keyword",
                "windowrulev2",
                "float, class:^(savevault-overlay)$",
            ])
            .status();
        Ok(OsHotkeyStatus {
            backend: "Hyprland".to_string(),
            registered: true,
            message: format!("Registered OS global hotkey {} with Hyprland.", hk.canonical()),
        })
    } else {
        Ok(OsHotkeyStatus {
            backend: "Hyprland".to_string(),
            registered: false,
            message: "Removed SaveVault overlay hotkey from Hyprland.".to_string(),
        })
    }
}

fn sync_sway_hotkey(
    enabled: bool,
    hotkey: Option<&ParsedOverlayHotkey>,
    command: &str,
) -> Result<OsHotkeyStatus> {
    if enabled {
        let hk = hotkey.context("Missing overlay hotkey")?;
        let sway_bind = hk.to_sway_bind();
        let _ = Command::new("swaymsg")
            .arg(format!("unbindsym {sway_bind}"))
            .status();
        let out = Command::new("swaymsg")
            .arg(format!("bindsym {sway_bind} exec {command}"))
            .output()
            .context("Failed to execute swaymsg")?;
        if !out.status.success() {
            bail!(
                "swaymsg failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let _ = Command::new("swaymsg")
            .arg("for_window [app_id=\"savevault-overlay\"] floating enable")
            .status();
        Ok(OsHotkeyStatus {
            backend: "Sway".to_string(),
            registered: true,
            message: format!("Registered OS global hotkey {} with Sway.", hk.canonical()),
        })
    } else {
        if let Some(hk) = hotkey {
            let _ = Command::new("swaymsg")
                .arg(format!("unbindsym {}", hk.to_sway_bind()))
                .status();
        }
        Ok(OsHotkeyStatus {
            backend: "Sway".to_string(),
            registered: false,
            message: "Removed SaveVault overlay hotkey from Sway.".to_string(),
        })
    }
}

fn sync_xfce_hotkey(
    enabled: bool,
    hotkey: Option<&ParsedOverlayHotkey>,
    command: &str,
) -> Result<OsHotkeyStatus> {
    let Some(hk) = hotkey else {
        return Ok(OsHotkeyStatus {
            backend: "XFCE".to_string(),
            registered: false,
            message: "Overlay hotkey disabled.".to_string(),
        });
    };
    let prop = format!("/commands/custom/{}", hk.to_xfce_shortcut());
    if enabled {
        let out = Command::new("xfconf-query")
            .args([
                "-c",
                "xfce4-keyboard-shortcuts",
                "-p",
                &prop,
                "-n",
                "-t",
                "string",
                "-s",
                command,
            ])
            .output()
            .context("Failed to execute xfconf-query")?;
        if !out.status.success() {
            bail!(
                "xfconf-query failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(OsHotkeyStatus {
            backend: "XFCE".to_string(),
            registered: true,
            message: format!("Registered OS global hotkey {} with XFCE.", hk.canonical()),
        })
    } else {
        let _ = Command::new("xfconf-query")
            .args(["-c", "xfce4-keyboard-shortcuts", "-p", &prop, "-r"])
            .status();
        Ok(OsHotkeyStatus {
            backend: "XFCE".to_string(),
            registered: false,
            message: "Removed SaveVault overlay hotkey from XFCE.".to_string(),
        })
    }
}

/// Registers or unregisters the overlay toggle shortcut directly with the user's operating system /
/// desktop environment so SaveVault never has to monitor keyboard input in the background.
pub fn sync_os_overlay_hotkey(enabled: bool, raw_hotkey: &str) -> Result<OsHotkeyStatus> {
    let parsed = parse_overlay_hotkey(raw_hotkey);
    if enabled && parsed.is_none() {
        bail!("Invalid overlay hotkey combination: '{raw_hotkey}'");
    }
    let command = resolve_overlay_toggle_command()?;

    if Platform::current() != Platform::Linux {
        return Ok(OsHotkeyStatus {
            backend: Platform::current().display_name().to_string(),
            registered: false,
            message: if enabled {
                format!(
                    "Bind {} to {command} in your OS shortcut settings.",
                    parsed
                        .as_ref()
                        .map(ParsedOverlayHotkey::canonical)
                        .unwrap_or_else(|| raw_hotkey.to_string())
                )
            } else {
                "Overlay hotkey disabled.".to_string()
            },
        });
    }

    let desktop = desktop_env_lower();
    let cosmic_dir = dirs::config_dir().map(|d| d.join("cosmic"));

    if desktop.contains("cosmic") {
        if let Some(dir) = cosmic_dir.as_deref() {
            return sync_cosmic_hotkey(dir, enabled, parsed.as_ref(), &command);
        }
    } else if desktop.contains("gnome")
        || desktop.contains("cinnamon")
        || desktop.contains("budgie")
        || desktop.contains("unity")
    {
        return sync_gnome_hotkey(enabled, parsed.as_ref(), &command);
    } else if desktop.contains("kde") || desktop.contains("plasma") {
        return sync_kde_hotkey(enabled, parsed.as_ref(), &command);
    } else if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        return sync_hyprland_hotkey(enabled, parsed.as_ref(), &command);
    } else if std::env::var_os("SWAYSOCK").is_some() {
        return sync_sway_hotkey(enabled, parsed.as_ref(), &command);
    } else if desktop.contains("xfce") {
        return sync_xfce_hotkey(enabled, parsed.as_ref(), &command);
    } else if let Some(dir) = cosmic_dir.as_deref().filter(|d| d.is_dir()) {
        return sync_cosmic_hotkey(dir, enabled, parsed.as_ref(), &command);
    }

    Ok(OsHotkeyStatus {
        backend: "Manual / Generic".to_string(),
        registered: false,
        message: if enabled {
            format!(
                "Could not detect a supported desktop shortcut manager; bind {} to {command} in your system shortcuts.",
                parsed
                    .as_ref()
                    .map(ParsedOverlayHotkey::canonical)
                    .unwrap_or_else(|| raw_hotkey.to_string())
            )
        } else {
            "Overlay hotkey disabled.".to_string()
        },
    })
}

/// Returns a human-readable summary of the overlay toggle state and OS shortcut integration.
pub fn describe_os_overlay_status(config: &AppConfig) -> String {
    let desktop = desktop_env_lower();
    let backend = if Platform::current() != Platform::Linux {
        Platform::current().display_name()
    } else if desktop.contains("cosmic")
        || dirs::config_dir()
            .map(|d| d.join("cosmic").is_dir())
            .unwrap_or(false)
    {
        "COSMIC Desktop"
    } else if desktop.contains("gnome")
        || desktop.contains("cinnamon")
        || desktop.contains("budgie")
        || desktop.contains("unity")
    {
        "GNOME"
    } else if desktop.contains("kde") || desktop.contains("plasma") {
        "KDE Plasma"
    } else if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        "Hyprland"
    } else if std::env::var_os("SWAYSOCK").is_some() {
        "Sway"
    } else if desktop.contains("xfce") {
        "XFCE"
    } else {
        "Desktop Environment"
    };

    if config.overlay_enabled {
        format!(
            "on (hotkey: {}, handled by {backend})",
            config.overlay_hotkey
        )
    } else {
        format!("off (default, hotkey: {})", config.overlay_hotkey)
    }
}

/// Returns the IPC Unix socket path used to toggle an open overlay window off cleanly.
pub fn overlay_socket_path() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(crate::config::default_data_dir)
        .join("savevault-overlay.sock")
}

/// Local IPC listener run by the overlay window so `--toggle-overlay` can close it cleanly.
pub struct OverlayIpcServer {
    close_requested: Arc<AtomicBool>,
    #[cfg(unix)]
    socket_path: PathBuf,
    #[cfg(unix)]
    stop: Arc<AtomicBool>,
}

impl OverlayIpcServer {
    /// Starts the overlay IPC server at `overlay_socket_path()`.
    pub fn start(wake: Box<dyn Fn() + Send + Sync + 'static>) -> Option<Self> {
        Self::start_at(overlay_socket_path(), wake)
    }

    /// Starts the overlay IPC server bound to a specific socket path (used by tests).
    #[cfg(unix)]
    pub fn start_at(
        socket_path: PathBuf,
        wake: Box<dyn Fn() + Send + Sync + 'static>,
    ) -> Option<Self> {
        if let Some(parent) = socket_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).ok()?;
        let close_requested = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));

        let close_flag = close_requested.clone();
        let stop_flag = stop.clone();
        let _ = std::thread::Builder::new()
            .name("savevault-overlay-ipc".to_string())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stop_flag.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut stream) = stream else {
                        continue;
                    };
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                    let mut buf = [0u8; 64];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let msg = std::str::from_utf8(&buf[..n]).unwrap_or("").trim();
                    if msg.starts_with("toggle") || msg.starts_with("close") {
                        let _ = stream.write_all(b"closing\n");
                        let _ = stream.flush();
                        close_flag.store(true, Ordering::SeqCst);
                        wake();
                        break;
                    } else if msg.starts_with("ping") {
                        let _ = stream.write_all(b"pong\n");
                        let _ = stream.flush();
                    }
                }
            });

        Some(Self {
            close_requested,
            socket_path,
            stop,
        })
    }

    #[cfg(not(unix))]
    pub fn start_at(
        _socket_path: PathBuf,
        _wake: Box<dyn Fn() + Send + Sync + 'static>,
    ) -> Option<Self> {
        None
    }

    /// True if another `--toggle-overlay` invocation asked this overlay instance to close.
    pub fn close_requested(&self) -> bool {
        self.close_requested.load(Ordering::SeqCst)
    }
}

impl Drop for OverlayIpcServer {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            self.stop.store(true, Ordering::SeqCst);
            // Wake a blocking accept() call if the IPC thread has not exited yet.
            let _ = std::os::unix::net::UnixStream::connect(&self.socket_path);
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

/// Sends a toggle/close signal to a running overlay instance at `socket_path`.
#[cfg(unix)]
pub fn send_overlay_ipc_toggle_at(socket_path: &Path) -> bool {
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket_path) else {
        return false;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    if stream.write_all(b"toggle\n").is_err() {
        return false;
    }
    let _ = stream.flush();
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).unwrap_or(0);
    std::str::from_utf8(&buf[..n])
        .unwrap_or("")
        .trim()
        .starts_with("closing")
}

#[cfg(not(unix))]
pub fn send_overlay_ipc_toggle_at(_socket_path: &Path) -> bool {
    false
}

/// Sends a toggle/close signal to the currently open SaveVault overlay window, if any.
pub fn send_overlay_ipc_toggle() -> bool {
    send_overlay_ipc_toggle_at(&overlay_socket_path())
}

/// Launches the SaveVault screen overlay as a detached process (`--overlay`).
pub fn spawn_overlay_process() -> Result<u32> {
    let exe = resolve_launch_exe()?;
    let child = Command::new(&exe)
        .arg("--overlay")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to spawn overlay process from {}", exe.display()))?;
    Ok(child.id())
}

/// Toggles the SaveVault screen overlay: closes an open overlay if running, otherwise launches one.
/// Returns `Ok(true)` if the overlay was opened, or `Ok(false)` if an open overlay was closed.
pub fn toggle_overlay_command() -> Result<bool> {
    if send_overlay_ipc_toggle() {
        println!("Closed SaveVault overlay.");
        Ok(false)
    } else {
        spawn_overlay_process()?;
        println!("Opened SaveVault overlay.");
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use tempfile::TempDir;

    #[test]
    fn parses_and_normalizes_overlay_hotkeys() {
        assert_eq!(
            normalize_overlay_hotkey("LAlt+LShift+S").as_deref(),
            Some("LAlt+LShift+S")
        );
        assert_eq!(
            normalize_overlay_hotkey("ctrl + shift + f10").as_deref(),
            Some("LCtrl+LShift+F10")
        );
        assert_eq!(
            normalize_overlay_hotkey("Super+Grave").as_deref(),
            Some("Super+Grave")
        );
        assert_eq!(normalize_overlay_hotkey("F12").as_deref(), Some("F12"));

        assert_eq!(parse_overlay_hotkey(""), None);
        assert_eq!(parse_overlay_hotkey("LAlt+LShift"), None);
        assert_eq!(parse_overlay_hotkey("MouseLeft"), None);
        assert_eq!(parse_overlay_hotkey("LAlt+A+B"), None);
    }

    #[test]
    fn translates_hotkeys_to_os_formats() {
        let hk = parse_overlay_hotkey("LAlt+LShift+S").expect("valid hotkey");
        assert_eq!(hk.to_cosmic_modifiers(), vec!["Alt", "Shift"]);
        assert_eq!(hk.to_xkb_keysym(), "s");
        assert_eq!(hk.to_gnome_accelerator(), "<Alt><Shift>s");
        assert_eq!(hk.to_kde_shortcut(), "Alt+Shift+S");
        assert_eq!(hk.to_hyprland_bind(), "ALT SHIFT, s");
        assert_eq!(hk.to_sway_bind(), "Alt+Shift+s");

        let super_hk = parse_overlay_hotkey("Super+LCtrl+F10").expect("valid hotkey");
        assert_eq!(super_hk.to_cosmic_modifiers(), vec!["Super", "Ctrl"]);
        assert_eq!(super_hk.to_xkb_keysym(), "F10");
        assert_eq!(super_hk.to_gnome_accelerator(), "<Super><Control>F10");
        assert_eq!(super_hk.to_kde_shortcut(), "Meta+Ctrl+F10");
        assert_eq!(super_hk.to_hyprland_bind(), "SUPER CTRL, F10");
        assert_eq!(super_hk.to_sway_bind(), "Mod4+Ctrl+F10");
    }

    #[test]
    fn cosmic_shortcuts_ron_preserves_existing_and_toggles_cleanly() {
        let existing = r#"{
    (
        modifiers: [
            Super,
        ],
    ): System(AppLibrary),
}
"#;
        let hk1 = parse_overlay_hotkey("LAlt+LShift+S").unwrap();
        let cmd = "\"/usr/bin/save_game_manager\" --toggle-overlay";
        let with_overlay = update_cosmic_shortcuts_ron(existing, true, Some(&hk1), cmd);

        assert!(with_overlay.contains("System(AppLibrary)"));
        assert!(with_overlay.contains("Alt,"));
        assert!(with_overlay.contains("Shift,"));
        assert!(with_overlay.contains("key: \"s\""));
        assert!(with_overlay.contains("--toggle-overlay"));

        let hk2 = parse_overlay_hotkey("LCtrl+F10").unwrap();
        let updated = update_cosmic_shortcuts_ron(&with_overlay, true, Some(&hk2), cmd);
        assert!(updated.contains("System(AppLibrary)"));
        assert!(updated.contains("Ctrl,"));
        assert!(updated.contains("key: \"F10\""));
        assert!(!updated.contains("key: \"s\""));

        let removed = update_cosmic_shortcuts_ron(&updated, false, Some(&hk2), cmd);
        assert!(removed.contains("System(AppLibrary)"));
        assert!(!removed.contains("--toggle-overlay"));
        assert!(!removed.contains("F10"));
    }

    #[test]
    fn cosmic_tiling_exceptions_ron_adds_and_removes_overlay_rule() {
        let existing = r#"[
    (
        appid: "guake",
        title: ".*",
        enabled: true,
    ),
]
"#;
        let enabled = update_cosmic_tiling_exceptions_ron(existing, true);
        assert!(enabled.contains("appid: \"guake\""));
        assert!(enabled.contains("appid: \"savevault-overlay\""));

        let enabled_twice = update_cosmic_tiling_exceptions_ron(&enabled, true);
        assert_eq!(
            enabled_twice.matches("appid: \"savevault-overlay\"").count(),
            1
        );

        let disabled = update_cosmic_tiling_exceptions_ron(&enabled_twice, false);
        assert!(disabled.contains("appid: \"guake\""));
        assert!(!disabled.contains("savevault-overlay"));
    }

    #[test]
    fn default_config_has_overlay_disabled_by_default() {
        let cfg = default_config();
        assert!(!cfg.overlay_enabled);
        assert_eq!(cfg.overlay_hotkey, "LAlt+LShift+S");
        assert!((cfg.overlay_opacity - 0.92).abs() < 0.001);
        assert!(describe_os_overlay_status(&cfg).starts_with("off (default"));
    }

    #[cfg(unix)]
    #[test]
    fn overlay_ipc_server_toggles_cleanly() {
        let dir = TempDir::new().unwrap();
        let sock = dir.path().join("test-overlay.sock");
        let woken = Arc::new(AtomicBool::new(false));
        let woken_cb = woken.clone();
        let server = OverlayIpcServer::start_at(
            sock.clone(),
            Box::new(move || {
                woken_cb.store(true, Ordering::SeqCst);
            }),
        )
        .expect("server started");

        assert!(!server.close_requested());
        assert!(send_overlay_ipc_toggle_at(&sock));
        assert!(server.close_requested());
        assert!(woken.load(Ordering::SeqCst));
    }
}
