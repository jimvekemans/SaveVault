use chrono::{Duration, Utc};
use eframe::egui::Color32;
use save_game_manager::auth::{
    generate_ssh_keypair, public_key_path_for, read_public_key_for_private_key,
    remote_url_for_auth,
};
use save_game_manager::chunker::{is_manifest_file_name, sha256_file};
use save_game_manager::config::{default_config, load_config, save_config};
use save_game_manager::git_ops::list_commits_for_path;
use save_game_manager::models::{
    AppConfig, AuthMethod, GameEntry, Platform, Provider, ProviderLibraryConfig, SyncRate,
    SyncSelectionMode, ThemeMode,
};
use save_game_manager::portability::{
    ExternalImportRequest, OnlineSaveImportRequest, import_game_from_external_repo,
    import_online_save, inspect_repository, reinstate_full_structure,
};
use save_game_manager::providers::{
    default_library_paths, find_multi_provider_groups, scan_provider_libraries, slugify_game_name,
};
use save_game_manager::sync_engine::{
    REPO_METADATA_FILE, games_due_on_startup, games_due_periodic, label_for_commit,
    read_repo_metadata_manifest, repo_subpath_for_game, rollback_game_to_commit,
    set_save_state_label, sync_game_in_config, sync_game_with_label,
};
use save_game_manager::theme::palette;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, SystemTime};
use tempfile::TempDir;

fn make_test_config(root: &Path) -> AppConfig {
    let mut cfg = default_config();
    cfg.local_repo_path = root.join("repo");
    cfg.auth = AuthMethod::PersonalAccessToken {
        token: String::new(),
        github_username: String::new(),
    };
    cfg
}

fn make_game(
    provider: Provider,
    name: &str,
    save_dir: PathBuf,
    sync_across_providers: bool,
) -> GameEntry {
    let slug = slugify_game_name(name);
    GameEntry {
        id: format!("{}:{slug}", provider.slug()),
        name: name.to_string(),
        game_slug: slug,
        provider,
        app_id: None,
        install_dir: None,
        save_dir,
        enabled: true,
        sync_selection: SyncSelectionMode::EntireFolder,
        sync_rate_override: None,
        sync_across_providers,
        active_save_label: None,
        last_synced_at: None,
        last_sync_status: None,
        ..Default::default()
    }
}

fn write_with_mtime(path: &Path, bytes: &[u8], secs_since_epoch: u64) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    let f = File::options().write(true).open(path).unwrap();
    let ts = SystemTime::UNIX_EPOCH + StdDuration::from_secs(secs_since_epoch);
    f.set_modified(ts).unwrap();
}

#[test]
fn test_repo_subpath_per_provider_and_flat() {
    let steam_game = make_game(
        Provider::Steam,
        "Metro Exodus",
        PathBuf::from("/tmp/steam_saves"),
        false,
    );
    let epic_game = make_game(
        Provider::EpicGames,
        "Metro Exodus - Enhanced Edition",
        PathBuf::from("/tmp/epic_saves"),
        false,
    );
    let gog_game = make_game(
        Provider::Gog,
        "Metro Exodus",
        PathBuf::from("/tmp/gog_saves"),
        false,
    );

    assert_eq!(
        repo_subpath_for_game(&epic_game, true),
        PathBuf::from("epic_games/metro_exodus")
    );
    assert_eq!(
        repo_subpath_for_game(&steam_game, true),
        PathBuf::from("steam/metro_exodus")
    );
    assert_eq!(
        repo_subpath_for_game(&gog_game, true),
        PathBuf::from("gog/metro_exodus")
    );

    assert_eq!(
        repo_subpath_for_game(&epic_game, false),
        PathBuf::from("metro_exodus")
    );
    assert_eq!(
        repo_subpath_for_game(&steam_game, false),
        PathBuf::from("metro_exodus")
    );
}

#[test]
fn test_sync_rate_defaults_and_scheduling() {
    assert_eq!(SyncRate::default(), SyncRate::OnStartup);

    let tmp = TempDir::new().unwrap();
    let mut cfg = make_test_config(tmp.path());
    assert!(!cfg.sync_enabled);
    assert_eq!(cfg.global_sync_rate, SyncRate::OnStartup);
    assert_eq!(cfg.repo_name, "save_game_manager");
    assert!(cfg.separate_folder_per_provider);

    let g_startup = make_game(Provider::Steam, "Hades", tmp.path().join("hades"), false);
    let mut g_manual = make_game(Provider::Gog, "Witcher 3", tmp.path().join("w3"), false);
    g_manual.sync_rate_override = Some(SyncRate::Manual);

    let mut g_periodic_due =
        make_game(Provider::EpicGames, "Control", tmp.path().join("ctrl"), false);
    g_periodic_due.sync_rate_override = Some(SyncRate::EveryMinutes(15));
    g_periodic_due.last_synced_at = Some(Utc::now() - Duration::minutes(20));

    let mut g_periodic_fresh =
        make_game(Provider::Steam, "Celeste", tmp.path().join("celeste"), false);
    g_periodic_fresh.sync_rate_override = Some(SyncRate::EveryMinutes(15));
    g_periodic_fresh.last_synced_at = Some(Utc::now() - Duration::minutes(2));

    let mut g_disabled = make_game(Provider::Steam, "Disabled", tmp.path().join("dis"), false);
    g_disabled.enabled = false;

    assert_eq!(
        g_startup.effective_sync_rate(&cfg.global_sync_rate),
        SyncRate::OnStartup
    );
    assert_eq!(
        g_manual.effective_sync_rate(&cfg.global_sync_rate),
        SyncRate::Manual
    );

    cfg.games = vec![
        g_startup.clone(),
        g_manual,
        g_periodic_due.clone(),
        g_periodic_fresh,
        g_disabled,
    ];

    // Live sync scheduling stays inactive until the user enables sync in Settings.
    assert!(games_due_on_startup(&cfg).is_empty());
    assert!(games_due_periodic(&cfg, Utc::now()).is_empty());

    cfg.sync_enabled = true;
    let startup_due = games_due_on_startup(&cfg);
    assert_eq!(startup_due, vec![g_startup.id]);

    let periodic_due = games_due_periodic(&cfg, Utc::now());
    assert_eq!(periodic_due, vec![g_periodic_due.id]);
}

