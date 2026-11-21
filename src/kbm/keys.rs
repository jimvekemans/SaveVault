//! Unified Keyboard/Mouse key definitions, profile types, and virtual controller action models.
//!
//! A single lookup table (`KEY_SPECS`) backs `egui` capture in the desktop UI, RetroArch `.cfg`
//! serialization, and Linux `evdev` keycodes so all three stay strictly consistent.

use eframe::egui::Key as EguiKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Pseudo-evdev code representing positive mouse wheel scroll inside combo resolution.
pub const PSEUDO_WHEEL_UP: u16 = 0xFF01;
/// Pseudo-evdev code representing negative mouse wheel scroll inside combo resolution.
pub const PSEUDO_WHEEL_DOWN: u16 = 0xFF02;

/// Single source of truth for a bindable keyboard key or mouse input across UI, RetroArch, and evdev.
#[derive(Debug, Clone, Copy)]
pub struct KeyBindingSpec {
    pub canonical: &'static str,
    pub aliases: &'static [&'static str],
    pub egui: Option<EguiKey>,
    pub evdev_code: u16,
    pub is_mouse_button: bool,
    pub retroarch_cfg: &'static str,
}

const fn key_spec(
    canonical: &'static str,
    aliases: &'static [&'static str],
    egui: Option<EguiKey>,
    evdev_code: u16,
    retroarch_cfg: &'static str,
) -> KeyBindingSpec {
    KeyBindingSpec {
        canonical,
        aliases,
        egui,
        evdev_code,
        is_mouse_button: false,
        retroarch_cfg,
    }
}

const fn mouse_spec(
    canonical: &'static str,
    aliases: &'static [&'static str],
    evdev_code: u16,
    retroarch_cfg: &'static str,
) -> KeyBindingSpec {
    KeyBindingSpec {
        canonical,
        aliases,
        egui: None,
        evdev_code,
        is_mouse_button: true,
        retroarch_cfg,
    }
}

