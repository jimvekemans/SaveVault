//! Pure-Rust mouse-look DPI normalization, built-in per-game aiming overhaul curves,
//! and multi-key chord resolution (shared cleanly across the desktop UI, library tests, and
//! live `retroarch-mkb-bridge` daemon).

use crate::kbm::keys::{KbmAction, KbmProfile, RetroArchHotkey, parse_combo_to_evdev_codes};
use std::collections::HashSet;

pub fn clamp_axis(val: f64) -> i32 {
    val.round().clamp(-32767.0, 32767.0) as i32
}

/// Virtual gamepad button index (`12..=27`) corresponding to `BTN_TRIGGER_HAPPY1..=16`.
pub fn hotkey_virtual_btn_index(hotkey: RetroArchHotkey) -> u16 {
    match hotkey {
        RetroArchHotkey::FastForwardHold => 12,
        RetroArchHotkey::FastForwardToggle => 13,
        RetroArchHotkey::SlowMotionHold => 14,
        RetroArchHotkey::SlowMotionToggle => 15,
        RetroArchHotkey::SaveState => 16,
        RetroArchHotkey::LoadState => 17,
        RetroArchHotkey::StateSlotPlus => 18,
        RetroArchHotkey::StateSlotMinus => 19,
        RetroArchHotkey::MenuToggle => 20,
        RetroArchHotkey::PauseToggle => 21,
        RetroArchHotkey::MouseGrabToggle => 22,
        RetroArchHotkey::Screenshot => 23,
        RetroArchHotkey::AudioMute => 24,
        RetroArchHotkey::FpsToggle => 25,
        RetroArchHotkey::ResetGame => 26,
        RetroArchHotkey::ExitEmulator => 27,
    }
}

/// Built-in console game aiming overhaul preset mapping the game's internal analog
/// right-stick deadzone, non-linear acceleration curve, and camera inertia filter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GameAimOverhaulPreset {
    pub id: &'static str,
    pub title: &'static str,
    pub engine_summary: &'static str,
    /// Game's internal horizontal right-stick deadzone (`0.0..=0.45`).
    pub internal_deadzone_x: f64,
    /// Game's internal vertical right-stick deadzone (`0.0..=0.45`).
    pub internal_deadzone_y: f64,
    /// Game's internal power-curve exponent `gamma` (`omega ~ u^gamma`).
    /// Overhaul applies `norm.powf(1.0 / gamma)` so slow-to-fast hand speeds map linearly.
    pub response_exponent: f64,
    /// Vertical-to-horizontal sensitivity multiplier for widescreen / asymmetric pitch speed.
    pub y_axis_aspect_compensation: f64,
    /// Physical mouse speed (Inches Per Second) mapped to 100% turn rate at `1000x` sensitivity.
    pub reference_max_ips: f64,
    /// Derivative feed-forward lead gain on acceleration/start to counter viscous lag.
    pub lead_pre_emphasis: f64,
    /// Single-tick active counter-brake gain on sudden high-speed stop, followed by instant zero-snap.
    pub stop_brake_gain: f64,
    /// Max excess velocity carry (in normalized tick units) preserved across fast flicks.
    pub subframe_carry_limit: f64,
}