#[test]
fn test_cross_provider_save_sync() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = make_test_config(tmp.path());

    let steam_saves = tmp.path().join("saves_steam");
    let epic_saves = tmp.path().join("saves_epic");

    write_with_mtime(&epic_saves.join("slot1.sav"), b"epic-older-save", 1_700_000_100);
    write_with_mtime(&steam_saves.join("slot1.sav"), b"steam-newer-save", 1_700_000_200);
    write_with_mtime(&steam_saves.join("slot2.sav"), b"steam-slot2", 1_700_000_200);

    cfg.games = vec![
        make_game(Provider::Steam, "Metro Exodus", steam_saves.clone(), true),
        make_game(Provider::EpicGames, "Metro Exodus", epic_saves.clone(), true),
    ];

    let outcome = sync_game_in_config(&mut cfg, "steam:metro_exodus", false).unwrap();
    assert_eq!(outcome.synced_games, 2);
    assert!(outcome.committed_sha.is_some());

    // Both local save folders should now have Steam's newer saves.
    assert_eq!(
        fs::read(epic_saves.join("slot1.sav")).unwrap(),
        b"steam-newer-save"
    );
    assert_eq!(
        fs::read(epic_saves.join("slot2.sav")).unwrap(),
        b"steam-slot2"
    );

    // Both provider folders in the repository must be updated.
    assert_eq!(
        fs::read(cfg.local_repo_path.join("steam/metro_exodus/slot1.sav")).unwrap(),
        b"steam-newer-save"
    );
    assert_eq!(
        fs::read(cfg.local_repo_path.join("epic_games/metro_exodus/slot1.sav")).unwrap(),
        b"steam-newer-save"
    );

    // Now simulate saving newer progress on Epic Games and syncing from Epic.
    write_with_mtime(&epic_saves.join("slot1.sav"), b"epic-newest-save", 1_700_000_500);
    sync_game_in_config(&mut cfg, "epic_games:metro_exodus", false).unwrap();

    assert_eq!(
        fs::read(steam_saves.join("slot1.sav")).unwrap(),
        b"epic-newest-save"
    );
    assert_eq!(
        fs::read(cfg.local_repo_path.join("steam/metro_exodus/slot1.sav")).unwrap(),
        b"epic-newest-save"
    );
}

#[test]
fn test_large_file_warning_split_and_reassembly() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = make_test_config(tmp.path());
    cfg.lfs_warning_threshold_mb = 1;
    cfg.chunk_size_mb = 1;

    let save_dir = tmp.path().join("big_game_saves");
    fs::create_dir_all(&save_dir).unwrap();

    // 2.5 MiB deterministic payload exceeding the 1 MiB threshold.
    let payload: Vec<u8> = (0..(2 * 1024 * 1024 + 512 * 1024))
        .map(|i| ((i * 31 + 7) % 251) as u8)
        .collect();
    let big_file = save_dir.join("huge_world.sav");
    fs::write(&big_file, &payload).unwrap();
    let original_sha = sha256_file(&big_file).unwrap();

    cfg.games = vec![make_game(Provider::Gog, "Huge RPG", save_dir.clone(), false)];

    // 1. Test warning when split_large_saves is disabled.
    cfg.split_large_saves = false;
    let warn_only = sync_game_in_config(&mut cfg, "gog:huge_rpg", false).unwrap();
    assert_eq!(warn_only.warnings.len(), 1);
    assert!(!warn_only.warnings[0].split_applied);
    assert!(
        cfg.local_repo_path
            .join("gog/huge_rpg/huge_world.sav")
            .is_file()
    );

    // 2. Test splitting when split_large_saves is enabled.
    cfg.split_large_saves = true;
    let split_outcome = sync_game_in_config(&mut cfg, "gog:huge_rpg", false).unwrap();
    assert_eq!(split_outcome.warnings.len(), 1);
    assert!(split_outcome.warnings[0].split_applied);

    let repo_game_dir = cfg.local_repo_path.join("gog/huge_rpg");
    assert!(!repo_game_dir.join("huge_world.sav").exists());

    let mut has_manifest = false;
    let mut part_count = 0usize;
    for entry in fs::read_dir(&repo_game_dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_manifest_file_name(&name) {
            has_manifest = true;
        } else if name.contains(".sgm_part.") {
            part_count += 1;
        }
    }
    assert!(has_manifest);
    assert_eq!(part_count, 3);

    // 3. Corrupt local save, sync v2, then roll back to split commit and verify SHA-256.
    let commits_after_split =
        list_commits_for_path(&cfg.local_repo_path, Path::new("gog/huge_rpg")).unwrap();
    let split_commit = commits_after_split[0].commit_hash.clone();

    fs::write(&big_file, b"corrupted-tiny-save").unwrap();
    sync_game_in_config(&mut cfg, "gog:huge_rpg", false).unwrap();
    assert_ne!(sha256_file(&big_file).unwrap(), original_sha);

    rollback_game_to_commit(&mut cfg, "gog:huge_rpg", &split_commit, false).unwrap();
    assert_eq!(sha256_file(&big_file).unwrap(), original_sha);
    assert_eq!(fs::read(&big_file).unwrap().len(), payload.len());
}