/// Complete unified table of supported mouse buttons, scroll axes, modifiers, and keyboard keys.
pub const KEY_SPECS: &[KeyBindingSpec] = &[
    mouse_spec("MouseLeft", &["mbtn1"], 0x110, "1"),
    mouse_spec("MouseRight", &["mbtn2"], 0x111, "2"),
    mouse_spec("MouseMiddle", &["mbtn3"], 0x112, "3"),
    mouse_spec("Mouse4", &["mouseside"], 0x113, "4"),
    mouse_spec("Mouse5", &["mouseextra"], 0x114, "5"),
    mouse_spec("WheelUp", &["wu"], PSEUDO_WHEEL_UP, "wu"),
    mouse_spec("WheelDown", &["wd"], PSEUDO_WHEEL_DOWN, "wd"),
    key_spec("Escape", &["esc"], Some(EguiKey::Escape), 1, "escape"),
    key_spec("1", &[], Some(EguiKey::Num1), 2, "num1"),
    key_spec("2", &[], Some(EguiKey::Num2), 3, "num2"),
    key_spec("3", &[], Some(EguiKey::Num3), 4, "num3"),
    key_spec("4", &[], Some(EguiKey::Num4), 5, "num4"),
    key_spec("5", &[], Some(EguiKey::Num5), 6, "num5"),
    key_spec("6", &[], Some(EguiKey::Num6), 7, "num6"),
    key_spec("7", &[], Some(EguiKey::Num7), 8, "num7"),
    key_spec("8", &[], Some(EguiKey::Num8), 9, "num8"),
    key_spec("9", &[], Some(EguiKey::Num9), 10, "num9"),
    key_spec("0", &[], Some(EguiKey::Num0), 11, "num0"),
    key_spec("Minus", &["-"], Some(EguiKey::Minus), 12, "minus"),
    key_spec("Equal", &["=", "equals"], Some(EguiKey::Equals), 13, "equals"),
    key_spec("Backspace", &[], Some(EguiKey::Backspace), 14, "backspace"),
    key_spec("Tab", &[], Some(EguiKey::Tab), 15, "tab"),
    key_spec("Q", &[], Some(EguiKey::Q), 16, "q"),
    key_spec("W", &[], Some(EguiKey::W), 17, "w"),
    key_spec("E", &[], Some(EguiKey::E), 18, "e"),
    key_spec("R", &[], Some(EguiKey::R), 19, "r"),
    key_spec("T", &[], Some(EguiKey::T), 20, "t"),
    key_spec("Y", &[], Some(EguiKey::Y), 21, "y"),
    key_spec("U", &[], Some(EguiKey::U), 22, "u"),
    key_spec("I", &[], Some(EguiKey::I), 23, "i"),
    key_spec("O", &[], Some(EguiKey::O), 24, "o"),
    key_spec("P", &[], Some(EguiKey::P), 25, "p"),
    key_spec("LeftBrace", &["["], Some(EguiKey::OpenBracket), 26, "leftbracket"),
    key_spec("RightBrace", &["]"], Some(EguiKey::CloseBracket), 27, "rightbracket"),
    key_spec("Enter", &["return"], Some(EguiKey::Enter), 28, "enter"),
    key_spec("LCtrl", &["leftctrl"], None, 29, "lctrl"),
    key_spec("A", &[], Some(EguiKey::A), 30, "a"),
    key_spec("S", &[], Some(EguiKey::S), 31, "s"),
    key_spec("D", &[], Some(EguiKey::D), 32, "d"),
    key_spec("F", &[], Some(EguiKey::F), 33, "f"),
    key_spec("G", &[], Some(EguiKey::G), 34, "g"),
    key_spec("H", &[], Some(EguiKey::H), 35, "h"),
    key_spec("J", &[], Some(EguiKey::J), 36, "j"),
    key_spec("K", &[], Some(EguiKey::K), 37, "k"),
    key_spec("L", &[], Some(EguiKey::L), 38, "l"),
    key_spec("Semicolon", &[";"], Some(EguiKey::Semicolon), 39, "semicolon"),
    key_spec("Apostrophe", &["'"], Some(EguiKey::Quote), 40, "quote"),
    key_spec("Grave", &["`", "tilde"], Some(EguiKey::Backtick), 41, "tilde"),
    key_spec("LShift", &["leftshift"], None, 42, "shift"),
    key_spec("Backslash", &["\\"], Some(EguiKey::Backslash), 43, "backslash"),
    key_spec("Z", &[], Some(EguiKey::Z), 44, "z"),
    key_spec("X", &[], Some(EguiKey::X), 45, "x"),
    key_spec("C", &[], Some(EguiKey::C), 46, "c"),
    key_spec("V", &[], Some(EguiKey::V), 47, "v"),
    key_spec("B", &[], Some(EguiKey::B), 48, "b"),
    key_spec("N", &[], Some(EguiKey::N), 49, "n"),
    key_spec("M", &[], Some(EguiKey::M), 50, "m"),
    key_spec("Comma", &[","], Some(EguiKey::Comma), 51, "comma"),
    key_spec("Dot", &["period", "."], Some(EguiKey::Period), 52, "period"),
    key_spec("Slash", &["/"], Some(EguiKey::Slash), 53, "slash"),
    key_spec("RShift", &["rightshift"], None, 54, "rshift"),
    key_spec("LAlt", &["leftalt"], None, 56, "alt"),
    key_spec("Space", &[], Some(EguiKey::Space), 57, "space"),
    key_spec("CapsLock", &[], None, 58, "capslock"),
    key_spec("F1", &[], Some(EguiKey::F1), 59, "f1"),
    key_spec("F2", &[], Some(EguiKey::F2), 60, "f2"),
    key_spec("F3", &[], Some(EguiKey::F3), 61, "f3"),
    key_spec("F4", &[], Some(EguiKey::F4), 62, "f4"),
    key_spec("F5", &[], Some(EguiKey::F5), 63, "f5"),
    key_spec("F6", &[], Some(EguiKey::F6), 64, "f6"),
    key_spec("F7", &[], Some(EguiKey::F7), 65, "f7"),
    key_spec("F8", &[], Some(EguiKey::F8), 66, "f8"),
    key_spec("F9", &[], Some(EguiKey::F9), 67, "f9"),
    key_spec("F10", &[], Some(EguiKey::F10), 68, "f10"),
    key_spec("F11", &[], Some(EguiKey::F11), 87, "f11"),
    key_spec("F12", &[], Some(EguiKey::F12), 88, "f12"),
    key_spec("RCtrl", &["rightctrl"], None, 97, "rctrl"),
    key_spec("RAlt", &["rightalt"], None, 100, "ralt"),
    key_spec("Home", &[], Some(EguiKey::Home), 102, "home"),
    key_spec("Up", &[], Some(EguiKey::ArrowUp), 103, "up"),
    key_spec("PageUp", &[], Some(EguiKey::PageUp), 104, "pageup"),
    key_spec("Left", &[], Some(EguiKey::ArrowLeft), 105, "left"),
    key_spec("Right", &[], Some(EguiKey::ArrowRight), 106, "right"),
    key_spec("End", &[], Some(EguiKey::End), 107, "end"),
    key_spec("Down", &[], Some(EguiKey::ArrowDown), 108, "down"),
    key_spec("PageDown", &[], Some(EguiKey::PageDown), 109, "pagedown"),
    key_spec("Insert", &[], Some(EguiKey::Insert), 110, "insert"),
    key_spec("Delete", &["del"], Some(EguiKey::Delete), 111, "del"),
];