const BUILTIN_AIM_OVERHAUL_PRESETS: &[GameAimOverhaulPreset] = &[
    GameAimOverhaulPreset {
        id: "black_ps2",
        title: "Black (PS2) — Criterion RenderWare Engine",
        engine_summary: "Counters Black's steep γ=2.15 turn acceleration, 16.5%/17.5% asymmetric deadzone, and multi-frame viscous camera lag.",
        internal_deadzone_x: 0.165,
        internal_deadzone_y: 0.175,
        response_exponent: 2.15,
        y_axis_aspect_compensation: 1.22,
        reference_max_ips: 20.0,
        lead_pre_emphasis: 0.65,
        stop_brake_gain: 0.28,
        subframe_carry_limit: 4.0,
    },
    GameAimOverhaulPreset {
        id: "ps2_timesplitters_goldeneye",
        title: "TimeSplitters / Free Radical / 007 (PS2)",
        engine_summary: "Counters Free Radical / EA 007 power acceleration (γ=1.95) and mild turn damping.",
        internal_deadzone_x: 0.15,
        internal_deadzone_y: 0.15,
        response_exponent: 1.95,
        y_axis_aspect_compensation: 1.15,
        reference_max_ips: 20.0,
        lead_pre_emphasis: 0.45,
        stop_brake_gain: 0.20,
        subframe_carry_limit: 3.5,
    },
    GameAimOverhaulPreset {
        id: "ps2_killzone_moh_cod",
        title: "Killzone / Medal of Honor / CoD (PS2)",
        engine_summary: "Counters heavy PS2 military shooter turn acceleration (γ=2.25) and heavy start/stop inertia.",
        internal_deadzone_x: 0.18,
        internal_deadzone_y: 0.19,
        response_exponent: 2.25,
        y_axis_aspect_compensation: 1.25,
        reference_max_ips: 18.0,
        lead_pre_emphasis: 0.70,
        stop_brake_gain: 0.30,
        subframe_carry_limit: 4.0,
    },
    GameAimOverhaulPreset {
        id: "nds_3ds_shooter",
        title: "NDS / 3DS Shooter & Camera Overhaul",
        engine_summary: "Counters Circle Pad / Dual-Screen FPS camera smoothing (γ=1.65) and inner deadzone.",
        internal_deadzone_x: 0.12,
        internal_deadzone_y: 0.12,
        response_exponent: 1.65,
        y_axis_aspect_compensation: 1.10,
        reference_max_ips: 22.0,
        lead_pre_emphasis: 0.40,
        stop_brake_gain: 0.15,
        subframe_carry_limit: 3.0,
    },
    GameAimOverhaulPreset {
        id: "console_fps_default",
        title: "Universal Console DualShock FPS Overhaul",
        engine_summary: "Balanced DualShock inverse response curve (γ=2.05) and anti-viscosity lead filter for unprofiled console shooters.",
        internal_deadzone_x: 0.16,
        internal_deadzone_y: 0.16,
        response_exponent: 2.05,
        y_axis_aspect_compensation: 1.18,
        reference_max_ips: 20.0,
        lead_pre_emphasis: 0.55,
        stop_brake_gain: 0.24,
        subframe_carry_limit: 3.5,
    },
];

/// Returns the built-in catalog of game aiming overhaul presets.
pub fn builtin_aim_overhaul_presets() -> &'static [GameAimOverhaulPreset] {
    BUILTIN_AIM_OVERHAUL_PRESETS
}

fn normalized_base_tokens(hint: &str) -> Vec<String> {
    let base = hint.split(['(', '[']).next().unwrap_or(hint);
    base.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

fn contains_token_phrase(tokens: &[String], phrase: &[&str]) -> bool {
    !phrase.is_empty()
        && tokens
            .windows(phrase.len())
            .any(|w| w.iter().zip(phrase.iter()).all(|(a, b)| a == b))
}

/// Resolves the matching built-in aiming overhaul preset from an optional explicit `preset_id`
/// or by whole-word matching on the game's base title.
pub fn resolve_aim_overhaul_preset(
    game_hint: &str,
    preset_id: Option<&str>,
) -> &'static GameAimOverhaulPreset {
    if let Some(id) = preset_id
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
        && let Some(found) = BUILTIN_AIM_OVERHAUL_PRESETS.iter().find(|p| p.id == id)
    {
        return found;
    }

    let tokens = normalized_base_tokens(game_hint);
    let target_id = if tokens.as_slice() == ["black"]
        || tokens.as_slice() == ["black", "ps2"]
    {
        "black_ps2"
    } else if tokens.iter().any(|t| t == "timesplitters" || t == "goldeneye")
        || contains_token_phrase(&tokens, &["agent", "under", "fire"])
        || tokens.iter().any(|t| t == "nightfire")
    {
        "ps2_timesplitters_goldeneye"
    } else if tokens.iter().any(|t| t == "killzone")
        || contains_token_phrase(&tokens, &["medal", "of", "honor"])
        || contains_token_phrase(&tokens, &["call", "of", "duty"])
    {
        "ps2_killzone_moh_cod"
    } else if contains_token_phrase(&tokens, &["metroid", "prime", "hunters"])
        || contains_token_phrase(&tokens, &["kid", "icarus"])
        || contains_token_phrase(&tokens, &["resident", "evil", "revelations"])
        || tokens.iter().any(|t| t == "dementium")
        || contains_token_phrase(&tokens, &["moon", "chronicles"])
    {
        "nds_3ds_shooter"
    } else {
        "console_fps_default"
    };

    BUILTIN_AIM_OVERHAUL_PRESETS
        .iter()
        .find(|p| p.id == target_id)
        .unwrap_or(&BUILTIN_AIM_OVERHAUL_PRESETS[BUILTIN_AIM_OVERHAUL_PRESETS.len() - 1])
}