#[test]
fn test_platform_provider_defaults_and_library_discovery() {
    let home = PathBuf::from("/home/tester");

    assert!(!default_library_paths(Provider::Steam, Platform::Linux, &home).is_empty());
    assert!(
        default_library_paths(Provider::Steam, Platform::Windows, &home)
            .iter()
            .any(|p| p.to_string_lossy().contains("Program Files (x86)"))
    );
    assert!(
        default_library_paths(Provider::Gog, Platform::MacOs, &home)
            .iter()
            .any(|p| p.to_string_lossy().contains("Applications"))
    );
    assert!(
        default_library_paths(Provider::EpicGames, Platform::Linux, &home)
            .iter()
            .any(|p| p.to_string_lossy().contains("Heroic"))
    );

    let tmp = TempDir::new().unwrap();

    // Mock Steam library with libraryfolders.vdf pointing to a secondary library.
    let steam_main = tmp.path().join("SteamMain");
    let steam_extra = tmp.path().join("SteamExtra");
    fs::create_dir_all(steam_main.join("steamapps")).unwrap();
    fs::create_dir_all(steam_extra.join("steamapps/common/MetroExodus")).unwrap();
    fs::create_dir_all(steam_extra.join("userdata/999/412020/remote")).unwrap();
    fs::write(
        steam_extra.join("userdata/999/412020/remote/save.cfg"),
        b"steam-cloud-save",
    )
    .unwrap();

    fs::write(
        steam_main.join("steamapps/libraryfolders.vdf"),
        format!(
            "\"libraryfolders\"\n{{\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
            steam_extra.display()
        ),
    )
    .unwrap();
    fs::write(
        steam_extra.join("steamapps/appmanifest_412020.acf"),
        "\"AppState\"\n{\n\t\"appid\"\t\t\"412020\"\n\t\"name\"\t\t\"Metro Exodus\"\n\t\"installdir\"\t\t\"MetroExodus\"\n}\n",
    )
    .unwrap();

    // Mock GOG library with goggame-*.info.
    let gog_lib = tmp.path().join("GogLib");
    let gog_game = gog_lib.join("Metro Exodus GOG");
    fs::create_dir_all(gog_game.join("saves")).unwrap();
    fs::write(
        gog_game.join("goggame-12345.info"),
        r#"{"gameId": "12345", "name": "Metro Exodus"}"#,
    )
    .unwrap();

    // Mock Epic Games library with .item manifest and Legendary installed.json.
    let epic_manifests = tmp.path().join("EpicManifests");
    let epic_install = tmp.path().join("EpicGames/MetroExodus");
    fs::create_dir_all(&epic_manifests).unwrap();
    fs::create_dir_all(epic_install.join("Saves")).unwrap();
    fs::write(
        epic_manifests.join("metro.item"),
        format!(
            r#"{{"DisplayName": "Metro Exodus - Enhanced Edition", "InstallLocation": "{}", "AppName": "Snapdragon"}}"#,
            epic_install.display()
        ),
    )
    .unwrap();

    let libs = vec![
        ProviderLibraryConfig {
            provider: Provider::Steam,
            library_paths: vec![steam_main],
            enabled: true,
        },
        ProviderLibraryConfig {
            provider: Provider::Gog,
            library_paths: vec![gog_lib],
            enabled: true,
        },
        ProviderLibraryConfig {
            provider: Provider::EpicGames,
            library_paths: vec![epic_manifests],
            enabled: true,
        },
    ];

    let discovered = scan_provider_libraries(&libs);
    assert_eq!(discovered.len(), 3);

    let steam_entry = discovered
        .iter()
        .find(|g| g.provider == Provider::Steam)
        .unwrap();
    assert_eq!(steam_entry.app_id.as_deref(), Some("412020"));
    assert!(steam_entry.save_dir.ends_with("userdata/999/412020/remote"));

    let gog_entry = discovered
        .iter()
        .find(|g| g.provider == Provider::Gog)
        .unwrap();
    assert!(gog_entry.save_dir.ends_with("saves"));

    let epic_entry = discovered
        .iter()
        .find(|g| g.provider == Provider::EpicGames)
        .unwrap();
    assert!(epic_entry.save_dir.ends_with("Saves"));

    let groups = find_multi_provider_groups(&discovered);
    assert_eq!(
        groups.get("metro_exodus").unwrap(),
        &vec![Provider::Steam, Provider::Gog, Provider::EpicGames]
    );
}

#[test]
fn test_entire_folder_vs_selected_files_sync_and_rollback() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = make_test_config(tmp.path());

    let save_dir = tmp.path().join("indie_saves");
    fs::create_dir_all(save_dir.join("sub")).unwrap();
    fs::create_dir_all(save_dir.join("logs")).unwrap();

    fs::write(save_dir.join("slot1.sav"), b"v1-slot1").unwrap();
    fs::write(save_dir.join("sub/chapter1.sav"), b"v1-chapter1").unwrap();
    fs::write(save_dir.join("profile.sav"), b"unselected-profile").unwrap();
    fs::write(save_dir.join("logs/debug.log"), b"unselected-log").unwrap();

    let mut game = make_game(Provider::Steam, "Indie Quest", save_dir.clone(), false);
    game.sync_selection = SyncSelectionMode::SelectedFiles(vec![
        "slot1.sav".to_string(),
        "sub/chapter1.sav".to_string(),
    ]);
    cfg.games = vec![game];

    // Sync v1 with only selected files.
    sync_game_in_config(&mut cfg, "steam:indie_quest", false).unwrap();

    let repo_dir = cfg.local_repo_path.join("steam/indie_quest");
    assert!(repo_dir.join("slot1.sav").is_file());
    assert!(repo_dir.join("sub/chapter1.sav").is_file());
    assert!(!repo_dir.join("profile.sav").exists());
    assert!(!repo_dir.join("logs/debug.log").exists());

    let v1_commits =
        list_commits_for_path(&cfg.local_repo_path, Path::new("steam/indie_quest")).unwrap();
    let v1_hash = v1_commits[0].commit_hash.clone();

    // Modify both selected and unselected files, sync v2, then rollback to v1.
    fs::write(save_dir.join("slot1.sav"), b"v2-slot1").unwrap();
    fs::write(save_dir.join("logs/debug.log"), b"keep-local-log").unwrap();
    sync_game_in_config(&mut cfg, "steam:indie_quest", false).unwrap();

    rollback_game_to_commit(&mut cfg, "steam:indie_quest", &v1_hash, false).unwrap();
    assert_eq!(fs::read(save_dir.join("slot1.sav")).unwrap(), b"v1-slot1");
    assert_eq!(
        fs::read(save_dir.join("logs/debug.log")).unwrap(),
        b"keep-local-log"
    );

    // Switch to EntireFolder and verify unselected files are now synced too.
    cfg.games[0].sync_selection = SyncSelectionMode::EntireFolder;
    sync_game_in_config(&mut cfg, "steam:indie_quest", false).unwrap();
    assert!(repo_dir.join("profile.sav").is_file());
    assert!(repo_dir.join("logs/debug.log").is_file());
}

#[test]
fn test_ssh_key_generation_and_auth_helpers() {
    let tmp = TempDir::new().unwrap();
    let key_path = tmp.path().join("ssh/save_game_manager_ed25519");

    let pub_key = generate_ssh_keypair(&key_path, "").unwrap();
    assert!(key_path.is_file());
    assert!(public_key_path_for(&key_path).is_file());
    assert!(pub_key.starts_with("ssh-ed25519 "));

    let read_back = read_public_key_for_private_key(&key_path).unwrap();
    assert_eq!(read_back, pub_key);

    // Calling generate_ssh_keypair again on the same path should fail safely without hanging.
    assert!(generate_ssh_keypair(&key_path, "").is_err());

    let ssh_auth = AuthMethod::ExistingSshKey {
        private_key_path: key_path.clone(),
        public_key_path: Some(public_key_path_for(&key_path)),
        passphrase: String::new(),
        github_username: "octocat".to_string(),
        pat_for_api: None,
    };
    assert_eq!(
        remote_url_for_auth("octocat", "save_game_manager", &ssh_auth).as_deref(),
        Some("git@github.com:octocat/save_game_manager.git")
    );

    let pat_auth = AuthMethod::PersonalAccessToken {
        token: "ghp_example".to_string(),
        github_username: "octocat".to_string(),
    };
    assert_eq!(
        remote_url_for_auth("octocat", "save_game_manager", &pat_auth).as_deref(),
        Some("https://github.com/octocat/save_game_manager.git")
    );
}

