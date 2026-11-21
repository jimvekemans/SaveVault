//! Cross-platform Keyboard & Mouse key definitions, profile models, and aim/combo mapping.
//!
//! Linux-specific `uinput` / `evdev` device code lives in `evdev.rs` and `bridge.rs`, which are
//! compiled only into the `retroarch-mkb-bridge` binary so the main library crate stays 100% safe.

pub mod keys;
pub mod mapping;

#[cfg(test)]
mod tests {
    use super::keys::*;
    use super::mapping::*;
    use eframe::egui::Key as EguiKey;

    #[test]
    fn unified_key_table_maps_every_ui_key_to_evdev_and_retroarch() {
        for key in [
            EguiKey::ArrowUp,
            EguiKey::ArrowDown,
            EguiKey::ArrowLeft,
            EguiKey::ArrowRight,
            EguiKey::Tab,
            EguiKey::Backspace,
            EguiKey::Enter,
            EguiKey::Space,
            EguiKey::Insert,
            EguiKey::Delete,
            EguiKey::Home,
            EguiKey::End,
            EguiKey::PageUp,
            EguiKey::PageDown,
            EguiKey::Comma,
            EguiKey::Backslash,
            EguiKey::Slash,
            EguiKey::OpenBracket,
            EguiKey::CloseBracket,
            EguiKey::Backtick,
            EguiKey::Minus,
            EguiKey::Period,
            EguiKey::Equals,
            EguiKey::Semicolon,
            EguiKey::Quote,
            EguiKey::A,
            EguiKey::Z,
            EguiKey::Num0,
            EguiKey::Num9,
            EguiKey::F1,
            EguiKey::F12,
        ] {
            let name = egui_key_to_binding_name(key)
                .unwrap_or_else(|| panic!("missing binding name for {key:?}"));
            assert!(
                binding_to_evdev_code(name).is_some(),
                "missing evdev code for {name}"
            );
            assert!(
                binding_to_retroarch_cfg(name).1.is_some(),
                "missing RetroArch cfg mapping for {name}"
            );
        }
    }

    #[test]
    fn combo_resolver_prioritizes_chords_and_suppresses_conflicts() {
        let mut p = KbmProfile::default();
        p.set_hotkey(RetroArchHotkey::FastForwardToggle, 0, "LAlt+F".to_string());
        p.set_hotkey(RetroArchHotkey::FastForwardHold, 0, "Grave".to_string());

        let mut resolver = ComboResolver::from_profile(&p);
        let lalt = binding_to_evdev_code("LAlt").unwrap();
        let f_key = binding_to_evdev_code("F").unwrap();
        let grave = binding_to_evdev_code("Grave").unwrap();

        let state_f = resolver.resolve(&[f_key].into_iter().collect());
        assert!(state_f.buttons[3]);
        assert!(!state_f.buttons[13]);

        let state_combo = resolver.resolve(&[lalt, f_key].into_iter().collect());
        assert!(state_combo.buttons[13]);
        assert!(!state_combo.buttons[3]);

        let state_post_release = resolver.resolve(&[f_key].into_iter().collect());
        assert!(!state_post_release.buttons[13]);
        assert!(!state_post_release.buttons[3]);

        let _ = resolver.resolve(&[grave].into_iter().collect());
        let state_grave = resolver.resolve(&[grave].into_iter().collect());
        assert!(state_grave.buttons[12]);

        let state_f_again = resolver.resolve(&[f_key].into_iter().collect());
        assert!(state_f_again.buttons[3]);
    }

    #[test]
    fn aim_preset_resolution_avoids_false_substring_matches() {
        assert_eq!(resolve_aim_overhaul_preset("Black (Europe)", None).id, "black_ps2");
        assert_eq!(
            resolve_aim_overhaul_preset("Tom Clancy's Splinter Cell: Blacklist", None).id,
            "console_fps_default"
        );
        assert_eq!(
            resolve_aim_overhaul_preset("Black & White", None).id,
            "console_fps_default"
        );
        assert_eq!(
            resolve_aim_overhaul_preset("Rayman Legends", None).id,
            "console_fps_default"
        );
        assert_eq!(
            resolve_aim_overhaul_preset("Friends: The One with All the Trivia", None).id,
            "console_fps_default"
        );
        assert_eq!(
            resolve_aim_overhaul_preset("Metroid Prime Hunters (USA)", None).id,
            "nds_3ds_shooter"
        );
    }