/// Looks up a key specification by canonical name or case-insensitive alias.
pub fn lookup_key_spec(name: &str) -> Option<&'static KeyBindingSpec> {
    let norm = name.trim().to_ascii_lowercase();
    KEY_SPECS.iter().find(|spec| {
        spec.canonical.to_ascii_lowercase() == norm
            || spec.aliases.iter().any(|a| *a == norm)
    })
}

/// Converts a human-readable binding name (e.g. `"W"`, `"MouseLeft"`, `"LCtrl"`) to a Linux evdev code.
pub fn binding_to_evdev_code(name: &str) -> Option<u16> {
    lookup_key_spec(name).map(|s| s.evdev_code)
}

/// Converts a human-readable binding name to RetroArch `(is_mouse_button, cfg_value)`.
pub fn binding_to_retroarch_cfg(name: &str) -> (bool, Option<&'static str>) {
    match lookup_key_spec(name) {
        Some(spec) => (spec.is_mouse_button, Some(spec.retroarch_cfg)),
        None => (false, None),
    }
}

/// Converts an `egui::Key` pressed during UI key capture into the canonical binding name.
pub fn egui_key_to_binding_name(key: EguiKey) -> Option<&'static str> {
    KEY_SPECS
        .iter()
        .find(|spec| spec.egui == Some(key))
        .map(|spec| spec.canonical)
}

/// Parses a single binding (`"W"`) or `+`-separated combo (`"LAlt+F"`) into deduplicated Linux evdev codes.
pub fn parse_combo_to_evdev_codes(combo: &str) -> Vec<u16> {
    let mut codes = Vec::new();
    for part in combo.split('+') {
        let Some(code) = binding_to_evdev_code(part) else {
            return Vec::new();
        };
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes
}

/// Virtual RetroPad / gamepad actions configurable for Mouse & Keyboard mapping.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KbmAction {
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    FireR1,
    AimL1,
    GrenadeR2,
    CrouchL2,
    CrossB,
    SquareY,
    TriangleX,
    CircleA,
    SprintL3,
    ZoomR3,
    Select,
    Start,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
}