#[test]
fn test_git_commit_history_and_rollback_workflow() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = make_test_config(tmp.path());

    let save_dir = tmp.path().join("action_saves");
    fs::create_dir_all(&save_dir).unwrap();
    fs::write(save_dir.join("state.sav"), b"level_1").unwrap();
    fs::write(save_dir.join("extra.sav"), b"bonus_items").unwrap();

    cfg.games = vec![make_game(Provider::Steam, "Action Hero", save_dir.clone(), false)];
    sync_game_in_config(&mut cfg, "steam:action_hero", false).unwrap();

    // Update save to v2: modify state.sav, remove extra.sav, add boss.sav.
    fs::write(save_dir.join("state.sav"), b"level_2").unwrap();
    fs::remove_file(save_dir.join("extra.sav")).unwrap();
    fs::write(save_dir.join("boss.sav"), b"boss_checkpoint").unwrap();
    sync_game_in_config(&mut cfg, "steam:action_hero", false).unwrap();

    let commits =
        list_commits_for_path(&cfg.local_repo_path, Path::new("steam/action_hero")).unwrap();
    assert_eq!(commits.len(), 2);
    assert!(!commits[0].changed_files.is_empty());

    // commits[0] is newest (v2), commits[1] is v1.
    let v1_commit = commits[1].commit_hash.clone();
    let restored = rollback_game_to_commit(&mut cfg, "steam:action_hero", &v1_commit, false).unwrap();
    assert_eq!(restored, 2);

    assert_eq!(fs::read(save_dir.join("state.sav")).unwrap(), b"level_1");
    assert_eq!(fs::read(save_dir.join("extra.sav")).unwrap(), b"bonus_items");
    assert!(!save_dir.join("boss.sav").exists());

    let commits_after =
        list_commits_for_path(&cfg.local_repo_path, Path::new("steam/action_hero")).unwrap();
    assert_eq!(commits_after.len(), 3);
    assert!(commits_after[0].message.starts_with("Rollback steam/action_hero"));
}

#[test]
fn test_config_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let cfg_path = tmp.path().join("nested/config.json");
    let mut cfg = make_test_config(tmp.path());
    cfg.global_sync_rate = SyncRate::EveryMinutes(45);
    cfg.separate_folder_per_provider = false;
    cfg.games.push(make_game(
        Provider::EpicGames,
        "Alan Wake 2",
        tmp.path().join("aw2"),
        true,
    ));

    save_config(&cfg_path, &cfg).unwrap();
    let loaded = load_config(&cfg_path).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn test_cross_provider_rollback_and_legendary_discovery() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = make_test_config(tmp.path());

    let steam_saves = tmp.path().join("cross_steam");
    let epic_saves = tmp.path().join("cross_epic");
    write_with_mtime(&steam_saves.join("save.dat"), b"v1-shared", 1_700_000_100);
    fs::create_dir_all(&epic_saves).unwrap();

    cfg.games = vec![
        make_game(Provider::Steam, "Metro Exodus", steam_saves.clone(), true),
        make_game(Provider::EpicGames, "Metro Exodus", epic_saves.clone(), true),
    ];

    sync_game_in_config(&mut cfg, "steam:metro_exodus", false).unwrap();
    let v1_commits =
        list_commits_for_path(&cfg.local_repo_path, Path::new("steam/metro_exodus")).unwrap();
    let v1_hash = v1_commits[0].commit_hash.clone();

    write_with_mtime(&epic_saves.join("save.dat"), b"v2-shared", 1_700_000_300);
    sync_game_in_config(&mut cfg, "epic_games:metro_exodus", false).unwrap();
    assert_eq!(fs::read(steam_saves.join("save.dat")).unwrap(), b"v2-shared");

    // Rolling back Steam should also restore Epic's local save and repo folder.
    rollback_game_to_commit(&mut cfg, "steam:metro_exodus", &v1_hash, false).unwrap();
    assert_eq!(fs::read(steam_saves.join("save.dat")).unwrap(), b"v1-shared");
    assert_eq!(fs::read(epic_saves.join("save.dat")).unwrap(), b"v1-shared");
    assert_eq!(
        fs::read(cfg.local_repo_path.join("epic_games/metro_exodus/save.dat")).unwrap(),
        b"v1-shared"
    );

    // Also test Heroic/Legendary installed.json discovery.
    let legendary_dir = tmp.path().join("legendary");
    let heroic_install = tmp.path().join("HeroicGames/Control");
    fs::create_dir_all(&legendary_dir).unwrap();
    fs::create_dir_all(heroic_install.join("saves")).unwrap();
    fs::write(
        legendary_dir.join("installed.json"),
        format!(
            r#"{{"ControlApp": {{"title": "Control", "install_path": "{}"}}}}"#,
            heroic_install.display()
        ),
    )
    .unwrap();
    let libs = vec![ProviderLibraryConfig {
        provider: Provider::EpicGames,
        library_paths: vec![legendary_dir],
        enabled: true,
    }];
    let discovered = scan_provider_libraries(&libs);
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].game_slug, "control");
    assert!(discovered[0].save_dir.ends_with("saves"));
}

#[test]
fn test_save_state_labels_on_sync_and_commits() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = make_test_config(tmp.path());

    let save_dir = tmp.path().join("elden_ring_saves");
    fs::create_dir_all(&save_dir).unwrap();
    fs::write(save_dir.join("ER0000.sl2"), b"state-before-malenia").unwrap();

    cfg.games = vec![make_game(Provider::Steam, "Elden Ring", save_dir.clone(), false)];

    let first = sync_game_with_label(&mut cfg, "steam:elden_ring", Some("Before Malenia"), false)
        .unwrap();
    let c1 = first.committed_sha.expect("expected first commit hash");
    assert_eq!(
        cfg.games[0].active_save_label.as_deref(),
        Some("Before Malenia")
    );

    fs::write(save_dir.join("ER0000.sl2"), b"state-ng-plus").unwrap();
    let second = sync_game_with_label(&mut cfg, "steam:elden_ring", Some("NG+ Start"), false)
        .unwrap();
    let c2 = second.committed_sha.expect("expected second commit hash");
    assert_eq!(cfg.games[0].active_save_label.as_deref(), Some("NG+ Start"));

    let commits =
        list_commits_for_path(&cfg.local_repo_path, Path::new("steam/elden_ring")).unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].label.as_deref(), Some("NG+ Start"));
    assert_eq!(commits[1].label.as_deref(), Some("Before Malenia"));

    // Relabel the older commit explicitly and verify config + repo metadata manifest update.
    set_save_state_label(&mut cfg, "steam:elden_ring", &c1, "Malenia Prep v2").unwrap();
    assert_eq!(
        label_for_commit(&cfg, "steam:elden_ring", &c1, None).as_deref(),
        Some("Malenia Prep v2")
    );
    assert_eq!(
        label_for_commit(&cfg, "steam:elden_ring", &c2, None).as_deref(),
        Some("NG+ Start")
    );

    assert!(cfg.local_repo_path.join(REPO_METADATA_FILE).is_file());
    let manifest =
        read_repo_metadata_manifest(&cfg.local_repo_path).expect("metadata manifest present");
    assert_eq!(manifest.games.len(), 1);
    assert_eq!(manifest.games[0].name, "Elden Ring");
    assert!(
        manifest
            .save_state_labels
            .iter()
            .any(|l| l.commit_hash == c1 && l.label == "Malenia Prep v2")
    );
}