    #[test]
    fn mouse_look_math_respects_sensitivity_deadzone_and_invert_y() {
        let p = KbmProfile {
            mouse_sensitivity: 1100.0,
            mouse_dpi: 800,
            mouse_deadzone: 0.14,
            invert_y: false,
            ..Default::default()
        };
        let mut st = MouseLookState::from_profile(&p);
        let (rx, ry) = st.compute_right_stick(12.0, -4.0);
        assert!(rx > 0 && ry < 0);

        let mut inv = MouseLookState::from_profile(&KbmProfile {
            invert_y: true,
            ..p
        });
        let (rx2, ry2) = inv.compute_right_stick(12.0, -4.0);
        assert_eq!(rx2, rx);
        assert_eq!(ry2, -ry);
    }

    #[test]
    fn mouse_overhaul_proportional_speed_slow_vs_fast() {
        let p = KbmProfile {
            mouse_sensitivity: 1000.0,
            mouse_dpi: 800,
            mouse_deadzone: 0.14,
            aiming_overhaul_enabled: true,
            aiming_preset_id: Some("black_ps2".to_string()),
            ..Default::default()
        };

        let steady_rx = |dx: f64| -> i32 {
            let mut st = MouseLookState::from_profile_for_game(&p, "Black (Europe)");
            let mut last = 0;
            for _ in 0..4 {
                last = st.compute_right_stick(dx, 0.0).0;
            }
            last
        };

        let rx_slow = steady_rx(2.0);
        let rx_med = steady_rx(12.0);
        let rx_fast = steady_rx(35.0);
        assert!(
            rx_slow > 0 && rx_slow < rx_med && rx_med < rx_fast && rx_fast < 32767,
            "expected slow < med < fast < 32767, got {rx_slow}, {rx_med}, {rx_fast}"
        );
    }

    #[test]
    fn mouse_overhaul_dpi_normalization_invariance() {
        let mk_state = |dpi: u32| -> i32 {
            let p = KbmProfile {
                mouse_sensitivity: 1000.0,
                mouse_dpi: dpi,
                mouse_deadzone: 0.14,
                aiming_overhaul_enabled: true,
                aiming_preset_id: Some("black_ps2".to_string()),
                ..Default::default()
            };
            let mut st = MouseLookState::from_profile_for_game(&p, "Black (Europe)");
            let counts = 8.0 * (dpi as f64 / 800.0);
            st.compute_right_stick(counts, 0.0).0
        };

        let rx_800 = mk_state(800);
        let rx_1600 = mk_state(1600);
        let rx_3200 = mk_state(3200);
        assert_eq!(rx_800, rx_1600);
        assert_eq!(rx_1600, rx_3200);
    }

    #[test]
    fn mouse_overhaul_counters_viscous_start_and_stop() {
        let p = KbmProfile {
            mouse_sensitivity: 1000.0,
            mouse_dpi: 800,
            mouse_deadzone: 0.14,
            aiming_overhaul_enabled: true,
            aiming_preset_id: Some("black_ps2".to_string()),
            ..Default::default()
        };
        let mut st = MouseLookState::from_profile_for_game(&p, "Black (Europe)");

        let (rx_first, _) = st.compute_right_stick(10.0, 0.0);
        let (rx_steady, _) = st.compute_right_stick(10.0, 0.0);
        assert!(
            rx_first > rx_steady,
            "expected lead boost on initial step: first={rx_first}, steady={rx_steady}"
        );

        let _ = st.compute_right_stick(30.0, 0.0);
        let (rx_stop, _) = st.compute_right_stick(0.0, 0.0);
        let (rx_after, _) = st.compute_right_stick(0.0, 0.0);
        assert!(rx_stop <= 0, "expected <= 0 on stop tick, got {rx_stop}");
        assert_eq!(rx_after, 0, "expected exact 0 after stop tick");
    }

    #[test]
    fn mouse_overhaul_toggle_checkbox_switches_curves() {
        let base = KbmProfile {
            mouse_sensitivity: 1000.0,
            mouse_dpi: 800,
            mouse_deadzone: 0.14,
            aiming_overhaul_enabled: true,
            aiming_preset_id: Some("black_ps2".to_string()),
            ..Default::default()
        };
        let mut overhaul = MouseLookState::from_profile_for_game(&base, "Black (Europe)");
        assert_eq!(overhaul.preset.id, "black_ps2");
        let (rx_on, _) = overhaul.compute_right_stick(10.0, 0.0);

        let mut raw = MouseLookState::from_profile_for_game(
            &KbmProfile {
                aiming_overhaul_enabled: false,
                ..base
            },
            "Black (Europe)",
        );
        let (rx_off, _) = raw.compute_right_stick(10.0, 0.0);
        assert_ne!(rx_on, rx_off);
    }
}