/// Pure mathematical mouse-look + DPI normalization + game aiming overhaul mapper
/// shared by both the live daemon and unit tests.
#[derive(Debug, Clone)]
pub struct MouseLookState {
    pub sensitivity: f32,
    pub deadzone: f32,
    pub mouse_dpi: u32,
    pub invert_y: bool,
    pub aiming_overhaul_enabled: bool,
    pub preset: &'static GameAimOverhaulPreset,
    pub smooth_vx: f64,
    pub smooth_vy: f64,
    pub prev_norm_vx: f64,
    pub prev_norm_vy: f64,
    pub carry_vx: f64,
    pub carry_vy: f64,
    pub brake_armed: bool,
}

impl MouseLookState {
    pub fn from_profile(profile: &KbmProfile) -> Self {
        Self::from_profile_for_game(profile, &profile.name)
    }

    pub fn from_profile_for_game(profile: &KbmProfile, game_hint: &str) -> Self {
        let preset = resolve_aim_overhaul_preset(game_hint, profile.aiming_preset_id.as_deref());
        Self {
            sensitivity: profile.mouse_sensitivity,
            deadzone: profile.mouse_deadzone,
            mouse_dpi: profile.mouse_dpi.clamp(100, 32000),
            invert_y: profile.invert_y,
            aiming_overhaul_enabled: profile.aiming_overhaul_enabled,
            preset,
            smooth_vx: 0.0,
            smooth_vy: 0.0,
            prev_norm_vx: 0.0,
            prev_norm_vy: 0.0,
            carry_vx: 0.0,
            carry_vy: 0.0,
            brake_armed: false,
        }
    }

    pub fn update_from_profile(&mut self, profile: &KbmProfile) {
        self.update_from_profile_for_game(profile, &profile.name);
    }

    pub fn update_from_profile_for_game(&mut self, profile: &KbmProfile, game_hint: &str) {
        self.sensitivity = profile.mouse_sensitivity;
        self.deadzone = profile.mouse_deadzone;
        self.mouse_dpi = profile.mouse_dpi.clamp(100, 32000);
        self.invert_y = profile.invert_y;
        self.aiming_overhaul_enabled = profile.aiming_overhaul_enabled;
        self.preset = resolve_aim_overhaul_preset(game_hint, profile.aiming_preset_id.as_deref());
    }

    fn reset_overhaul_motion(&mut self) {
        self.prev_norm_vx = 0.0;
        self.prev_norm_vy = 0.0;
        self.carry_vx = 0.0;
        self.carry_vy = 0.0;
        self.brake_armed = false;
    }