#[test]
fn test_reinstate_full_structure_after_system_reset() {
    let tmp = TempDir::new().unwrap();
    let mut user_a = make_test_config(tmp.path());
    user_a.local_repo_path = tmp.path().join("user_a_repo");

    let cp_saves = tmp.path().join("orig_cp_saves");
    let control_saves = tmp.path().join("orig_control_saves");
    fs::create_dir_all(&cp_saves).unwrap();
    fs::create_dir_all(&control_saves).unwrap();
    fs::write(cp_saves.join("sav.dat"), b"cp-act2-save").unwrap();
    fs::write(control_saves.join("slot.bin"), b"control-endgame").unwrap();

    user_a.games = vec![
        make_game(Provider::Steam, "Cyberpunk 2077", cp_saves, false),
        make_game(Provider::EpicGames, "Control", control_saves, false),
    ];

    sync_game_with_label(&mut user_a, "steam:cyberpunk_2077", Some("Act 2 Nomad"), false)
        .unwrap();
    sync_game_with_label(&mut user_a, "epic_games:control", Some("Oldest House 100%"), false)
        .unwrap();

    // Simulate a clean OS reinstall / system reset with empty config and fresh local repo path.
    let mut reset_cfg = default_config();
    reset_cfg.local_repo_path = tmp.path().join("reset_repo");
    reset_cfg.provider_libraries.clear();
    let fallback_root = tmp.path().join("reinstated_saves");

    let outcome = reinstate_full_structure(
        &mut reset_cfg,
        Some(user_a.local_repo_path.to_str().unwrap()),
        &fallback_root,
    )
    .unwrap();

    assert_eq!(outcome.games_reinstated, 2);
    assert_eq!(outcome.files_restored, 2);
    assert_eq!(outcome.labels_restored, 2);
    assert!(reset_cfg.separate_folder_per_provider);

    let cp_restored = fallback_root.join("steam/cyberpunk_2077/sav.dat");
    let ctrl_restored = fallback_root.join("epic_games/control/slot.bin");
    assert_eq!(fs::read(cp_restored).unwrap(), b"cp-act2-save");
    assert_eq!(fs::read(ctrl_restored).unwrap(), b"control-endgame");

    let cp_entry = reset_cfg
        .games
        .iter()
        .find(|g| g.id == "steam:cyberpunk_2077")
        .unwrap();
    assert_eq!(cp_entry.active_save_label.as_deref(), Some("Act 2 Nomad"));
}

#[test]
fn test_import_from_shared_external_repository() {
    let tmp = TempDir::new().unwrap();

    // Friend's repository with two historical labeled states for Hades.
    let mut friend_cfg = make_test_config(tmp.path());
    friend_cfg.local_repo_path = tmp.path().join("friend_repo");
    let friend_hades = tmp.path().join("friend_hades");
    fs::create_dir_all(&friend_hades).unwrap();
    friend_cfg.games = vec![make_game(Provider::Steam, "Hades", friend_hades.clone(), false)];

    fs::write(friend_hades.join("Profile1.sav"), b"hades-early-run").unwrap();
    let c1 = sync_game_with_label(&mut friend_cfg, "steam:hades", Some("Tartarus Exit"), false)
        .unwrap()
        .committed_sha
        .unwrap();

    fs::write(friend_hades.join("Profile1.sav"), b"hades-final-escape").unwrap();
    sync_game_with_label(&mut friend_cfg, "steam:hades", Some("Redacted Clear"), false).unwrap();

    // Current user inspects friend's repo and imports the earlier commit into Epic Games.
    let inspection = inspect_repository(&friend_cfg.local_repo_path).unwrap();
    assert_eq!(inspection.games.len(), 1);
    assert!(
        inspection
            .labels
            .iter()
            .any(|l| l.commit_hash == c1 && l.label == "Tartarus Exit")
    );

    let mut user_b = make_test_config(tmp.path());
    user_b.local_repo_path = tmp.path().join("user_b_repo");
    let target_epic_dir = tmp.path().join("user_b_hades_epic");

    let import_res = import_game_from_external_repo(
        &mut user_b,
        &friend_cfg.local_repo_path,
        &ExternalImportRequest {
            source_game: inspection.games[0].clone(),
            commit_hash: Some(c1),
            target_provider: Provider::EpicGames,
            target_save_dir: target_epic_dir.clone(),
            custom_label: Some("Friend Tartarus State".to_string()),
        },
        false,
    )
    .unwrap();

    assert!(import_res.committed_sha.is_some());
    assert_eq!(
        fs::read(target_epic_dir.join("Profile1.sav")).unwrap(),
        b"hades-early-run"
    );
    assert_eq!(
        fs::read(user_b.local_repo_path.join("epic_games/hades/Profile1.sav")).unwrap(),
        b"hades-early-run"
    );
    assert_eq!(
        user_b.games[0].active_save_label.as_deref(),
        Some("Friend Tartarus State")
    );
}