impl KbmAction {
    /// Canonical list of all gamepad actions in UI display order.
    pub fn all() -> &'static [KbmAction] {
        &[
            KbmAction::MoveUp,
            KbmAction::MoveDown,
            KbmAction::MoveLeft,
            KbmAction::MoveRight,
            KbmAction::FireR1,
            KbmAction::AimL1,
            KbmAction::GrenadeR2,
            KbmAction::CrouchL2,
            KbmAction::CrossB,
            KbmAction::SquareY,
            KbmAction::TriangleX,
            KbmAction::CircleA,
            KbmAction::SprintL3,
            KbmAction::ZoomR3,
            KbmAction::Select,
            KbmAction::Start,
            KbmAction::DpadUp,
            KbmAction::DpadDown,
            KbmAction::DpadLeft,
            KbmAction::DpadRight,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            KbmAction::MoveUp => "Left Stick Up",
            KbmAction::MoveDown => "Left Stick Down",
            KbmAction::MoveLeft => "Left Stick Left",
            KbmAction::MoveRight => "Left Stick Right",
            KbmAction::FireR1 => "R1 / RB (Right Shoulder)",
            KbmAction::AimL1 => "L1 / LB (Left Shoulder)",
            KbmAction::GrenadeR2 => "R2 / RT (Right Trigger)",
            KbmAction::CrouchL2 => "L2 / LT (Left Trigger)",
            KbmAction::CrossB => "South Button (Cross / B)",
            KbmAction::SquareY => "West Button (Square / Y)",
            KbmAction::TriangleX => "North Button (Triangle / X)",
            KbmAction::CircleA => "East Button (Circle / A)",
            KbmAction::SprintL3 => "L3 (Left Stick Click)",
            KbmAction::ZoomR3 => "R3 (Right Stick Click)",
            KbmAction::Select => "Select / Back",
            KbmAction::Start => "Start / Options",
            KbmAction::DpadUp => "D-Pad Up",
            KbmAction::DpadDown => "D-Pad Down",
            KbmAction::DpadLeft => "D-Pad Left",
            KbmAction::DpadRight => "D-Pad Right",
        }
    }

    pub fn category(&self) -> &'static str {
        match self {
            KbmAction::MoveUp
            | KbmAction::MoveDown
            | KbmAction::MoveLeft
            | KbmAction::MoveRight => "Movement (Left Analog Stick)",
            KbmAction::FireR1
            | KbmAction::AimL1
            | KbmAction::GrenadeR2
            | KbmAction::CrouchL2
            | KbmAction::CrossB
            | KbmAction::SquareY
            | KbmAction::TriangleX
            | KbmAction::CircleA
            | KbmAction::SprintL3
            | KbmAction::ZoomR3 => "Shoulders, Triggers & Face Buttons",
            KbmAction::Select
            | KbmAction::Start
            | KbmAction::DpadUp
            | KbmAction::DpadDown
            | KbmAction::DpadLeft
            | KbmAction::DpadRight => "Menu & D-Pad",
        }
    }
}

/// Emulator hotkey functions triggerable via single keys or multi-key combos.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RetroArchHotkey {
    FastForwardHold,
    FastForwardToggle,
    SlowMotionHold,
    SlowMotionToggle,
    SaveState,
    LoadState,
    StateSlotPlus,
    StateSlotMinus,
    MenuToggle,
    PauseToggle,
    MouseGrabToggle,
    Screenshot,
    AudioMute,
    FpsToggle,
    ResetGame,
    ExitEmulator,
}

impl RetroArchHotkey {
    /// Canonical list of all emulator hotkeys in UI display order.
    pub fn all() -> &'static [RetroArchHotkey] {
        &[
            RetroArchHotkey::FastForwardHold,
            RetroArchHotkey::FastForwardToggle,
            RetroArchHotkey::SlowMotionHold,
            RetroArchHotkey::SlowMotionToggle,
            RetroArchHotkey::SaveState,
            RetroArchHotkey::LoadState,
            RetroArchHotkey::StateSlotPlus,
            RetroArchHotkey::StateSlotMinus,
            RetroArchHotkey::MenuToggle,
            RetroArchHotkey::PauseToggle,
            RetroArchHotkey::MouseGrabToggle,
            RetroArchHotkey::Screenshot,
            RetroArchHotkey::AudioMute,
            RetroArchHotkey::FpsToggle,
            RetroArchHotkey::ResetGame,
            RetroArchHotkey::ExitEmulator,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            RetroArchHotkey::FastForwardHold => "Hold Fast-Forward",
            RetroArchHotkey::FastForwardToggle => "Toggle Fast-Forward",
            RetroArchHotkey::SlowMotionHold => "Hold Slow-Motion",
            RetroArchHotkey::SlowMotionToggle => "Toggle Slow-Motion",
            RetroArchHotkey::SaveState => "Save State",
            RetroArchHotkey::LoadState => "Load State",
            RetroArchHotkey::StateSlotPlus => "Next State Slot (+)",
            RetroArchHotkey::StateSlotMinus => "Prev State Slot (-)",
            RetroArchHotkey::MenuToggle => "Quick Menu Toggle",
            RetroArchHotkey::PauseToggle => "Pause / Resume Game",
            RetroArchHotkey::MouseGrabToggle => "Toggle Mouse Grab",
            RetroArchHotkey::Screenshot => "Take Screenshot",
            RetroArchHotkey::AudioMute => "Mute / Unmute Audio",
            RetroArchHotkey::FpsToggle => "Show / Hide FPS",
            RetroArchHotkey::ResetGame => "Reset Game",
            RetroArchHotkey::ExitEmulator => "Quit RetroArch",
        }
    }

    pub fn category(&self) -> &'static str {
        match self {
            RetroArchHotkey::FastForwardHold
            | RetroArchHotkey::FastForwardToggle
            | RetroArchHotkey::SlowMotionHold
            | RetroArchHotkey::SlowMotionToggle => "Speed & Frame Control",
            RetroArchHotkey::SaveState
            | RetroArchHotkey::LoadState
            | RetroArchHotkey::StateSlotPlus
            | RetroArchHotkey::StateSlotMinus => "Save States & Slots",
            RetroArchHotkey::MenuToggle
            | RetroArchHotkey::PauseToggle
            | RetroArchHotkey::MouseGrabToggle
            | RetroArchHotkey::Screenshot
            | RetroArchHotkey::AudioMute
            | RetroArchHotkey::FpsToggle
            | RetroArchHotkey::ResetGame
            | RetroArchHotkey::ExitEmulator => "Emulator & Overlay",
        }
    }
}

