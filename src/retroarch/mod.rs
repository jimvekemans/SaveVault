//! RetroArch integration: `retroarch.cfg`/core parsing, library scanning, Save States
//! discovery, KBM registry management, and launching.

pub mod cfg;
pub mod launch;
pub mod registry;
pub mod scan;

pub use cfg::{
    RetroArchConfig, action_retroarch_cfg_suffix, apply_kbm_profile_to_retroarch,
    core_search_directories, default_retroarch_config_dir, find_core_library,
    game_override_cfg_path, hotkey_retroarch_cfg_key, resolve_core_for_rom,
    resolve_core_for_rom_with_root,
};
pub use launch::{
    ensure_mkb_bridge_running, find_mkb_bridge_binary, is_retroarch_executable_name,
    launch_retroarch_game, split_command_words,
};
pub use registry::{
    KBM_PROFILES_FILENAME, KbmGameProfileEntry, KbmProfileRegistry, build_kbm_registry,
    default_kbm_profiles_path, kbm_profiles_path_for, load_kbm_profiles_registry,
    sync_kbm_registry_for_config, write_kbm_profiles_registry_at,
};
pub use scan::{
    detect_retroarch_roots, ensure_playlist_item, list_retroarch_save_states,
    per_rom_save_includes, scan_retroarch_paths,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use crate::models::{
        DetectionSource, GameEntry, KbmAction, KbmProfile, Provider, RetroArchGameMeta,
        RetroArchHotkey, SyncSelectionMode,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    #[test]
    fn parses_retroarch_cfg_directories_and_sort_flags() {
        let tmp = TempDir::new().unwrap();
        let ra = tmp.path().join("ra");
        fs::create_dir_all(&ra).unwrap();
        fs::write(
            ra.join("retroarch.cfg"),
            concat!(
                "libretro_directory = \":/custom_cores\"\n",
                "savefile_directory = \":/my_saves\"\n",
                "savestate_directory = \":/my_states\"\n",
                "sort_savefiles_enable = \"false\"\n",
                "sort_savefiles_by_content_enable = \"false\"\n",
                "sort_savestates_enable = \"true\"\n",
                "sort_savestates_by_content_enable = \"true\"\n",
            ),
        )
        .unwrap();

        let cfg = RetroArchConfig::load(&ra);
        assert_eq!(cfg.libretro_directory, Some(ra.join("custom_cores")));
        let rom = tmp.path().join("roms/ps1/Metal Gear.cue");
        assert_eq!(
            cfg.saves_dir_for(&ra, "Beetle PSX", &rom),
            ra.join("my_saves")
        );
        assert_eq!(
            cfg.states_dir_for(&ra, "Beetle PSX", &rom),
            ra.join("my_states/Beetle PSX/ps1")
        );
    }

    #[test]
    fn scans_playlists_all_cores_without_misclassifying_generic_isos() {
        let tmp = TempDir::new().unwrap();
        let ra = tmp.path().join("retroarch");
        let rom_ps2 = tmp.path().join("ROMs/ps2");
        let rom_ps1 = tmp.path().join("ROMs/ps1");
        let rom_nds = tmp.path().join("ROMs/nds");
        fs::create_dir_all(ra.join("playlists")).unwrap();
        fs::create_dir_all(ra.join("config/ParaLLEl PS2")).unwrap();
        fs::create_dir_all(ra.join("states/ParaLLEl PS2")).unwrap();
        fs::create_dir_all(&rom_ps2).unwrap();
        fs::create_dir_all(&rom_ps1).unwrap();
        fs::create_dir_all(&rom_nds).unwrap();

        let black_iso = rom_ps2.join("Black (Europe) (En,Fr,De,Es,It).iso");
        let blacklist_iso = rom_ps1.join("Generic Game (USA).iso");
        let ps1_cue = rom_ps1.join("Castlevania (USA).cue");
        let nds_rom = rom_nds.join("Mario Kart DS (USA).nds");
        let nds_dup = rom_nds.join("Mario Kart DS (USA) (Rev 1).nds");
        for f in [&black_iso, &blacklist_iso, &ps1_cue, &nds_rom, &nds_dup] {
            fs::write(f, b"rom").unwrap();
        }
        fs::write(
            ra.join("states/ParaLLEl PS2/Black (Europe) (En,Fr,De,Es,It).state"),
            b"state0",
        )
        .unwrap();

        ensure_playlist_item(
            &ra.join("playlists/Sony - PlayStation 2.lpl"),
            &black_iso,
            "Black (Europe)",
            Path::new("/app/cores/pcsx2_libretro.so"),
            "Sony - PlayStation 2 (LRPS2)",
            None,
        )
        .unwrap();
        ensure_playlist_item(
            &ra.join("playlists/Sony - PlayStation.lpl"),
            &ps1_cue,
            "Castlevania",
            Path::new("/app/cores/mednafen_psx_libretro.so"),
            "Sony - PlayStation (Beetle PSX)",
            None,
        )
        .unwrap();

        let games = scan_retroarch_paths(&[ra.clone(), tmp.path().join("ROMs")]);
        // Non-PS2 loose `Generic Game (USA).iso` in /ps1/ is NOT assumed PS2; playlist PS1,
        // PS2 Black, and both NDS ROMs (even with identical clean name) are kept.
        assert_eq!(games.len(), 4);
        assert!(games.iter().any(|g| g.name == "Castlevania" && g.app_id.as_deref() == Some("Beetle PSX")));

        let black = games.iter().find(|g| g.name == "Black (Europe)").unwrap();
        assert_eq!(black.provider, Provider::RetroArch);
        assert_eq!(
            black.detection.as_ref().unwrap().source,
            DetectionSource::Heuristic
        );
        // Never auto-enable KBM solely from game name "Black".
        assert!(!black.retroarch.as_ref().unwrap().kbm_enabled);
        assert_eq!(
            black.sync_selection,
            SyncSelectionMode::SelectedFiles(per_rom_save_includes("Black (Europe) (En,Fr,De,Es,It)"))
        );
        let states = list_retroarch_save_states(black);
        assert_eq!(states.len(), 1);
    }

    #[test]
    fn cfg_override_preserves_user_lines_and_restores_opt_axis_scale() {
        let tmp = TempDir::new().unwrap();
        let ra = tmp.path().join("retroarch");
        fs::create_dir_all(ra.join("config/ParaLLEl PS2")).unwrap();
        let cfg_file = ra.join("config/ParaLLEl PS2/TestGame.cfg");
        let opt_file = ra.join("config/ParaLLEl PS2/TestGame.opt");
        fs::write(
            &cfg_file,
            "video_driver = \"vulkan\"\ninput_player1_turbo = \"h\"\n",
        )
        .unwrap();
        fs::write(&opt_file, "pcsx2_axis_scale1 = \"133%\"\n").unwrap();

        let mut profile = KbmProfile {
            mouse_sensitivity: 1650.0,
            aiming_overhaul_enabled: true,
            ..Default::default()
        };
        profile.set_binding(KbmAction::CrossB, 0, "F".to_string());
        profile.set_binding(KbmAction::DpadUp, 0, "PageUp".to_string());

        let mut game = GameEntry {
            id: "retroarch:testgame".into(),
            name: "TestGame".into(),
            game_slug: "testgame".into(),
            provider: Provider::RetroArch,
            library_root: Some(ra.clone()),
            save_dir: ra.join("saves/ParaLLEl PS2"),
            retroarch: Some(RetroArchGameMeta {
                rom_path: tmp.path().join("ROMs/ps2/TestGame.iso"),
                core_path: PathBuf::from("/usr/lib/libretro/pcsx2_libretro.so"),
                core_name: "ParaLLEl PS2".into(),
                kbm_enabled: true,
                kbm_profile: profile,
                ..Default::default()
            }),
            ..Default::default()
        };

        apply_kbm_profile_to_retroarch(&game).unwrap();
        let out = fs::read_to_string(&cfg_file).unwrap();
        assert!(out.contains("video_driver = \"vulkan\""));
        assert!(out.contains("input_player1_turbo = \"h\""));
        assert!(out.contains("# --- BEGIN SAVEVAULT KBM OVERRIDE ---"));
        assert!(out.contains("input_player1_b = \"f\""));
        assert!(out.contains("input_player1_up = \"pageup\""));

        let opt_overhauled = fs::read_to_string(&opt_file).unwrap();
        assert!(opt_overhauled.contains("pcsx2_axis_scale1 = \"100%\""));

        // Disable KBM: SaveVault block stripped, user `input_player1_turbo` preserved, .opt restored!
        game.retroarch.as_mut().unwrap().kbm_enabled = false;
        apply_kbm_profile_to_retroarch(&game).unwrap();
        let after = fs::read_to_string(&cfg_file).unwrap();
        assert!(after.contains("input_player1_turbo = \"h\""));
        assert!(!after.contains("BEGIN SAVEVAULT KBM OVERRIDE"));

        let opt_restored = fs::read_to_string(&opt_file).unwrap();
        assert!(opt_restored.contains("pcsx2_axis_scale1 = \"133%\""));
        assert!(!opt_restored.contains("savevault_prev_pcsx2_axis_scale1"));

        // When no user cfg existed and KBM is off, no empty file is created.
        fs::remove_file(&cfg_file).unwrap();
        assert!(apply_kbm_profile_to_retroarch(&game).unwrap().is_none());
        assert!(!cfg_file.exists());
    }

    #[test]
    fn split_command_words_and_executable_matching() {
        assert_eq!(
            split_command_words("flatpak run org.libretro.RetroArch"),
            vec!["flatpak", "run", "org.libretro.RetroArch"]
        );
        assert_eq!(
            split_command_words("\"/opt/Retro Arch/retroarch\" --verbose"),
            vec!["/opt/Retro Arch/retroarch", "--verbose"]
        );
        assert!(is_retroarch_executable_name("/usr/bin/retroarch"));
        assert!(is_retroarch_executable_name(
            "/home/u/Apps/RetroArch-Linux-x86_64.AppImage"
        ));
        assert!(is_retroarch_executable_name("retroarch.exe"));
        assert!(!is_retroarch_executable_name("retroarch-mkb-bridge"));
        assert!(!is_retroarch_executable_name(
            "/usr/local/bin/retroarch-mkb-bridge"
        ));
    }

    #[test]
    fn registry_respects_config_path_and_returns_empty_when_missing() {
        let tmp = TempDir::new().unwrap();
        let reg_path = tmp.path().join("retroarch_kbm_profiles.json");
        let missing = load_kbm_profiles_registry(&reg_path);
        assert!(missing.games.is_empty());

        let mut cfg = default_config();
        let cfg_path = tmp.path().join("config.json");
        // No KBM games -> registry is not created unnecessarily.
        assert!(sync_kbm_registry_for_config(&cfg_path, &cfg).unwrap().is_none());
        assert!(!reg_path.exists());

        cfg.default_kbm_profile.set_hotkey(
            RetroArchHotkey::FastForwardToggle,
            0,
            "LCtrl+Space".to_string(),
        );
        assert!(sync_kbm_registry_for_config(&cfg_path, &cfg).unwrap().is_some());
        assert!(reg_path.is_file());
    }
}