#[test]
fn test_import_online_save_file_and_folder_with_pre_backup() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = make_test_config(tmp.path());

    let target_dir = tmp.path().join("witcher3_saves");
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(target_dir.join("manual_slot0.sav"), b"my-original-progress").unwrap();

    // 1. Import a downloaded single online save file with pre-import backup enabled.
    let downloaded_file = tmp.path().join("online_100pct.sav");
    fs::write(&downloaded_file, b"downloaded-online-save").unwrap();

    import_online_save(
        &mut cfg,
        &OnlineSaveImportRequest {
            source_path: downloaded_file,
            game_name: "Witcher 3".to_string(),
            provider: Provider::Gog,
            target_save_dir: target_dir.clone(),
            label: Some("Nexus 100% Save".to_string()),
            backup_existing_before_import: true,
        },
        false,
    )
    .unwrap();

    assert_eq!(
        fs::read(target_dir.join("manual_slot0.sav")).unwrap(),
        b"my-original-progress"
    );
    assert_eq!(
        fs::read(target_dir.join("online_100pct.sav")).unwrap(),
        b"downloaded-online-save"
    );

    let commits =
        list_commits_for_path(&cfg.local_repo_path, Path::new("gog/witcher_3")).unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].label.as_deref(), Some("Nexus 100% Save"));
    assert_eq!(commits[1].label.as_deref(), Some("Pre-import backup"));

    let pre_backup_commit = commits[1].commit_hash.clone();

    // 2. Import an unpacked online save folder containing a replacement save file.
    let downloaded_folder = tmp.path().join("downloaded_folder_save");
    fs::create_dir_all(&downloaded_folder).unwrap();
    fs::write(downloaded_folder.join("manual_slot0.sav"), b"folder-override").unwrap();

    import_online_save(
        &mut cfg,
        &OnlineSaveImportRequest {
            source_path: downloaded_folder,
            game_name: "Witcher 3".to_string(),
            provider: Provider::Gog,
            target_save_dir: target_dir.clone(),
            label: Some("Boss Rush Pack".to_string()),
            backup_existing_before_import: false,
        },
        false,
    )
    .unwrap();
    assert_eq!(
        fs::read(target_dir.join("manual_slot0.sav")).unwrap(),
        b"folder-override"
    );

    // Verify rollback back to the automatic pre-import backup restores original state cleanly.
    rollback_game_to_commit(&mut cfg, "gog:witcher_3", &pre_backup_commit, false).unwrap();
    assert_eq!(
        fs::read(target_dir.join("manual_slot0.sav")).unwrap(),
        b"my-original-progress"
    );
    assert!(!target_dir.join("online_100pct.sav").exists());
}

#[test]
fn test_jizihub_theme_palettes() {
    let dark = palette(ThemeMode::Dark);
    let light = palette(ThemeMode::Light);

    assert_eq!(dark.bg_canvas, Color32::from_rgb(0x0c, 0x0e, 0x12));
    assert_eq!(dark.accent, Color32::from_rgb(0xff, 0x6a, 0x2b));
    assert_eq!(light.bg_canvas, Color32::from_rgb(0xe7, 0xdf, 0xd4));
    assert_eq!(light.accent, Color32::from_rgb(0xd9, 0x4e, 0x14));
    assert_eq!(ThemeMode::Dark.display_name(), "Dark");
    assert_eq!(ThemeMode::Light.display_name(), "Light");
    assert_eq!(ThemeMode::Dark.toggle(), ThemeMode::Light);
    assert_eq!(ThemeMode::Light.toggle(), ThemeMode::Dark);
}

#[test]
fn test_save_file_viewer_and_editor_formats() {
    use save_game_manager::save_editor::{
        SaveFileFormat, format_json_content, inspect_save_file, save_edited_file,
        validate_save_content,
    };

    let tmp = tempfile::tempdir().unwrap();

    // 1. JSON save file: inspect, pretty-format, validate, edit & create .sgm.bak backup
    let json_path = tmp.path().join("profile.json");
    fs::write(&json_path, r#"{"level":12,"hp":100}"#).unwrap();
    let loaded_json = inspect_save_file(&json_path).unwrap();
    assert_eq!(loaded_json.format, SaveFileFormat::Json);
    assert!(loaded_json.editable);

    let pretty = format_json_content(&loaded_json.text_content).unwrap();
    assert!(pretty.contains('\n'));
    assert!(validate_save_content(SaveFileFormat::Json, "{bad json").is_err());

    let updated_json = r#"{"level": 13, "hp": 120}"#;
    let backup = save_edited_file(&json_path, SaveFileFormat::Json, updated_json, true).unwrap();
    assert!(backup.as_ref().is_some_and(|p| p.ends_with("profile.json.sgm.bak")));
    assert_eq!(
        fs::read_to_string(backup.unwrap()).unwrap(),
        r#"{"level":12,"hp":100}"#
    );
    assert_eq!(fs::read_to_string(&json_path).unwrap(), updated_json);

    // 2. Structured INI save config
    let ini_path = tmp.path().join("settings.ini");
    fs::write(&ini_path, "[Player]\ndifficulty=Normal\n").unwrap();
    let loaded_ini = inspect_save_file(&ini_path).unwrap();
    assert_eq!(loaded_ini.format, SaveFileFormat::StructuredText);
    assert!(loaded_ini.editable);
    save_edited_file(
        &ini_path,
        SaveFileFormat::StructuredText,
        "[Player]\ndifficulty=Hard\n",
        false,
    )
    .unwrap();
    assert!(fs::read_to_string(&ini_path).unwrap().contains("Hard"));

    // 3. Binary save file: read-only hex + extracted strings, rejects text writes
    let bin_path = tmp.path().join("slot0.sav");
    let bin_bytes = b"\x00\x01\x02SAVEHDR\x00HeroName\x00\xff\xfe\x00\x00";
    fs::write(&bin_path, bin_bytes).unwrap();
    let loaded_bin = inspect_save_file(&bin_path).unwrap();
    assert_eq!(loaded_bin.format, SaveFileFormat::Binary);
    assert!(!loaded_bin.editable);
    assert!(loaded_bin.hex_preview.contains("SAVEHDR"));
    assert!(loaded_bin.extracted_strings.iter().any(|s| s == "SAVEHDR"));
    assert!(loaded_bin.extracted_strings.iter().any(|s| s == "HeroName"));
    assert!(save_edited_file(&bin_path, SaveFileFormat::Binary, "overwrite", false).is_err());
}

#[test]
fn test_system_autostart_configuration() {
    use save_game_manager::autostart::configure_autostart_at;
    use save_game_manager::models::Platform;

    let tmp = tempfile::tempdir().unwrap();
    let exe = tmp.path().join("save_game_manager");
    let icon = tmp.path().join("savevault.svg");

    let desktop = tmp.path().join("linux/savevault.desktop");
    configure_autostart_at(true, Platform::Linux, &desktop, &exe, Some(&icon)).unwrap();
    let desktop_txt = fs::read_to_string(&desktop).unwrap();
    assert!(desktop_txt.contains("Name=SaveVault"));
    assert!(desktop_txt.contains("X-GNOME-Autostart-enabled=true"));
    configure_autostart_at(false, Platform::Linux, &desktop, &exe, None).unwrap();
    assert!(!desktop.exists());

    let plist = tmp.path().join("macos/io.savevault.app.plist");
    configure_autostart_at(true, Platform::MacOs, &plist, &exe, None).unwrap();
    let plist_txt = fs::read_to_string(&plist).unwrap();
    assert!(plist_txt.contains("io.savevault.app"));
    assert!(plist_txt.contains("<true/>"));

    let cmd = tmp.path().join("windows/SaveVault.cmd");
    configure_autostart_at(true, Platform::Windows, &cmd, &exe, None).unwrap();
    let cmd_txt = fs::read_to_string(&cmd).unwrap();
    assert!(cmd_txt.contains("@echo off"));
}

#[test]
fn test_logo_icon_generation() {
    use save_game_manager::logo::{APP_NAME, SVG_LOGO, build_app_icon, build_app_icon_png};

    assert_eq!(APP_NAME, "SaveVault");
    assert!(SVG_LOGO.contains("<svg") && SVG_LOGO.contains("</svg>"));

    let icon = build_app_icon();
    assert_eq!(icon.width, 64);
    assert_eq!(icon.height, 64);
    assert_eq!(icon.rgba.len(), 64 * 64 * 4);
    assert!(icon.rgba.as_chunks::<4>().0.iter().any(|px| px[3] > 0));

    let png = build_app_icon_png();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(&png[12..16], b"IHDR");
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 64);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 64);
    assert!(png.windows(4).any(|w| w == b"IDAT"));
    assert!(png.ends_with(b"IEND\xaeB`\x82"));
}