fn default_kbm_profile_name() -> String {
    "Standard Controller (WASD + Mouse Look)".to_string()
}

pub fn default_mouse_sensitivity() -> f32 {
    1100.0
}

pub fn default_mouse_dpi() -> u32 {
    800
}

pub fn default_mouse_deadzone() -> f32 {
    0.14
}

fn default_true() -> bool {
    true
}

pub fn default_kbm_bindings() -> BTreeMap<KbmAction, Vec<String>> {
    use KbmAction::*;
    BTreeMap::from([
        (MoveUp, vec!["W".to_string()]),
        (MoveDown, vec!["S".to_string()]),
        (MoveLeft, vec!["A".to_string()]),
        (MoveRight, vec!["D".to_string()]),
        (FireR1, vec!["MouseLeft".to_string()]),
        (AimL1, vec!["MouseRight".to_string()]),
        (GrenadeR2, vec!["MouseMiddle".to_string(), "G".to_string()]),
        (CrouchL2, vec!["LCtrl".to_string(), "C".to_string()]),
        (CrossB, vec!["R".to_string()]),
        (SquareY, vec!["F".to_string(), "WheelUp".to_string()]),
        (TriangleX, vec!["E".to_string()]),
        (CircleA, vec!["Q".to_string(), "WheelDown".to_string()]),
        (SprintL3, vec!["LShift".to_string()]),
        (ZoomR3, vec!["Space".to_string()]),
        (Select, vec!["Tab".to_string()]),
        (Start, vec!["Enter".to_string()]),
        (DpadUp, vec!["Up".to_string(), "2".to_string()]),
        (DpadDown, vec!["Down".to_string(), "4".to_string()]),
        (DpadLeft, vec!["Left".to_string(), "1".to_string()]),
        (DpadRight, vec!["Right".to_string(), "3".to_string()]),
    ])
}

pub fn default_kbm_hotkeys() -> BTreeMap<RetroArchHotkey, Vec<String>> {
    use RetroArchHotkey::*;
    BTreeMap::from([
        (FastForwardHold, vec!["Grave".to_string()]),
        (FastForwardToggle, vec!["LAlt+F".to_string()]),
        (SlowMotionHold, vec!["LAlt+S".to_string()]),
        (SlowMotionToggle, Vec::new()),
        (SaveState, vec!["F2".to_string(), "LAlt+F5".to_string()]),
        (LoadState, vec!["F4".to_string(), "LAlt+F9".to_string()]),
        (StateSlotPlus, vec!["F7".to_string()]),
        (StateSlotMinus, vec!["F6".to_string()]),
        (MenuToggle, vec!["F1".to_string()]),
        (PauseToggle, vec!["LAlt+P".to_string()]),
        (MouseGrabToggle, vec!["F11".to_string()]),
        (Screenshot, vec!["F8".to_string()]),
        (AudioMute, vec!["F9".to_string()]),
        (FpsToggle, vec!["F3".to_string()]),
        (ResetGame, vec!["LAlt+Backspace".to_string()]),
        (ExitEmulator, vec!["Escape".to_string()]),
    ])
}

