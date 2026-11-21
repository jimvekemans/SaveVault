//! Shared UI component for editing RetroArch Mouse & Keyboard (KBM) profiles,
//! multi-key combos, and RetroArch emulator hotkeys (used both per-game in `Controls & KBM`
//! and globally in `Settings -> Default KBM Override`).

use crate::kbm::keys::{KbmAction, KbmProfile, RetroArchHotkey, egui_key_to_binding_name};
use crate::kbm::mapping::{builtin_aim_overhaul_presets, resolve_aim_overhaul_preset};
use crate::theme::{JizihubPalette, glass_frame_2, render_badge};
use eframe::egui;

#[derive(Debug, Clone, Copy, Default)]
pub struct KbmEditorOutcome {
    pub changed: bool,
    pub commit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KbmRebindScope {
    Game(String),
    DefaultTemplate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbmRebindTarget {
    Action(KbmAction),
    Hotkey(RetroArchHotkey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KbmRebindState {
    pub scope: KbmRebindScope,
    pub target: KbmRebindTarget,
    pub slot: usize,
    pub armed_frame: u8,
    pub pending_solo_modifier: Option<&'static str>,
}

const ACTION_QUICK_PICKS: &[&str] = &[
    "MouseLeft",
    "MouseRight",
    "MouseMiddle",
    "Mouse4",
    "Mouse5",
    "WheelUp",
    "WheelDown",
    "LShift",
    "RShift",
    "LCtrl",
    "RCtrl",
    "LAlt",
    "Space",
    "Tab",
    "Enter",
];

const HOTKEY_QUICK_PICKS: &[&str] = &[
    "Grave",
    "LAlt+F",
    "LAlt+Space",
    "LCtrl+Space",
    "LAlt+S",
    "LAlt+P",
    "LAlt+F5",
    "LAlt+F9",
    "LAlt+Backspace",
    "F1",
    "F2",
    "F3",
    "F4",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "Escape",
];

fn format_with_modifiers(mods: &egui::Modifiers, leaf: &str) -> String {
    let mut parts = Vec::new();
    if mods.ctrl {
        parts.push("LCtrl");
    }
    if mods.alt {
        parts.push("LAlt");
    }
    if mods.shift {
        parts.push("LShift");
    }
    parts.push(leaf);
    parts.join("+")
}

/// Checks `egui` events for single keys, standalone modifiers, or multi-key combos (e.g. `LAlt+F`).
fn poll_capture_for_scope(
    ctx: &egui::Context,
    rebind: &mut Option<KbmRebindState>,
    scope: &KbmRebindScope,
) -> Option<(KbmRebindTarget, usize, String)> {
    let state = rebind.as_mut()?;
    if &state.scope != scope {
        return None;
    }
    if state.armed_frame == 0 {
        state.armed_frame = 1;
        ctx.request_repaint();
        return None;
    }

    let events = ctx.input(|i| i.events.clone());
    let mut captured: Option<String> = None;
    let mut cancelled = false;

    for ev in &events {
        match ev {
            egui::Event::Key {
                key: egui::Key::Escape,
                pressed: true,
                modifiers,
                ..
            } => {
                if !modifiers.ctrl && !modifiers.alt && !modifiers.shift {
                    cancelled = true;
                } else {
                    captured = Some(format_with_modifiers(modifiers, "Escape"));
                }
                break;
            }
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => {
                if let Some(name) = egui_key_to_binding_name(*key) {
                    captured = Some(format_with_modifiers(modifiers, name));
                    break;
                }
            }
            egui::Event::ModifiersChanged(mods) => {
                if mods.ctrl && !mods.alt && !mods.shift {
                    state.pending_solo_modifier = Some("LCtrl");
                } else if mods.alt && !mods.ctrl && !mods.shift {
                    state.pending_solo_modifier = Some("LAlt");
                } else if mods.shift && !mods.ctrl && !mods.alt {
                    state.pending_solo_modifier = Some("LShift");
                } else if !mods.ctrl && !mods.alt && !mods.shift
                    && let Some(solo) = state.pending_solo_modifier.take()
                {
                    captured = Some(solo.to_string());
                    break;
                } else {
                    // Multiple modifiers pressed together; wait for the combo key or full release.
                    state.pending_solo_modifier = None;
                }
            }
            egui::Event::PointerButton {
                button,
                pressed: true,
                modifiers,
                ..
            } => {
                let mbtn = match button {
                    egui::PointerButton::Primary => "MouseLeft",
                    egui::PointerButton::Secondary => "MouseRight",
                    egui::PointerButton::Middle => "MouseMiddle",
                    egui::PointerButton::Extra1 => "Mouse4",
                    egui::PointerButton::Extra2 => "Mouse5",
                };
                captured = Some(format_with_modifiers(modifiers, mbtn));
                break;
            }
            egui::Event::MouseWheel {
                delta, modifiers, ..
            } => {
                if delta.y > 0.05 {
                    captured = Some(format_with_modifiers(modifiers, "WheelUp"));
                    break;
                } else if delta.y < -0.05 {
                    captured = Some(format_with_modifiers(modifiers, "WheelDown"));
                    break;
                }
            }
            _ => {}
        }
    }

    if cancelled {
        *rebind = None;
        return None;
    }
    if let Some(binding) = captured {
        let target = state.target;
        let slot = state.slot;
        *rebind = None;
        return Some((target, slot, binding));
    }
    ctx.request_repaint();
    None
}

fn render_binding_row<K: Copy + std::hash::Hash + Eq + std::fmt::Debug>(
    ui: &mut egui::Ui,
    pal: &JizihubPalette,
    id_salt: &str,
    scope: &KbmRebindScope,
    target: KbmRebindTarget,
    key_id: K,
    label: &str,
    slot_values: [&str; 2],
    quick_picks: &[&str],
    rebind: &mut Option<KbmRebindState>,
) -> Option<(usize, String)> {
    let mut updated = None;
    ui.horizontal_wrapped(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(230.0, 24.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(egui::RichText::new(label).color(pal.text_2));
            },
        );

        for slot in 0..2 {
            let current_val = slot_values[slot];
            let is_capturing = rebind.as_ref().is_some_and(|r| {
                r.scope == *scope && r.target == target && r.slot == slot
            });

            let btn_text = if is_capturing {
                "Press key / combo... (Esc)".to_string()
            } else if current_val.is_empty() {
                if slot == 0 {
                    "Unbound".to_string()
                } else {
                    "+ Alt / Combo".to_string()
                }
            } else {
                current_val.to_string()
            };

            let btn = egui::Button::new(
                egui::RichText::new(btn_text).monospace().small().color(
                    if is_capturing {
                        pal.accent_contrast
                    } else if current_val.is_empty() {
                        pal.text_3
                    } else {
                        pal.text_1
                    },
                ),
            )
            .fill(if is_capturing {
                pal.accent
            } else {
                pal.glass_2
            })
            .stroke(egui::Stroke::new(
                1.0,
                if is_capturing {
                    pal.accent
                } else {
                    pal.border_strong
                },
            ))
            .min_size(egui::vec2(124.0, 22.0));

            if ui
                .add(btn)
                .on_hover_text(
                    "Click then press any key, mouse button, wheel, or modifier combo (e.g. LAlt+F)",
                )
                .clicked()
            {
                if is_capturing {
                    *rebind = None;
                } else {
                    *rebind = Some(KbmRebindState {
                        scope: scope.clone(),
                        target,
                        slot,
                        armed_frame: 0,
                        pending_solo_modifier: None,
                    });
                }
            }

            egui::ComboBox::from_id_salt((id_salt, key_id, slot, "pick"))
                .selected_text("...")
                .width(38.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(current_val.is_empty(), "(Clear)").clicked() {
                        updated = Some((slot, String::new()));
                    }
                    for &pick in quick_picks {
                        if ui.selectable_label(current_val == pick, pick).clicked() {
                            updated = Some((slot, pick.to_string()));
                        }
                    }
                });
        }
    });
    updated
}

/// Renders mouse sensitivity, DPI normalization, aiming overhaul toggle, anti-deadzone,
/// Y-inversion, auto-grab, in-game keybindings, and RetroArch hotkeys / multi-key combos
/// for `profile`. Returns `KbmEditorOutcome` distinguishing continuous slider drags (`commit: false`)
/// from completed edits (`commit: true`).
pub fn render_kbm_profile_editor(
    ui: &mut egui::Ui,
    pal: &JizihubPalette,
    id_salt: &str,
    game_hint: Option<&str>,
    scope: KbmRebindScope,
    profile: &mut KbmProfile,
    rebind: &mut Option<KbmRebindState>,
) -> KbmEditorOutcome {
    let mut outcome = KbmEditorOutcome::default();
    let pointer_down = ui.ctx().input(|i| i.pointer.any_down());

    if let Some((target, slot, binding)) = poll_capture_for_scope(ui.ctx(), rebind, &scope) {
        match target {
            KbmRebindTarget::Action(act) => profile.set_binding(act, slot, binding),
            KbmRebindTarget::Hotkey(hk) => profile.set_hotkey(hk, slot, binding),
        }
        outcome.changed = true;
        outcome.commit = true;
    }

    let hint = game_hint.unwrap_or(&profile.name);
    let active_preset = resolve_aim_overhaul_preset(hint, profile.aiming_preset_id.as_deref());

    glass_frame_2(pal).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.strong("Mouse Aim, DPI & Sensitivity");
            render_badge(ui, pal, "LIVE 250 HZ", true);
            if profile.aiming_overhaul_enabled {
                render_badge(ui, pal, "AIM OVERHAUL ON", true);
            }
        });
        ui.add_space(4.0);

        ui.horizontal_wrapped(|ui| {
            ui.label("Mouse DPI:");
            let mut dpi_i32 = profile.mouse_dpi.clamp(100, 32000) as i32;
            let resp = ui
                .add(
                    egui::DragValue::new(&mut dpi_i32)
                        .range(100..=32000)
                        .speed(25.0)
                        .suffix(" DPI"),
                )
                .on_hover_text(
                    "Set your physical mouse DPI so slow vs. fast hand speed maps to proportional camera rotation regardless of DPI.",
                );
            if resp.changed() {
                profile.mouse_dpi = dpi_i32.clamp(100, 32000) as u32;
                outcome.changed = true;
                if !pointer_down {
                    outcome.commit = true;
                }
            }
            if resp.drag_stopped() {
                outcome.commit = true;
            }
            for dpi_preset in [400_u32, 800, 1600, 3200, 6400] {
                let selected = profile.mouse_dpi == dpi_preset;
                if ui
                    .selectable_label(selected, format!("{dpi_preset}"))
                    .clicked()
                {
                    profile.mouse_dpi = dpi_preset;
                    outcome.changed = true;
                    outcome.commit = true;
                }
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().slider_width = 200.0;
            ui.label("Mouse sensitivity:");
            let resp = ui.add(
                egui::Slider::new(&mut profile.mouse_sensitivity, 100.0..=65000.0)
                    .logarithmic(true)
                    .max_decimals(0)
                    .suffix("x"),
            );
            if resp.changed() {
                outcome.changed = true;
                if !pointer_down {
                    outcome.commit = true;
                }
            }
            if resp.drag_stopped() {
                outcome.commit = true;
            }
            for (lbl, val) in [
                ("Low", 650.0_f32),
                ("Overhaul 1K", 1000.0_f32),
                ("Balanced", 1100.0_f32),
                ("High", 4000.0_f32),
                ("Ultra", 16000.0_f32),
                ("Max 65K", 65000.0_f32),
            ] {
                if ui.small_button(lbl).clicked() {
                    profile.mouse_sensitivity = val;
                    outcome.changed = true;
                    outcome.commit = true;
                }
            }
        });

        if profile.aiming_overhaul_enabled {
            let max_ips = (active_preset.reference_max_ips
                * (1000.0 / (profile.mouse_sensitivity as f64).max(1.0)))
            .max(0.1);
            let max_cms = max_ips * 2.54;
            ui.small(
                egui::RichText::new(format!(
                    "DPI-mapped range: 0 -> {:.1} IPS ({:.0} cm/s) maps cleanly across 0%..100% turn speed at {} DPI.",
                    max_ips, max_cms, profile.mouse_dpi
                ))
                .color(pal.text_2),
            );
            if profile.mouse_sensitivity > 3500.0 {
                ui.horizontal_wrapped(|ui| {
                    ui.small(
                        egui::RichText::new(
                            "Tip: High sensitivity (>3500x) compresses slow-vs-fast speed separation. Try Overhaul 1K (1000x).",
                        )
                        .color(pal.warn_fg),
                    );
                    if ui.small_button("Apply Recommended 1000x").clicked() {
                        profile.mouse_sensitivity = 1000.0;
                        profile.mouse_deadzone = 0.14;
                        outcome.changed = true;
                        outcome.commit = true;
                    }
                });
            }
        }

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().slider_width = 200.0;
            let mut dz_pct = profile.mouse_deadzone * 100.0;
            ui.label("Anti-deadzone floor:");
            let resp = ui
                .add(
                    egui::Slider::new(&mut dz_pct, 0.0..=45.0)
                        .step_by(1.0)
                        .suffix("%"),
                )
                .on_hover_text(
                    "Clears internal console analog stick deadzones so small mouse movements aim smoothly.",
                );
            if resp.changed() {
                profile.mouse_deadzone = (dz_pct / 100.0).clamp(0.0, 0.45);
                outcome.changed = true;
                if !pointer_down {
                    outcome.commit = true;
                }
            }
            if resp.drag_stopped() {
                outcome.commit = true;
            }
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        if ui
            .checkbox(
                &mut profile.aiming_overhaul_enabled,
                "Activate Game Aiming Overhaul (Counter Built-In Acceleration & Viscous Crosshair Lag)",
            )
            .on_hover_text(
                "Uses built-in game-specific inverse power curves and lead/brake pre-emphasis so slow vs. fast movement scales naturally and crosshair starts/stops crisply without manual calibration.",
            )
            .changed()
        {
            outcome.changed = true;
            outcome.commit = true;
        }

        if profile.aiming_overhaul_enabled {
            ui.horizontal_wrapped(|ui| {
                ui.label("Internal Game Profile:");
                let current_label = match profile.aiming_preset_id.as_deref() {
                    None | Some("") | Some("auto") => {
                        format!("Auto-Match ({})", active_preset.title)
                    }
                    Some(_) => active_preset.title.to_string(),
                };
                egui::ComboBox::from_id_salt((id_salt, "aim_overhaul_preset"))
                    .selected_text(current_label)
                    .show_ui(ui, |ui| {
                        let auto_selected = matches!(
                            profile.aiming_preset_id.as_deref(),
                            None | Some("") | Some("auto")
                        );
                        if ui
                            .selectable_label(auto_selected, "Auto-Detect by Game")
                            .clicked()
                        {
                            profile.aiming_preset_id = None;
                            outcome.changed = true;
                            outcome.commit = true;
                        }
                        for p in builtin_aim_overhaul_presets() {
                            let is_sel = profile.aiming_preset_id.as_deref() == Some(p.id);
                            if ui.selectable_label(is_sel, p.title).clicked() {
                                profile.aiming_preset_id = Some(p.id.to_string());
                                outcome.changed = true;
                                outcome.commit = true;
                            }
                        }
                    });
            });
            ui.small(
                egui::RichText::new(format!(
                    "{} (γ={:.2} inverse curve, DZ {:.1}%/{:.1}%, lead +{:.0}%, stop-brake {:.0}%)",
                    active_preset.engine_summary,
                    active_preset.response_exponent,
                    active_preset.internal_deadzone_x * 100.0,
                    active_preset.internal_deadzone_y * 100.0,
                    active_preset.lead_pre_emphasis * 100.0,
                    active_preset.stop_brake_gain * 100.0,
                ))
                .color(pal.text_3),
            );
        }

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .checkbox(&mut profile.invert_y, "Invert Mouse Y-Axis (Pitch)")
                .changed()
            {
                outcome.changed = true;
                outcome.commit = true;
            }
            if ui
                .checkbox(
                    &mut profile.auto_mouse_grab,
                    "Auto-grab mouse cursor in RetroArch",
                )
                .changed()
            {
                outcome.changed = true;
                outcome.commit = true;
            }
        });
    });

    ui.add_space(6.0);

    let mut current_category = "";
    for &action in KbmAction::all() {
        let cat = action.category();
        if cat != current_category {
            if !current_category.is_empty() {
                ui.add_space(4.0);
            }
            ui.strong(cat);
            current_category = cat;
        }
        let s0 = profile.binding(action, 0).to_string();
        let s1 = profile.binding(action, 1).to_string();
        if let Some((slot, val)) = render_binding_row(
            ui,
            pal,
            id_salt,
            &scope,
            KbmRebindTarget::Action(action),
            action,
            action.label(),
            [&s0, &s1],
            ACTION_QUICK_PICKS,
            rebind,
        ) {
            profile.set_binding(action, slot, val);
            outcome.changed = true;
            outcome.commit = true;
        }
    }

    ui.add_space(10.0);
    glass_frame_2(pal).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.strong("RetroArch Hotkeys & Key Combos");
            render_badge(ui, pal, "CHORD SUPPORT", true);
        });
        ui.small(
            egui::RichText::new(
                "Bind single keys (e.g. Grave/Tilde, F2) or key combos (e.g. LAlt+F, LAlt+S, LCtrl+Space). \
                 Holding a combo suppresses underlying in-game single-key actions automatically and hot-reloads live during gameplay.",
            )
            .color(pal.text_3),
        );
        ui.add_space(4.0);

        let mut hk_category = "";
        for &hk in RetroArchHotkey::all() {
            let cat = hk.category();
            if cat != hk_category {
                if !hk_category.is_empty() {
                    ui.add_space(4.0);
                }
                ui.strong(cat);
                hk_category = cat;
            }
            let s0 = profile.hotkey(hk, 0).to_string();
            let s1 = profile.hotkey(hk, 1).to_string();
            if let Some((slot, val)) = render_binding_row(
                ui,
                pal,
                id_salt,
                &scope,
                KbmRebindTarget::Hotkey(hk),
                hk,
                hk.label(),
                [&s0, &s1],
                HOTKEY_QUICK_PICKS,
                rebind,
            ) {
                profile.set_hotkey(hk, slot, val);
                outcome.changed = true;
                outcome.commit = true;
            }
        }
    });

    outcome
}