#[test]
fn test_steam_cross_library_userdata_and_proton_save_discovery() {
    use save_game_manager::models::Provider;
    use save_game_manager::providers::scan_provider_libraries;

    let tmp = tempfile::tempdir().unwrap();
    let main_steam = tmp.path().join("main_steam");
    let extra_lib = tmp.path().join("extra_lib");

    // Configure libraryfolders.vdf pointing main_steam -> extra_lib
    fs::create_dir_all(main_steam.join("config")).unwrap();
    fs::write(
        main_steam.join("config/libraryfolders.vdf"),
        format!(
            "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
            main_steam.display(),
            extra_lib.display(),
        ),
    )
    .unwrap();

    // Game 1: installed in extra_lib, saves in main_steam/userdata/123456/999001/remote
    fs::create_dir_all(extra_lib.join("steamapps/common/CrossLibCloud")).unwrap();
    fs::write(
        extra_lib.join("steamapps/appmanifest_999001.acf"),
        "\"AppState\"\n{\n\t\"appid\"\t\t\"999001\"\n\t\"name\"\t\t\"CrossLibCloud\"\n\t\"installdir\"\t\t\"CrossLibCloud\"\n}\n",
    )
    .unwrap();
    let expected_cloud = main_steam.join("userdata/123456/999001/remote");
    fs::create_dir_all(&expected_cloud).unwrap();
    fs::write(expected_cloud.join("slot0.sav"), b"cloud-save").unwrap();

    // Game 2: Proton prefix with vendor AppData/Roaming/EldenRing save dir
    fs::create_dir_all(extra_lib.join("steamapps/common/ProtonGame")).unwrap();
    fs::write(
        extra_lib.join("steamapps/appmanifest_999002.acf"),
        "\"AppState\"\n{\n\t\"appid\"\t\t\"999002\"\n\t\"name\"\t\t\"ProtonGame\"\n\t\"installdir\"\t\t\"ProtonGame\"\n}\n",
    )
    .unwrap();
    let expected_proton = extra_lib
        .join("steamapps/compatdata/999002/pfx/drive_c/users/steamuser/AppData/Roaming/EldenRing");
    fs::create_dir_all(&expected_proton).unwrap();
    fs::write(expected_proton.join("ER0000.sl2"), b"proton-save").unwrap();

    // Game 3: No save folder discovered yet -> must NOT fall back to huge install_dir
    let no_saves_install = extra_lib.join("steamapps/common/NoSavesYet");
    fs::create_dir_all(&no_saves_install).unwrap();
    fs::write(no_saves_install.join("assets.pak"), b"big-install-binary").unwrap();
    fs::write(
        extra_lib.join("steamapps/appmanifest_999003.acf"),
        "\"AppState\"\n{\n\t\"appid\"\t\t\"999003\"\n\t\"name\"\t\t\"NoSavesYet\"\n\t\"installdir\"\t\t\"NoSavesYet\"\n}\n",
    )
    .unwrap();

    let found = scan_provider_libraries(&[save_game_manager::models::ProviderLibraryConfig {
        provider: Provider::Steam,
        enabled: true,
        library_paths: vec![main_steam.clone()],
    }]);
    let cross = found.iter().find(|g| g.name == "CrossLibCloud").unwrap();
    assert_eq!(cross.save_dir, expected_cloud);

    let proton = found.iter().find(|g| g.name == "ProtonGame").unwrap();
    assert_eq!(proton.save_dir, expected_proton);

    let fallback = found.iter().find(|g| g.name == "NoSavesYet").unwrap();
    assert_ne!(fallback.save_dir, no_saves_install);
    assert_eq!(fallback.save_dir, no_saves_install.join("saves"));
}

#[test]
fn test_remove_game_keep_or_delete_git_folder() {
    use save_game_manager::sync_engine::{remove_game_entry, sync_multiple_games};

    let tmp = TempDir::new().unwrap();
    let mut cfg = make_test_config(tmp.path());

    let save_a = tmp.path().join("saves_a");
    let save_b = tmp.path().join("saves_b");
    fs::create_dir_all(&save_a).unwrap();
    fs::create_dir_all(&save_b).unwrap();
    fs::write(save_a.join("slot_a.sav"), b"save-a-data").unwrap();
    fs::write(save_b.join("slot_b.sav"), b"save-b-data").unwrap();

    cfg.games
        .push(make_game(Provider::Steam, "Game A", save_a.clone(), false));
    cfg.games
        .push(make_game(Provider::Gog, "Game B", save_b.clone(), false));

    let ids: Vec<String> = cfg.games.iter().map(|g| g.id.clone()).collect();
    sync_multiple_games(&mut cfg, &ids, false).unwrap();

    assert!(cfg.local_repo_path.join("steam/game_a/slot_a.sav").exists());
    assert!(cfg.local_repo_path.join("gog/game_b/slot_b.sav").exists());

    // 1. Remove Game A from list while keeping its folder in Git.
    let keep_outcome = remove_game_entry(&mut cfg, "steam:game_a", false, false).unwrap();
    assert!(!keep_outcome.removed_from_git);
    assert!(cfg.games.iter().all(|g| g.id != "steam:game_a"));
    assert!(cfg.local_repo_path.join("steam/game_a/slot_a.sav").exists());
    assert!(save_a.join("slot_a.sav").exists());

    // 2. Remove Game B from list AND delete its folder from Git.
    let delete_outcome = remove_game_entry(&mut cfg, "gog:game_b", true, false).unwrap();
    assert!(delete_outcome.removed_from_git);
    assert!(delete_outcome.committed_sha.is_some());
    assert!(cfg.games.iter().all(|g| g.id != "gog:game_b"));
    assert!(!cfg.local_repo_path.join("gog/game_b").exists());
    // Local saves on disk must never be deleted.
    assert!(save_b.join("slot_b.sav").exists());

    let manifest = read_repo_metadata_manifest(&cfg.local_repo_path).unwrap();
    assert!(manifest.games.iter().all(|g| g.id != "gog:game_b"));
}