/// Mouse & Keyboard override configuration (stored globally as a template and per RetroArch game).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KbmProfile {
    #[serde(default = "default_kbm_profile_name")]
    pub name: String,
    #[serde(default = "default_mouse_sensitivity")]
    pub mouse_sensitivity: f32,
    #[serde(default = "default_mouse_dpi")]
    pub mouse_dpi: u32,
    #[serde(default = "default_mouse_deadzone")]
    pub mouse_deadzone: f32,
    #[serde(default = "default_true")]
    pub aiming_overhaul_enabled: bool,
    /// Optional explicit internal overhaul preset id (`None` or `"auto"` auto-matches the game).
    #[serde(default)]
    pub aiming_preset_id: Option<String>,
    #[serde(default)]
    pub invert_y: bool,
    #[serde(default = "default_true")]
    pub auto_mouse_grab: bool,
    #[serde(default = "default_kbm_bindings")]
    pub bindings: BTreeMap<KbmAction, Vec<String>>,
    #[serde(default = "default_kbm_hotkeys")]
    pub hotkeys: BTreeMap<RetroArchHotkey, Vec<String>>,
}

impl Default for KbmProfile {
    fn default() -> Self {
        Self {
            name: default_kbm_profile_name(),
            mouse_sensitivity: default_mouse_sensitivity(),
            mouse_dpi: default_mouse_dpi(),
            mouse_deadzone: default_mouse_deadzone(),
            aiming_overhaul_enabled: true,
            aiming_preset_id: None,
            invert_y: false,
            auto_mouse_grab: true,
            bindings: default_kbm_bindings(),
            hotkeys: default_kbm_hotkeys(),
        }
    }
}

fn set_slot_in_map<K: Ord>(map: &mut BTreeMap<K, Vec<String>>, key: K, slot: usize, value: String) {
    let trimmed = value.trim().to_string();
    let entry = map.entry(key).or_default();
    if trimmed.is_empty() {
        if slot < entry.len() {
            entry.remove(slot);
        }
    } else {
        while entry.len() <= slot {
            entry.push(String::new());
        }
        entry[slot] = trimmed;
        entry.retain(|s| !s.is_empty());
    }
}

impl KbmProfile {
    /// Returns the primary or secondary binding string for `action`.
    pub fn binding(&self, action: KbmAction, slot: usize) -> &str {
        self.bindings
            .get(&action)
            .and_then(|v| v.get(slot))
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Sets or clears a primary (`slot == 0`) or secondary (`slot == 1`) binding for `action`.
    pub fn set_binding(&mut self, action: KbmAction, slot: usize, value: String) {
        set_slot_in_map(&mut self.bindings, action, slot, value);
    }

    /// Returns the primary or secondary combo string for `hotkey`.
    pub fn hotkey(&self, hotkey: RetroArchHotkey, slot: usize) -> &str {
        self.hotkeys
            .get(&hotkey)
            .and_then(|v| v.get(slot))
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Sets or clears a primary (`slot == 0`) or secondary (`slot == 1`) combo for `hotkey`.
    pub fn set_hotkey(&mut self, hotkey: RetroArchHotkey, slot: usize, value: String) {
        set_slot_in_map(&mut self.hotkeys, hotkey, slot, value);
    }
}

/// Per-game RetroArch launch parameters and KBM override settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RetroArchGameMeta {
    pub rom_path: PathBuf,
    pub core_path: PathBuf,
    /// RetroArch internal core library directory name (e.g. `ParaLLEl PS2`, `melonDS DS`, `Azahar`).
    pub core_name: String,
    #[serde(default)]
    pub playlist_name: Option<String>,
    #[serde(default)]
    pub system_name: Option<String>,
    #[serde(default)]
    pub kbm_enabled: bool,
    #[serde(default)]
    pub kbm_profile: KbmProfile,
    #[serde(default)]
    pub kbm_customized: bool,
}