    /// Computes `(ABS_RX, ABS_RY)` in `[-32767, 32767]` from 250 Hz tick mouse delta `(dx, dy)`.
    pub fn compute_right_stick(&mut self, dx: f64, dy: f64) -> (i32, i32) {
        const POLL_HZ: f64 = 250.0;
        let dpi = (self.mouse_dpi as f64).clamp(100.0, 32000.0);
        let y_sign = if self.invert_y { -1.0 } else { 1.0 };

        if !self.aiming_overhaul_enabled {
            self.reset_overhaul_motion();
            let dpi_scale = 800.0 / dpi;
            let sdx = dx * dpi_scale;
            let sdy = dy * dpi_scale;
            self.smooth_vx = 0.70 * sdx + 0.30 * self.smooth_vx;
            self.smooth_vy = 0.70 * sdy + 0.30 * self.smooth_vy;
            let mag = self.smooth_vx.hypot(self.smooth_vy);
            if mag < 0.08 {
                self.smooth_vx = 0.0;
                self.smooth_vy = 0.0;
                return (0, 0);
            }
            let sens = (self.sensitivity as f64).max(1.0);
            let dz = (self.deadzone as f64).clamp(0.0, 0.90);
            let norm = ((mag * sens) / 32767.0).min(1.0);
            let out_mag = (dz + (1.0 - dz) * norm).min(1.0);
            let rx = clamp_axis((self.smooth_vx / mag) * out_mag * 32767.0);
            let ry = clamp_axis(y_sign * (self.smooth_vy / mag) * out_mag * 32767.0);
            return (rx, ry);
        }

        let ips_x = (dx * POLL_HZ) / dpi;
        let ips_y = (dy * POLL_HZ) / dpi;
        let sens_mult = (self.sensitivity as f64).max(1.0) / 1000.0;
        let ref_ips = self.preset.reference_max_ips.max(1.0);
        let raw_vx = (ips_x * sens_mult) / ref_ips;
        let raw_vy = (ips_y * sens_mult * self.preset.y_axis_aspect_compensation) / ref_ips;

        if raw_vx * self.carry_vx < 0.0 {
            self.carry_vx = 0.0;
        }
        if raw_vy * self.carry_vy < 0.0 {
            self.carry_vy = 0.0;
        }

        let inv_gamma = 1.0 / self.preset.response_exponent.max(0.5);
        let dz_x = (self.deadzone as f64)
            .max(self.preset.internal_deadzone_x)
            .clamp(0.0, 0.85);
        let dz_y = (self.deadzone as f64)
            .max(self.preset.internal_deadzone_y)
            .clamp(0.0, 0.85);

        let raw_zero = dx.abs() < 1e-4 && dy.abs() < 1e-4;
        if raw_zero && self.carry_vx.hypot(self.carry_vy) <= 0.08 {
            self.carry_vx = 0.0;
            self.carry_vy = 0.0;
            self.smooth_vx = 0.0;
            self.smooth_vy = 0.0;

            let prev_mag = self.prev_norm_vx.hypot(self.prev_norm_vy);
            if self.brake_armed && prev_mag > 0.22 && self.preset.stop_brake_gain > 0.0 {
                let brake_vx = -self.prev_norm_vx * self.preset.stop_brake_gain;
                let brake_vy = -self.prev_norm_vy * self.preset.stop_brake_gain;
                let bmag_raw = brake_vx.hypot(brake_vy).max(1e-6);
                let bmag = bmag_raw.clamp(0.0, 0.28);
                let curved = bmag.powf(inv_gamma);
                let out_x = (brake_vx / bmag_raw) * (dz_x + (1.0 - dz_x) * curved);
                let out_y = (brake_vy / bmag_raw) * (dz_y + (1.0 - dz_y) * curved);
                self.prev_norm_vx = 0.0;
                self.prev_norm_vy = 0.0;
                self.brake_armed = false;
                return (
                    clamp_axis(out_x * 32767.0),
                    clamp_axis(y_sign * out_y * 32767.0),
                );
            }

            self.prev_norm_vx = 0.0;
            self.prev_norm_vy = 0.0;
            self.brake_armed = false;
            return (0, 0);
        }

        let (desired_vx, desired_vy) = if raw_zero {
            let carry_step_x = self.carry_vx.clamp(-1.0, 1.0);
            let carry_step_y = self.carry_vy.clamp(-1.0, 1.0);
            self.carry_vx -= carry_step_x;
            self.carry_vy -= carry_step_y;
            (carry_step_x, carry_step_y)
        } else {
            (raw_vx + self.carry_vx, raw_vy + self.carry_vy)
        };

        let desired_mag = desired_vx.hypot(desired_vy);
        let prev_mag = self.prev_norm_vx.hypot(self.prev_norm_vy);

        let dvx = desired_vx - self.prev_norm_vx;
        let dvy = desired_vy - self.prev_norm_vy;
        let lead_gain = if desired_mag >= prev_mag {
            self.preset.lead_pre_emphasis
        } else {
            self.preset.lead_pre_emphasis * 0.35
        };
        let mut lead_vx = desired_vx + lead_gain * dvx;
        let mut lead_vy = desired_vy + lead_gain * dvy;
        if lead_vx * desired_vx + lead_vy * desired_vy < 0.0 {
            lead_vx = desired_vx * 0.25;
            lead_vy = desired_vy * 0.25;
        }

        self.prev_norm_vx = desired_vx.clamp(-1.0, 1.0);
        self.prev_norm_vy = desired_vy.clamp(-1.0, 1.0);
        self.brake_armed = true;

        if !raw_zero {
            if desired_mag > 1.0 {
                let excess = ((desired_mag - 1.0) * 0.85).min(self.preset.subframe_carry_limit);
                self.carry_vx = (desired_vx / desired_mag) * excess;
                self.carry_vy = (desired_vy / desired_mag) * excess;
            } else {
                self.carry_vx = 0.0;
                self.carry_vy = 0.0;
            }
        }

        let lead_mag = lead_vx.hypot(lead_vy);
        if lead_mag < 1e-4 {
            return (0, 0);
        }
        let norm_mag = lead_mag.clamp(0.0, 1.0);
        let curved_mag = norm_mag.powf(inv_gamma);
        let ux = lead_vx / lead_mag;
        let uy = lead_vy / lead_mag;
        let out_x = ux * (dz_x + (1.0 - dz_x) * curved_mag);
        let out_y = uy * (dz_y + (1.0 - dz_y) * curved_mag);

        (
            clamp_axis(out_x * 32767.0),
            clamp_axis(y_sign * out_y * 32767.0),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VirtualOutputTarget {
    ButtonIndex(u16), // 0..11 = DualShock buttons, 12..27 = hotkeys
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    DpadLeft,
    DpadRight,
    DpadUp,
    DpadDown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompiledCombo {
    codes: Vec<u16>,
    target: VirtualOutputTarget,
}

/// Output state snapshot produced by `ComboResolver::resolve`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedControllerState {
    pub buttons: [bool; 28],
    pub lx: i32,
    pub ly: i32,
    pub hx: i32,
    pub hy: i32,
}

impl Default for ResolvedControllerState {
    fn default() -> Self {
        Self {
            buttons: [false; 28],
            lx: 0,
            ly: 0,
            hx: 0,
            hy: 0,
        }
    }
}

/// Evaluates single-key bindings and multi-key combos with chord priority and post-chord suppression.
#[derive(Debug, Clone, Default)]
pub struct ComboResolver {
    combos: Vec<CompiledCombo>,
    suppressed_until_release: HashSet<u16>,
}

impl ComboResolver {
    pub fn from_profile(profile: &KbmProfile) -> Self {
        let mut r = Self::default();
        r.rebuild(profile);
        r
    }

    pub fn rebuild(&mut self, profile: &KbmProfile) {
        self.combos.clear();
        self.suppressed_until_release.clear();

        for &action in KbmAction::all() {
            let target = match action {
                KbmAction::MoveLeft => VirtualOutputTarget::MoveLeft,
                KbmAction::MoveRight => VirtualOutputTarget::MoveRight,
                KbmAction::MoveUp => VirtualOutputTarget::MoveUp,
                KbmAction::MoveDown => VirtualOutputTarget::MoveDown,
                KbmAction::FireR1 => VirtualOutputTarget::ButtonIndex(5),
                KbmAction::AimL1 => VirtualOutputTarget::ButtonIndex(4),
                KbmAction::GrenadeR2 => VirtualOutputTarget::ButtonIndex(7),
                KbmAction::CrouchL2 => VirtualOutputTarget::ButtonIndex(6),
                KbmAction::CrossB => VirtualOutputTarget::ButtonIndex(0),
                KbmAction::SquareY => VirtualOutputTarget::ButtonIndex(3),
                KbmAction::TriangleX => VirtualOutputTarget::ButtonIndex(2),
                KbmAction::CircleA => VirtualOutputTarget::ButtonIndex(1),
                KbmAction::SprintL3 => VirtualOutputTarget::ButtonIndex(10),
                KbmAction::ZoomR3 => VirtualOutputTarget::ButtonIndex(11),
                KbmAction::Select => VirtualOutputTarget::ButtonIndex(8),
                KbmAction::Start => VirtualOutputTarget::ButtonIndex(9),
                KbmAction::DpadLeft => VirtualOutputTarget::DpadLeft,
                KbmAction::DpadRight => VirtualOutputTarget::DpadRight,
                KbmAction::DpadUp => VirtualOutputTarget::DpadUp,
                KbmAction::DpadDown => VirtualOutputTarget::DpadDown,
            };
            if let Some(list) = profile.bindings.get(&action) {
                for binding in list {
                    let codes = parse_combo_to_evdev_codes(binding);
                    if !codes.is_empty() {
                        self.combos.push(CompiledCombo { codes, target });
                    }
                }
            }
        }

        for &hotkey in RetroArchHotkey::all() {
            let target = VirtualOutputTarget::ButtonIndex(hotkey_virtual_btn_index(hotkey));
            if let Some(list) = profile.hotkeys.get(&hotkey) {
                for combo in list {
                    let codes = parse_combo_to_evdev_codes(combo);
                    if !codes.is_empty() {
                        self.combos.push(CompiledCombo { codes, target });
                    }
                }
            }
        }
    }

    pub fn resolve(&mut self, held: &HashSet<u16>) -> ResolvedControllerState {
        self.suppressed_until_release.retain(|c| held.contains(c));

        let mut chord_consumed = HashSet::new();
        for c in &self.combos {
            if c.codes.len() > 1 && c.codes.iter().all(|code| held.contains(code)) {
                for &code in &c.codes {
                    chord_consumed.insert(code);
                    self.suppressed_until_release.insert(code);
                }
            }
        }

        let mut state = ResolvedControllerState::default();
        let mut mv = [false; 4];
        let mut dp = [false; 4];

        for c in &self.combos {
            if !c.codes.iter().all(|code| held.contains(code)) {
                continue;
            }
            if c.codes.len() == 1
                && (chord_consumed.contains(&c.codes[0])
                    || self.suppressed_until_release.contains(&c.codes[0]))
            {
                continue;
            }
            match c.target {
                VirtualOutputTarget::ButtonIndex(idx) => {
                    if let Some(slot) = state.buttons.get_mut(idx as usize) {
                        *slot = true;
                    }
                }
                VirtualOutputTarget::MoveLeft => mv[0] = true,
                VirtualOutputTarget::MoveRight => mv[1] = true,
                VirtualOutputTarget::MoveUp => mv[2] = true,
                VirtualOutputTarget::MoveDown => mv[3] = true,
                VirtualOutputTarget::DpadLeft => dp[0] = true,
                VirtualOutputTarget::DpadRight => dp[1] = true,
                VirtualOutputTarget::DpadUp => dp[2] = true,
                VirtualOutputTarget::DpadDown => dp[3] = true,
            }
        }

        state.lx = (if mv[1] { 32767 } else { 0 }) - (if mv[0] { 32767 } else { 0 });
        state.ly = (if mv[3] { 32767 } else { 0 }) - (if mv[2] { 32767 } else { 0 });
        state.hx = (if dp[1] { 1 } else { 0 }) - (if dp[0] { 1 } else { 0 });
        state.hy = (if dp[3] { 1 } else { 0 }) - (if dp[2] { 1 } else { 0 });
        state
    }
}