#[test]
fn test_retroarch_platform_save_states_and_kbm_profiles() {
    use save_game_manager::models::{KbmAction, RetroArchHotkey};
    use save_game_manager::retroarch::{
        apply_kbm_profile_to_retroarch, ensure_playlist_item, load_kbm_profiles_registry,
    };

    let tmp = TempDir::new().unwrap();
    let ra_dir = tmp.path().join("retroarch");
    let roms_ps2 = tmp.path().join("ROMs/ps2");
    fs::create_dir_all(ra_dir.join("saves/ParaLLEl PS2")).unwrap();
    fs::create_dir_all(ra_dir.join("states/ParaLLEl PS2")).unwrap();
    fs::create_dir_all(ra_dir.join("config/ParaLLEl PS2")).unwrap();
    fs::create_dir_all(&roms_ps2).unwrap();

    let black_iso = roms_ps2.join("Black (Europe) (En,Fr,De,Es,It).iso");
    fs::write(&black_iso, b"black-iso-data").unwrap();
    let black_ps2 = ra_dir.join("saves/ParaLLEl PS2/Black (Europe) (En,Fr,De,Es,It).ps2");
    let shared_mc = ra_dir.join("saves/ParaLLEl PS2/Mcd001.ps2");
    fs::write(&black_ps2, b"black-save-v1").unwrap();
    fs::write(&shared_mc, b"shared-memcard").unwrap();
    let state0 = ra_dir.join("states/ParaLLEl PS2/Black (Europe) (En,Fr,De,Es,It).state");
    fs::write(&state0, b"savestate-v1").unwrap();

    ensure_playlist_item(
        &ra_dir.join("playlists/Sony - PlayStation 2.lpl"),
        &black_iso,
        "Black (Europe)",
        Path::new("/usr/lib/libretro/pcsx2_libretro.so"),
        "Sony - PlayStation 2 (LRPS2)",
        None,
    )
    .unwrap();

    let discovered = scan_provider_libraries(&[ProviderLibraryConfig {
        provider: Provider::RetroArch,
        library_paths: vec![ra_dir.clone(), tmp.path().join("ROMs")],
        enabled: true,
    }]);
    assert_eq!(discovered.len(), 1);
    let mut black = discovered[0].clone();
    assert_eq!(black.provider, Provider::RetroArch);
    assert_eq!(black.id, "retroarch:black_europe");

    // Customize per-game KBM settings and verify .cfg + registry roundtrip.
    {
        let meta = black.retroarch.as_mut().unwrap();
        meta.kbm_enabled = true;
        meta.kbm_profile.mouse_sensitivity = 1750.0;
        meta.kbm_profile
            .set_binding(KbmAction::SprintL3, 0, "F".to_string());
        meta.kbm_profile
            .set_hotkey(RetroArchHotkey::FastForwardToggle, 0, "LCtrl+Space".to_string());
    }
    let cfg_written = apply_kbm_profile_to_retroarch(&black).unwrap().unwrap();
    let cfg_txt = fs::read_to_string(&cfg_written).unwrap();
    assert!(cfg_txt.contains("input_player1_l3 = \"f\""));
    assert!(cfg_txt.contains("input_toggle_fast_forward_btn = \"13\""));

    let mut cfg = make_test_config(tmp.path());
    cfg.default_kbm_profile.mouse_sensitivity = 1400.0;
    cfg.games.push(black);
    let cfg_path = tmp.path().join("save_game_manager/config.json");
    save_config(&cfg_path, &cfg).unwrap();
    save_game_manager::retroarch::sync_kbm_registry_for_config(&cfg_path, &cfg).unwrap();

    let reg = load_kbm_profiles_registry(&tmp.path().join("save_game_manager/retroarch_kbm_profiles.json"));
    assert_eq!(reg.default_profile.mouse_sensitivity, 1400.0);
    assert_eq!(reg.games.len(), 1);
    assert_eq!(reg.games[0].profile.mouse_sensitivity, 1750.0);
    assert_eq!(
        reg.games[0]
            .profile
            .hotkey(RetroArchHotkey::FastForwardToggle, 0),
        "LCtrl+Space"
    );

    // Verify per-ROM save filtering & save states snapshot + rollback without clobbering Mcd001.ps2.
    let v1 = sync_game_in_config(&mut cfg, "retroarch:black_europe", false)
        .unwrap()
        .committed_sha
        .unwrap();
    use save_game_manager::git_ops::ROOTS_DIR;
    assert!(
        cfg.local_repo_path
            .join("retroarch/black_europe/Black (Europe) (En,Fr,De,Es,It).ps2")
            .is_file()
    );
    assert!(!cfg.local_repo_path.join("retroarch/black_europe/Mcd001.ps2").exists());
    assert!(
        cfg.local_repo_path
            .join("retroarch/black_europe")
            .join(ROOTS_DIR)
            .join("savestates/Black (Europe) (En,Fr,De,Es,It).state")
            .is_file()
    );

    fs::write(&black_ps2, b"black-save-v2").unwrap();
    fs::write(&shared_mc, b"other-game-memcard-edit").unwrap();
    fs::write(&state0, b"savestate-v2").unwrap();
    sync_game_in_config(&mut cfg, "retroarch:black_europe", false).unwrap();

    rollback_game_to_commit(&mut cfg, "retroarch:black_europe", &v1, false).unwrap();
    assert_eq!(fs::read(&black_ps2).unwrap(), b"black-save-v1");
    assert_eq!(fs::read(&state0).unwrap(), b"savestate-v1");
    assert_eq!(fs::read(&shared_mc).unwrap(), b"other-game-memcard-edit");
}



