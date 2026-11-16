//! End-to-end tests for the v2 engine: two-way sync between machines, conflicts, safety
//! backups, profiles, multi-root games, mod fingerprints, detection, and config migration.

use save_game_manager::config::{default_config, load_config, save_config};
use save_game_manager::detect::db::SaveDb;
use save_game_manager::detect::{
    AUTO_ENABLE_CONFIDENCE, apply_detection, detect_game, merge_scan_results, scan_installed_games,
};
use save_game_manager::git_ops::{PROFILES_DIR, ROOTS_DIR, SNAPSHOT_META_FILE, apply_auth_to_git_command};
use save_game_manager::models::{
    AppConfig, AuthMethod, ConflictPolicy, DetectionSource, GameEntry, ModSource, ModSourceKind,
    Provider, ProviderLibraryConfig, RootKind, SaveRoot, SyncSelectionMode,
};
use save_game_manager::profiles::{
    ProfileStart, create_profile, delete_profile, profile_summaries, rename_profile, switch_profile,
};
use save_game_manager::safety::{list_safety_backups, read_backup_info, restore_roots_zip};
use save_game_manager::sync_engine::{
    GameHealth, SyncOptions, game_health, list_snapshots, live_state, preview_restore,
    pull_cloud_catalog, read_snapshot_meta, remove_game_entry, repo_subpath_for_game,
    resolve_conflict_keep_local, resolve_conflict_use_cloud, rollback_game_to_commit, sync_games,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn bare_remote(root: &Path) -> PathBuf {
    let path = root.join("cloud.git");
    let out = Command::new("git")
        .args(["init", "--bare", "-b", "main"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    path
}

fn machine(root: &Path, name: &str, remote: Option<&Path>) -> AppConfig {
    let mut cfg = default_config();
    cfg.local_repo_path = root.join(name).join("data").join("repo");
    cfg.machine_name = name.to_string();
    cfg.auth = AuthMethod::PersonalAccessToken {
        token: String::new(),
        github_username: String::new(),
    };
    cfg.custom_remote_url = remote.map(|p| p.display().to_string());
    cfg.sync_enabled = true;
    cfg
}

fn game(name: &str, save_dir: PathBuf) -> GameEntry {
    let slug = name.to_lowercase().replace(' ', "_");
    GameEntry {
        id: format!("steam:{slug}"),
        name: name.to_string(),
        game_slug: slug,
        provider: Provider::Steam,
        save_dir,
        enabled: true,
        ..Default::default()
    }
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn sync(cfg: &mut AppConfig, id: &str) -> save_game_manager::sync_engine::SyncOutcome {
    sync_games(
        cfg,
        &[id.to_string()],
        SyncOptions {
            use_remote: true,
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn unchanged_saves_do_not_create_snapshots() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = machine(tmp.path(), "solo", None);
    let dir = tmp.path().join("saves");
    write(&dir.join("slot1.sav"), "v1");
    cfg.games.push(game("Quiet Game", dir.clone()));

    let first = sync(&mut cfg, "steam:quiet_game");
    assert_eq!(first.uploaded, vec!["steam:quiet_game"]);
    assert!(first.committed_sha.is_some());
    assert!(!first.pushed_to_remote, "no remote configured");

    let second = sync(&mut cfg, "steam:quiet_game");
    assert!(second.uploaded.is_empty());
    assert_eq!(second.unchanged, vec!["steam:quiet_game"]);
    assert!(second.committed_sha.is_none());
    assert_eq!(list_snapshots(&cfg, "steam:quiet_game", "", 50).unwrap().len(), 1);

    let live = live_state(&cfg, &cfg.games[0]);
    assert_eq!(game_health(&cfg.games[0], false, Some(&live)), GameHealth::Protected);
    write(&dir.join("slot1.sav"), "v2 longer");
    let live = live_state(&cfg, &cfg.games[0]);
    assert_eq!(game_health(&cfg.games[0], false, Some(&live)), GameHealth::ChangesPending);
    assert_eq!(game_health(&cfg.games[0], true, Some(&live)), GameHealth::Running);
}

#[test]
fn saves_travel_between_two_machines_with_safety_backup() {
    let tmp = TempDir::new().unwrap();
    let remote = bare_remote(tmp.path());
    let mut a = machine(tmp.path(), "desktop", Some(&remote));
    let mut b = machine(tmp.path(), "laptop", Some(&remote));

    let a_dir = tmp.path().join("desktop/saves");
    let b_dir = tmp.path().join("laptop/saves");
    write(&a_dir.join("slot1.sav"), "from desktop");
    write(&a_dir.join("sub/extra.dat"), "extra");
    a.games.push(game("Travel Game", a_dir.clone()));
    fs::create_dir_all(&b_dir).unwrap();
    b.games.push(game("Travel Game", b_dir.clone()));

    let out_a = sync(&mut a, "steam:travel_game");
    assert!(out_a.pushed_to_remote, "{:?}", out_a.push_error);

    // The laptop has the game but no saves: they arrive.
    let out_b = sync(&mut b, "steam:travel_game");
    assert_eq!(out_b.downloaded, vec!["steam:travel_game"]);
    assert_eq!(read(&b_dir.join("slot1.sav")), "from desktop");
    assert_eq!(read(&b_dir.join("sub/extra.dat")), "extra");

    // Progress on the laptop goes back to the desktop, which had not changed anything.
    write(&b_dir.join("slot1.sav"), "laptop progress");
    fs::remove_file(b_dir.join("sub/extra.dat")).unwrap();
    let out_b = sync(&mut b, "steam:travel_game");
    assert_eq!(out_b.uploaded, vec!["steam:travel_game"]);
    assert!(out_b.pushed_to_remote);

    let out_a = sync(&mut a, "steam:travel_game");
    assert_eq!(out_a.downloaded, vec!["steam:travel_game"]);
    assert_eq!(read(&a_dir.join("slot1.sav")), "laptop progress");
    assert!(!a_dir.join("sub/extra.dat").exists(), "deletions travel too");

    // The desktop's previous files were zipped before being replaced.
    let backups = list_safety_backups(&a, &a.games[0]);
    assert_eq!(backups.len(), 1);
    let info = read_backup_info(&backups[0].path).unwrap();
    assert_eq!(info.game_id, "steam:travel_game");
    assert_eq!(info.machine, "desktop");
    let undo = tmp.path().join("undo");
    let n = restore_roots_zip(
        &backups[0].path,
        &[SaveRoot {
            id: "saves".into(),
            path: undo.clone(),
            ..Default::default()
        }],
    )
    .unwrap();
    assert_eq!(n, 2);
    assert_eq!(read(&undo.join("slot1.sav")), "from desktop");

    // Snapshots remember which machine made them.
    let timeline = list_snapshots(&a, "steam:travel_game", "", 50).unwrap();
    assert_eq!(timeline[0].machine.as_deref(), Some("laptop"));
    assert_eq!(timeline.last().unwrap().machine.as_deref(), Some("desktop"));

    // Nothing left to do on either side.
    assert_eq!(sync(&mut a, "steam:travel_game").unchanged.len(), 1);
    assert_eq!(sync(&mut b, "steam:travel_game").unchanged.len(), 1);
}

#[test]
fn conflicting_progress_is_never_overwritten_silently() {
    let tmp = TempDir::new().unwrap();
    let remote = bare_remote(tmp.path());
    let mut a = machine(tmp.path(), "desktop", Some(&remote));
    let mut b = machine(tmp.path(), "laptop", Some(&remote));
    let a_dir = tmp.path().join("desktop/saves");
    let b_dir = tmp.path().join("laptop/saves");
    write(&a_dir.join("slot.sav"), "base");
    a.games.push(game("Clash", a_dir.clone()));
    fs::create_dir_all(&b_dir).unwrap();
    b.games.push(game("Clash", b_dir.clone()));
    sync(&mut a, "steam:clash");
    sync(&mut b, "steam:clash");

    // Both play offline-ish, then the desktop syncs first.
    write(&a_dir.join("slot.sav"), "desktop version");
    write(&b_dir.join("slot.sav"), "laptop version!");
    sync(&mut a, "steam:clash");

    let out_b = sync(&mut b, "steam:clash");
    assert_eq!(out_b.conflicts, vec!["steam:clash"]);
    assert!(out_b.uploaded.is_empty() && out_b.downloaded.is_empty());
    assert_eq!(read(&b_dir.join("slot.sav")), "laptop version!", "live saves untouched");
    let conflict = b.games[0].sync_state.conflict.clone().expect("conflict recorded");
    assert_eq!(conflict.cloud_machine.as_deref(), Some("desktop"));
    assert_eq!(game_health(&b.games[0], false, None), GameHealth::Conflict);

    // Syncing again keeps asking instead of picking a side.
    let again = sync(&mut b, "steam:clash");
    assert_eq!(again.conflicts, vec!["steam:clash"]);
    assert_eq!(read(&b_dir.join("slot.sav")), "laptop version!");

    // Choosing the cloud version keeps the laptop's files in a safety zip.
    let restored = resolve_conflict_use_cloud(&mut b, "steam:clash", true).unwrap();
    assert_eq!(restored, 1);
    assert_eq!(read(&b_dir.join("slot.sav")), "desktop version");
    assert!(b.games[0].sync_state.conflict.is_none());
    let backups = list_safety_backups(&b, &b.games[0]);
    assert!(!backups.is_empty());
    let undo = tmp.path().join("undo");
    restore_roots_zip(
        &backups[0].path,
        &[SaveRoot {
            id: "saves".into(),
            path: undo.clone(),
            ..Default::default()
        }],
    )
    .unwrap();
    assert_eq!(read(&undo.join("slot.sav")), "laptop version!");

    // The other direction: keep this machine's saves.
    write(&a_dir.join("slot.sav"), "desktop again");
    write(&b_dir.join("slot.sav"), "laptop insists");
    sync(&mut a, "steam:clash");
    assert_eq!(sync(&mut b, "steam:clash").conflicts.len(), 1);
    let kept = resolve_conflict_keep_local(&mut b, "steam:clash", true).unwrap();
    assert_eq!(kept.uploaded, vec!["steam:clash"]);
    assert!(kept.pushed_to_remote, "{:?}", kept.push_error);
    assert_eq!(read(&b_dir.join("slot.sav")), "laptop insists");
    assert_eq!(sync(&mut a, "steam:clash").downloaded.len(), 1);
    assert_eq!(read(&a_dir.join("slot.sav")), "laptop insists");
}

#[test]
fn conflict_policy_can_prefer_a_side_automatically() {
    let tmp = TempDir::new().unwrap();
    let remote = bare_remote(tmp.path());
    let mut a = machine(tmp.path(), "a", Some(&remote));
    let mut b = machine(tmp.path(), "b", Some(&remote));
    b.conflict_policy = ConflictPolicy::PreferCloud;
    let a_dir = tmp.path().join("a/saves");
    let b_dir = tmp.path().join("b/saves");
    write(&a_dir.join("s.sav"), "base");
    a.games.push(game("Auto", a_dir.clone()));
    fs::create_dir_all(&b_dir).unwrap();
    b.games.push(game("Auto", b_dir.clone()));
    sync(&mut a, "steam:auto");
    sync(&mut b, "steam:auto");

    write(&a_dir.join("s.sav"), "cloud side");
    write(&b_dir.join("s.sav"), "local side!");
    sync(&mut a, "steam:auto");
    let out = sync(&mut b, "steam:auto");
    assert_eq!(out.downloaded, vec!["steam:auto"]);
    assert_eq!(read(&b_dir.join("s.sav")), "cloud side");
    assert_eq!(list_safety_backups(&b, &b.games[0]).len(), 1);

    b.conflict_policy = ConflictPolicy::PreferLocal;
    write(&a_dir.join("s.sav"), "cloud side 2");
    write(&b_dir.join("s.sav"), "local side 2!");
    sync(&mut a, "steam:auto");
    let out = sync(&mut b, "steam:auto");
    assert_eq!(out.uploaded, vec!["steam:auto"]);
    assert_eq!(read(&b_dir.join("s.sav")), "local side 2!");
}

#[test]
fn offline_snapshots_on_both_machines_are_combined_per_game() {
    let tmp = TempDir::new().unwrap();
    let remote = bare_remote(tmp.path());
    let mut a = machine(tmp.path(), "a", Some(&remote));
    let mut b = machine(tmp.path(), "b", Some(&remote));
    for (cfg, m) in [(&mut a, "a"), (&mut b, "b")] {
        for g in ["One", "Two"] {
            let dir = tmp.path().join(m).join(g);
            fs::create_dir_all(&dir).unwrap();
            cfg.games.push(game(g, dir));
        }
    }
    write(&tmp.path().join("a/One/s.sav"), "one base");
    write(&tmp.path().join("a/Two/s.sav"), "two base");
    let ids = vec!["steam:one".to_string(), "steam:two".to_string()];
    let all = |cfg: &mut AppConfig, remote: bool| {
        sync_games(
            cfg,
            &ids,
            SyncOptions {
                use_remote: remote,
                ..Default::default()
            },
        )
        .unwrap()
    };
    all(&mut a, true);
    assert_eq!(all(&mut b, true).downloaded.len(), 2);

    // Each machine snapshots a different game without reaching the cloud.
    write(&tmp.path().join("a/One/s.sav"), "one from a");
    write(&tmp.path().join("b/Two/s.sav"), "two from b");
    all(&mut a, false);
    all(&mut b, false);

    // A pushes; B has diverged history but touched a different game: both survive.
    assert!(all(&mut a, true).pushed_to_remote);
    let out_b = all(&mut b, true);
    assert!(out_b.conflicts.is_empty(), "{:?}", out_b.conflicts);
    assert!(out_b.pushed_to_remote, "{:?}", out_b.push_error);
    assert_eq!(read(&tmp.path().join("b/One/s.sav")), "one from a");
    assert_eq!(read(&tmp.path().join("b/Two/s.sav")), "two from b");
    let out_a = all(&mut a, true);
    assert_eq!(out_a.downloaded, vec!["steam:two"]);
    assert_eq!(read(&tmp.path().join("a/Two/s.sav")), "two from b");
}

#[test]
fn new_machine_learns_games_from_cloud_catalog_and_restores_on_install() {
    let tmp = TempDir::new().unwrap();
    let remote = bare_remote(tmp.path());
    let mut a = machine(tmp.path(), "old-pc", Some(&remote));
    let a_dir = tmp.path().join("old-pc/saves");
    write(&a_dir.join("hero.sav"), "level 40");
    let mut g = game("Catalog Quest", a_dir);
    g.save_dir_template = Some("<xdgConfig>/CatalogQuest".to_string());
    a.games.push(g);
    sync(&mut a, "steam:catalog_quest");

    let mut b = machine(tmp.path(), "new-pc", Some(&remote));
    let added = pull_cloud_catalog(&mut b).unwrap();
    assert_eq!(added, vec!["steam:catalog_quest"]);
    let placeholder = &b.games[0];
    assert!(placeholder.is_placeholder());
    assert!(!placeholder.enabled);
    assert_eq!(placeholder.detection.as_ref().unwrap().source, DetectionSource::Cloud);
    assert_eq!(game_health(placeholder, false, None), GameHealth::NotInstalled);

    // Syncing a placeholder is a harmless no-op.
    let out = sync(&mut b, "steam:catalog_quest");
    assert_eq!(out.skipped.len(), 1);

    // Once the game is located (here: by hand), its saves come down.
    let b_dir = tmp.path().join("new-pc/saves");
    fs::create_dir_all(&b_dir).unwrap();
    b.games[0].save_dir = b_dir.clone();
    b.games[0].enabled = true;
    let out = sync(&mut b, "steam:catalog_quest");
    assert_eq!(out.downloaded, vec!["steam:catalog_quest"]);
    assert_eq!(read(&b_dir.join("hero.sav")), "level 40");

    // The catalog round-trips: the new PC's metadata still lists the game.
    assert!(pull_cloud_catalog(&mut a).unwrap().is_empty());
}

#[test]
fn reinstalled_game_gets_its_saves_back_and_missing_drive_is_left_alone() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = machine(tmp.path(), "pc", None);
    let prefix = tmp.path().join("drive/compatdata/5/pfx");
    let dir = prefix.join("drive_c/users/steamuser/Saved Games/Thing");
    write(&dir.join("a.sav"), "precious");
    let mut g = game("Thing", dir.clone());
    g.prefix_dir = Some(prefix.clone());
    cfg.games.push(g);
    sync(&mut cfg, "steam:thing");

    // Prefix wiped by a reinstall: the empty folder must not be "synced" as a deletion.
    fs::remove_dir_all(&dir).unwrap();
    let out = sync(&mut cfg, "steam:thing");
    assert_eq!(out.downloaded, vec!["steam:thing"]);
    assert_eq!(read(&dir.join("a.sav")), "precious");

    // Drive unplugged: nothing is recreated elsewhere and the snapshot stays intact.
    let parked = tmp.path().join("unplugged");
    fs::rename(tmp.path().join("drive"), &parked).unwrap();
    let out = sync(&mut cfg, "steam:thing");
    assert_eq!(out.skipped.len(), 1);
    assert!(!dir.exists());
    assert_eq!(game_health(&cfg.games[0], false, None), GameHealth::Unavailable);
    let repo_file = cfg
        .local_repo_path
        .join(repo_subpath_for_game(&cfg.games[0], true))
        .join("a.sav");
    assert_eq!(read(&repo_file), "precious");
}

#[test]
fn profiles_keep_separate_characters_for_single_slot_games() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = machine(tmp.path(), "pc", None);
    let dir = tmp.path().join("saves");
    write(&dir.join("character.sav"), "warrior lvl 30");
    cfg.games.push(game("One Slot", dir.clone()));
    sync(&mut cfg, "steam:one_slot");
    rename_profile(&mut cfg, "steam:one_slot", "", "Warrior").unwrap();

    // A fresh profile empties the save folder so the game starts a new character.
    let mage = create_profile(&mut cfg, "steam:one_slot", "Mage", "magic only run", ProfileStart::Empty).unwrap();
    assert_eq!(mage, "mage");
    assert!(dir.join("character.sav").exists(), "creating does not touch live files");
    switch_profile(&mut cfg, "steam:one_slot", &mage, false).unwrap();
    assert!(!dir.join("character.sav").exists());
    assert_eq!(cfg.games[0].active_profile, "mage");

    write(&dir.join("character.sav"), "mage lvl 5");
    sync(&mut cfg, "steam:one_slot");

    // Unsaved progress on the outgoing profile is captured automatically when switching.
    write(&dir.join("character.sav"), "mage lvl 6");
    let n = switch_profile(&mut cfg, "steam:one_slot", "", false).unwrap();
    assert_eq!(n, 1);
    assert_eq!(read(&dir.join("character.sav")), "warrior lvl 30");
    switch_profile(&mut cfg, "steam:one_slot", &mage, false).unwrap();
    assert_eq!(read(&dir.join("character.sav")), "mage lvl 6");

    // Repository layout and independent timelines.
    let game_dir = cfg.local_repo_path.join("steam/one_slot");
    assert_eq!(read(&game_dir.join("character.sav")), "warrior lvl 30");
    assert_eq!(read(&game_dir.join(PROFILES_DIR).join("mage/character.sav")), "mage lvl 6");
    let main_tl = list_snapshots(&cfg, "steam:one_slot", "", 50).unwrap();
    let mage_tl = list_snapshots(&cfg, "steam:one_slot", &mage, 50).unwrap();
    assert!(main_tl.iter().all(|c| c.changed_files.iter().all(|f| !f.contains(PROFILES_DIR))));
    assert_eq!(mage_tl.len(), 2);

    let summaries = profile_summaries(&cfg, "steam:one_slot");
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].name, "Warrior");
    assert!(summaries[1].active);
    assert_eq!(summaries[1].note, "magic only run");

    // A copy of the current profile, and one branched from an old snapshot.
    let copy = create_profile(&mut cfg, "steam:one_slot", "Mage (evil ending)", "", ProfileStart::CopyOfCurrent).unwrap();
    assert_eq!(read(&game_dir.join(PROFILES_DIR).join(&copy).join("character.sav")), "mage lvl 6");
    let old = mage_tl.last().unwrap().commit_hash.clone();
    let branched = create_profile(
        &mut cfg,
        "steam:one_slot",
        "Mage early",
        "",
        ProfileStart::FromSnapshot {
            profile: mage.clone(),
            commit: old,
        },
    )
    .unwrap();
    switch_profile(&mut cfg, "steam:one_slot", &branched, false).unwrap();
    assert_eq!(read(&dir.join("character.sav")), "mage lvl 5");

    // Guard rails.
    assert!(create_profile(&mut cfg, "steam:one_slot", "mage", "", ProfileStart::Empty).is_err());
    assert!(delete_profile(&mut cfg, "steam:one_slot", &branched).is_err(), "active profile");
    assert!(delete_profile(&mut cfg, "steam:one_slot", "").is_err(), "main profile");
    delete_profile(&mut cfg, "steam:one_slot", &copy).unwrap();
    assert!(!game_dir.join(PROFILES_DIR).join(&copy).exists());
    assert_eq!(cfg.games[0].profiles.len(), 2);

    // Switching always leaves a safety zip behind.
    assert!(!list_safety_backups(&cfg, &cfg.games[0]).is_empty());
}

#[test]
fn profiles_sync_between_machines_without_cross_talk() {
    let tmp = TempDir::new().unwrap();
    let remote = bare_remote(tmp.path());
    let mut a = machine(tmp.path(), "a", Some(&remote));
    let mut b = machine(tmp.path(), "b", Some(&remote));
    let a_dir = tmp.path().join("a/saves");
    let b_dir = tmp.path().join("b/saves");
    write(&a_dir.join("c.sav"), "main");
    a.games.push(game("Shared", a_dir.clone()));
    fs::create_dir_all(&b_dir).unwrap();
    b.games.push(game("Shared", b_dir.clone()));
    sync(&mut a, "steam:shared");

    let alt = create_profile(&mut a, "steam:shared", "Alt", "", ProfileStart::Empty).unwrap();
    switch_profile(&mut a, "steam:shared", &alt, true).unwrap();
    write(&a_dir.join("c.sav"), "alt on a");
    sync(&mut a, "steam:shared");

    // B stays on the main profile: it receives main's saves and learns about "Alt".
    let out = sync(&mut b, "steam:shared");
    assert_eq!(out.downloaded, vec!["steam:shared"]);
    assert_eq!(read(&b_dir.join("c.sav")), "main");
    assert_eq!(b.games[0].profiles.len(), 1);
    assert_eq!(b.games[0].profiles[0].name, "Alt");

    // A's later Alt progress does not disturb B's main profile.
    write(&a_dir.join("c.sav"), "alt on a v2");
    sync(&mut a, "steam:shared");
    let out = sync(&mut b, "steam:shared");
    assert!(out.downloaded.is_empty() && out.conflicts.is_empty());
    assert_eq!(read(&b_dir.join("c.sav")), "main");

    // B can switch to Alt and gets A's character.
    switch_profile(&mut b, "steam:shared", &alt, true).unwrap();
    assert_eq!(read(&b_dir.join("c.sav")), "alt on a v2");
}

#[test]
fn extra_roots_and_mod_fingerprints_are_part_of_every_snapshot() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = machine(tmp.path(), "pc", None);
    let saves = tmp.path().join("Documents/Modded/Saves");
    let settings = tmp.path().join("Documents/Modded/Settings");
    let mods = tmp.path().join("install/Mods");
    write(&saves.join("quick.ess"), "save A");
    write(&saves.join("autosave1.ess"), "auto");
    write(&settings.join("game.ini"), "shadows=high");
    write(&settings.join("Logs/run.log"), "noise");
    fs::create_dir_all(mods.join("SkyUI")).unwrap();
    fs::create_dir_all(mods.join("USSEP")).unwrap();

    let mut g = game("Modded", saves.clone());
    g.exclude_globs = vec!["autosave*".to_string()];
    g.extra_roots.push(SaveRoot {
        id: "config".into(),
        label: "Settings".into(),
        kind: RootKind::Config,
        path: settings.clone(),
        enabled: true,
        shared_across_profiles: true,
        ..Default::default()
    });
    g.mod_sources.push(ModSource {
        label: "Mods".into(),
        path: mods.clone(),
        kind: ModSourceKind::Folder,
        enabled: true,
    });
    cfg.games.push(g);

    let first = sync(&mut cfg, "steam:modded").committed_sha.unwrap();
    let game_dir = cfg.local_repo_path.join("steam/modded");
    assert!(game_dir.join("quick.ess").exists());
    assert!(!game_dir.join("autosave1.ess").exists(), "excluded by pattern");
    assert_eq!(read(&game_dir.join(ROOTS_DIR).join("config/game.ini")), "shadows=high");
    assert!(!game_dir.join(ROOTS_DIR).join("config/Logs").exists(), "transient folder skipped");
    assert!(game_dir.join(SNAPSHOT_META_FILE).exists());

    // Change saves, settings and the mod list.
    write(&saves.join("quick.ess"), "save B!");
    write(&settings.join("game.ini"), "shadows=low");
    fs::remove_dir(mods.join("SkyUI")).unwrap();
    fs::create_dir_all(mods.join("Requiem")).unwrap();
    sync(&mut cfg, "steam:modded");

    let then = read_snapshot_meta(&cfg, &cfg.games[0], "", &first).unwrap().mods.unwrap();
    assert_eq!(then.entries, vec!["Mods: SkyUI", "Mods: USSEP"]);
    let now = read_snapshot_meta(&cfg, &cfg.games[0], "", "HEAD").unwrap().mods.unwrap();
    assert_eq!(now.entries, vec!["Mods: Requiem", "Mods: USSEP"]);

    // Preview, then restore the first snapshot: both roots go back, excluded files stay.
    let preview = preview_restore(&cfg, "steam:modded", &first).unwrap();
    assert_eq!(preview.will_replace.len(), 2);
    assert!(preview.will_add.is_empty() && preview.will_remove.is_empty());
    assert_eq!(preview.snapshot_mods.unwrap().entries.len(), 2);

    rollback_game_to_commit(&mut cfg, "steam:modded", &first, false).unwrap();
    assert_eq!(read(&saves.join("quick.ess")), "save A");
    assert_eq!(read(&settings.join("game.ini")), "shadows=high");
    assert_eq!(read(&saves.join("autosave1.ess")), "auto");
    assert_eq!(read(&settings.join("Logs/run.log")), "noise");

    // Shared roots are not swapped by profile switches; per-profile roots are.
    let alt = create_profile(&mut cfg, "steam:modded", "Vanilla", "", ProfileStart::Empty).unwrap();
    switch_profile(&mut cfg, "steam:modded", &alt, false).unwrap();
    assert!(!saves.join("quick.ess").exists());
    assert_eq!(read(&saves.join("autosave1.ess")), "auto", "excluded files are never touched");
    assert_eq!(read(&settings.join("game.ini")), "shadows=high");
}

#[test]
fn removing_a_game_from_the_cloud_pauses_it_elsewhere_without_touching_saves() {
    let tmp = TempDir::new().unwrap();
    let remote = bare_remote(tmp.path());
    let mut a = machine(tmp.path(), "a", Some(&remote));
    let mut b = machine(tmp.path(), "b", Some(&remote));
    let a_dir = tmp.path().join("a/saves");
    let b_dir = tmp.path().join("b/saves");
    write(&a_dir.join("s.sav"), "data");
    a.games.push(game("Gone", a_dir));
    fs::create_dir_all(&b_dir).unwrap();
    b.games.push(game("Gone", b_dir.clone()));
    sync(&mut a, "steam:gone");
    sync(&mut b, "steam:gone");

    let removed = remove_game_entry(&mut a, "steam:gone", true, true).unwrap();
    assert!(removed.pushed_to_remote, "{:?}", removed.push_error);

    let out = sync(&mut b, "steam:gone");
    assert!(out.downloaded.is_empty() && out.uploaded.is_empty());
    assert_eq!(read(&b_dir.join("s.sav")), "data");
    assert!(!b.games[0].enabled);
    assert!(b.games[0].last_sync_status.as_deref().unwrap().contains("removed"));
}

const DB_FIXTURE: &str = r#"
"Elden Ring":
  files:
    "<winAppData>/EldenRing/<storeUserId>":
      tags: [save]
      when: [{os: windows}]
    "<winAppData>/EldenRing/GraphicsConfig.xml":
      tags: [config]
      when: [{os: windows}]
  installDir: {"ELDEN RING": {}}
  steam: {id: 1245620}
"The Witcher 3: Wild Hunt":
  files:
    "<winDocuments>/The Witcher 3/*.settings":
      tags: [config]
      when: [{os: windows}]
    "<winDocuments>/The Witcher 3/gamesaves/*.sav":
      tags: [save]
      when: [{os: windows}]
    "<winDocuments>/The Witcher 3/gamesaves/*.png":
      tags: [save]
      when: [{os: windows}]
  steam: {id: 292030}
"Red Dead Redemption 2":
  files:
    "<winDocuments>/Rockstar Games/Red Dead Redemption 2/Profiles/<storeUserId>":
      tags: [save]
      when: [{os: windows}]
    "<winDocuments>/Rockstar Games/Red Dead Redemption 2/Settings":
      tags: [config]
      when: [{os: windows}]
  steam: {id: 1174180}
"Cloud Only":
  files:
    "<root>/userdata/<storeUserId>/555/remote":
      tags: [save]
      when: [{store: steam}]
  steam: {id: 555}
"Unplayed":
  files:
    "<winLocalAppData>/Unplayed/Saved/SaveGames":
      tags: [save]
      when: [{os: windows}]
  steam: {id: 777}
"#;

fn fake_steam(root: &Path, apps: &[(&str, &str, &str)]) -> Vec<ProviderLibraryConfig> {
    let steamapps = root.join("steamapps");
    fs::create_dir_all(steamapps.join("common")).unwrap();
    fs::create_dir_all(root.join("userdata")).unwrap();
    for (appid, name, installdir) in apps {
        write(
            &steamapps.join(format!("appmanifest_{appid}.acf")),
            &format!(
                "\"AppState\"\n{{\n\t\"appid\"\t\t\"{appid}\"\n\t\"name\"\t\t\"{name}\"\n\t\"installdir\"\t\t\"{installdir}\"\n}}\n"
            ),
        );
        fs::create_dir_all(steamapps.join("common").join(installdir)).unwrap();
        fs::create_dir_all(
            steamapps
                .join("compatdata")
                .join(appid)
                .join("pfx/drive_c/users/steamuser"),
        )
        .unwrap();
    }
    vec![ProviderLibraryConfig {
        provider: Provider::Steam,
        library_paths: vec![root.to_path_buf()],
        enabled: true,
    }]
}

#[test]
fn detection_finds_exact_save_locations_from_database_cloud_and_smart_scan() {
    let tmp = TempDir::new().unwrap();
    let steam = tmp.path().join("Steam");
    let libs = fake_steam(
        &steam,
        &[
            ("1245620", "ELDEN RING", "ELDEN RING"),
            ("292030", "The Witcher 3: Wild Hunt", "The Witcher 3"),
            ("1174180", "Red Dead Redemption 2", "Red Dead Redemption 2"),
            ("555", "Cloud Only", "CloudOnly"),
            ("777", "Unplayed", "Unplayed"),
            ("888", "Indie Mystery", "IndieMystery"),
            ("999", "Never Launched", "NeverLaunched"),
            ("228980", "Steamworks Common Redistributables", "Steamworks Shared"),
        ],
    );
    let user = |appid: &str| steam.join(format!("steamapps/compatdata/{appid}/pfx/drive_c/users/steamuser"));

    // Elden Ring: lowercase folder on disk, Steam-id subfolder.
    write(&user("1245620").join("AppData/Roaming/eldenring/76561198000000000/ER0000.sl2"), "er");
    write(&user("1245620").join("AppData/Roaming/eldenring/GraphicsConfig.xml"), "<x/>");
    // Witcher 3: saves next to unrelated files.
    write(&user("292030").join("Documents/The Witcher 3/gamesaves/Quick.sav"), "w3");
    write(&user("292030").join("Documents/The Witcher 3/gamesaves/Quick.png"), "png");
    write(&user("292030").join("Documents/The Witcher 3/user.settings"), "cfg");
    write(&user("292030").join("Documents/The Witcher 3/crash.dmp"), "junk");
    // RDR2: launcher junk beside the real profile folder.
    write(&user("1174180").join("Documents/Rockstar Games/Red Dead Redemption 2/Profiles/ABCD1234/SRDR30000"), "rdr");
    write(&user("1174180").join("Documents/Rockstar Games/Red Dead Redemption 2/Settings/system.xml"), "cfg");
    write(&user("1174180").join("Documents/Rockstar Games/Launcher/launcher.log"), "junk");
    write(&user("1174180").join("Documents/Rockstar Games/Social Club/Profiles/x.dat"), "junk");
    // Cloud-only game.
    write(&steam.join("userdata/42/555/remote/save.dat"), "cloud");
    // Unknown to the database, but its prefix has an obvious save folder.
    write(&user("888").join("AppData/LocalLow/Tiny Team/Indie Mystery/slot0.save"), "indie");

    let db = SaveDb::from_manifest_yaml(DB_FIXTURE.as_bytes()).unwrap();
    let scanned = scan_installed_games(&libs, Some(&db), false);
    let by_id = |id: &str| scanned.iter().find(|(g, _)| g.id == id).unwrap_or_else(|| panic!("{id} missing"));
    assert!(!scanned.iter().any(|(g, _)| g.name.contains("Redistributables")));

    let (er, det) = by_id("steam:elden_ring");
    assert_eq!(er.save_dir, user("1245620").join("AppData/Roaming/eldenring"));
    assert_eq!(er.sync_selection, SyncSelectionMode::EntireFolder);
    assert_eq!(er.save_dir_template.as_deref(), Some("<winAppData>/EldenRing"));
    assert_eq!(det.info.source, DetectionSource::Database);
    assert!(det.info.confidence >= 90 && er.enabled);
    assert!(er.prefix_dir.as_ref().unwrap().ends_with("compatdata/1245620/pfx"));

    let (w3, _) = by_id("steam:the_witcher_3_wild_hunt");
    assert_eq!(w3.save_dir, user("292030").join("Documents/The Witcher 3/gamesaves"));
    assert_eq!(
        w3.sync_selection,
        SyncSelectionMode::SelectedFiles(vec!["*.png".to_string(), "*.sav".to_string()])
    );
    let cfg_root = w3.extra_roots.iter().find(|r| r.kind == RootKind::Config).unwrap();
    assert_eq!(cfg_root.path, user("292030").join("Documents/The Witcher 3"));
    assert_eq!(cfg_root.include, vec!["*.settings"]);
    assert!(!cfg_root.enabled, "settings are opt-in");
    assert!(cfg_root.shared_across_profiles);

    let (rdr, _) = by_id("steam:red_dead_redemption_2");
    assert_eq!(
        rdr.save_dir,
        user("1174180").join("Documents/Rockstar Games/Red Dead Redemption 2/Profiles")
    );

    let (cloud, det) = by_id("steam:cloud_only");
    assert_eq!(cloud.save_dir, steam.join("userdata/42/555/remote"));
    assert_eq!(det.info.source, DetectionSource::Database);
    assert_eq!(
        cloud.save_dir_template.as_deref(),
        Some("<root>/userdata/<storeUserId>/555/remote")
    );

    let (unplayed, det) = by_id("steam:unplayed");
    assert_eq!(unplayed.save_dir, user("777").join("AppData/Local/Unplayed/Saved/SaveGames"));
    assert!(!unplayed.save_dir.exists());
    assert_eq!(det.info.confidence, 70);
    assert!(unplayed.enabled, "known location: tracked as soon as saves appear");

    let (indie, det) = by_id("steam:indie_mystery");
    assert_eq!(indie.save_dir, user("888").join("AppData/LocalLow/Tiny Team/Indie Mystery"));
    assert_eq!(det.info.source, DetectionSource::Heuristic);
    assert!(det.info.confidence >= AUTO_ENABLE_CONFIDENCE, "{}", det.info.confidence);

    let (never, det) = by_id("steam:never_launched");
    assert!(never.is_placeholder());
    assert_eq!(det.info.confidence, 0);
    assert!(!never.enabled);

    // Merging: new games are added; an older, sloppier location becomes a suggestion only.
    let mut cfg = machine(tmp.path(), "pc", None);
    let mut legacy = game("Red Dead Redemption 2", user("1174180").join("Documents/Rockstar Games"));
    legacy.app_id = Some("1174180".into());
    cfg.games.push(legacy);
    let summary = merge_scan_results(&mut cfg, scanned);
    assert_eq!(summary.installed, 7);
    assert_eq!(summary.added.len(), 6);
    assert_eq!(summary.needs_review, vec!["steam:never_launched"]);
    assert!(summary.suggestions.contains_key("steam:red_dead_redemption_2"));
    let kept = cfg.games.iter().find(|g| g.id == "steam:red_dead_redemption_2").unwrap();
    assert!(kept.save_dir.ends_with("Rockstar Games"), "existing choice is not changed silently");
    assert!(kept.prefix_dir.is_some(), "install facts are refreshed");

    // With automatic tracking off, new games are only offered; ignored ones are not even that.
    let mut curated = machine(tmp.path(), "curated", None);
    curated.auto_track_new_games = false;
    curated.ignored_game_ids.push("steam:cloud_only".to_string());
    let offered = merge_scan_results(&mut curated, scan_installed_games(&libs, Some(&db), false));
    assert!(curated.games.is_empty() && offered.added.is_empty());
    assert_eq!(offered.untracked.len(), 6);
    assert!(!offered.untracked.iter().any(|g| g.id == "steam:cloud_only"));

    // Applying the suggestion on request moves it to the precise folder.
    let det = summary.suggestions["steam:red_dead_redemption_2"].clone();
    let g = cfg.games.iter_mut().find(|g| g.id == "steam:red_dead_redemption_2").unwrap();
    apply_detection(g, &det, false);
    assert!(g.save_dir.ends_with("Red Dead Redemption 2/Profiles"));

    // A hand-picked location is never overridden.
    g.detection.as_mut().unwrap().source = DetectionSource::Manual;
    g.save_dir = PathBuf::from("/somewhere/else");
    let again = detect_game(g, Some(&db));
    apply_detection(g, &again, false);
    assert_eq!(g.save_dir, PathBuf::from("/somewhere/else"));
}

#[test]
fn cloud_placeholder_is_located_by_scan_and_restored_into_the_right_prefix() {
    let tmp = TempDir::new().unwrap();
    let remote = bare_remote(tmp.path());
    let db = SaveDb::from_manifest_yaml(DB_FIXTURE.as_bytes()).unwrap();

    // Old PC: Elden Ring detected and synced.
    let steam_a = tmp.path().join("a/Steam");
    let libs_a = fake_steam(&steam_a, &[("1245620", "ELDEN RING", "ELDEN RING")]);
    write(
        &steam_a.join("steamapps/compatdata/1245620/pfx/drive_c/users/steamuser/AppData/Roaming/EldenRing/765/ER0000.sl2"),
        "200 hours",
    );
    let mut a = machine(tmp.path(), "a", Some(&remote));
    a.provider_libraries = libs_a.clone();
    merge_scan_results(&mut a, scan_installed_games(&libs_a, Some(&db), false));
    assert!(sync(&mut a, "steam:elden_ring").pushed_to_remote);

    // New PC: nothing installed yet, catalog pulled.
    let mut b = machine(tmp.path(), "b", Some(&remote));
    assert_eq!(pull_cloud_catalog(&mut b).unwrap(), vec!["steam:elden_ring"]);

    // The game gets installed (different library path, never launched), then a scan runs.
    let steam_b = tmp.path().join("b/SteamLibrary");
    let libs_b = fake_steam(&steam_b, &[("1245620", "ELDEN RING", "ELDEN RING")]);
    b.provider_libraries = libs_b.clone();
    let summary = merge_scan_results(&mut b, scan_installed_games(&libs_b, Some(&db), false));
    assert_eq!(summary.located, vec!["steam:elden_ring"]);
    assert!(summary.added.is_empty());
    let g = &b.games[0];
    assert!(g.enabled && !g.is_placeholder());
    let expected = steam_b.join("steamapps/compatdata/1245620/pfx/drive_c/users/steamuser/AppData/Roaming/EldenRing");
    assert_eq!(g.save_dir, expected);

    let out = sync(&mut b, "steam:elden_ring");
    assert_eq!(out.downloaded, vec!["steam:elden_ring"]);
    assert_eq!(read(&expected.join("765/ER0000.sl2")), "200 hours");
}

#[test]
fn legacy_config_is_migrated_with_backup_and_secrets_leave_the_config_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.json");
    let legacy = serde_json::json!({
        "repo_name": "save_game_manager",
        "local_repo_path": tmp.path().join("repo"),
        "auth": {"PersonalAccessToken": {"token": "ghp_legacysecret", "github_username": "someone"}},
        "separate_folder_per_provider": true,
        "global_sync_rate": "OnStartup",
        "split_large_saves": true,
        "lfs_warning_threshold_mb": 95,
        "chunk_size_mb": 45,
        "provider_libraries": [
            {"provider": "Steam", "library_paths": [], "enabled": true}
        ],
        "games": [{
            "id": "steam:old_game", "name": "Old Game", "game_slug": "old_game",
            "provider": "Steam", "app_id": "1", "install_dir": null,
            "save_dir": tmp.path().join("old_saves"), "enabled": true,
            "sync_selection": "EntireFolder", "sync_rate_override": null,
            "sync_across_providers": false, "last_synced_at": null,
            "last_sync_status": "Synced 2 file(s)"
        }],
        "theme_mode": "Dark",
        "sync_enabled": true
    });
    fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

    let cfg = load_config(&path).unwrap();
    assert_eq!(cfg.schema_version, 2);
    assert!(cfg.onboarding_done, "existing setups skip first-run");
    assert!(!cfg.machine_name.is_empty());
    assert!(cfg.safety_backups && cfg.backup_on_game_exit && cfg.auto_update_detection_db);
    assert!(!cfg.auto_track_new_games, "a hand-curated list is not extended silently");
    assert_eq!(cfg.games[0].active_profile, "");
    assert!(cfg.provider_libraries.iter().any(|l| l.provider == Provider::Lutris));
    assert_eq!(cfg.auth.pat_token(), Some("ghp_legacysecret"));

    // The token is gone from config.json and lives in a private secrets file.
    let on_disk = fs::read_to_string(&path).unwrap();
    assert!(!on_disk.contains("ghp_legacysecret"));
    let secrets = tmp.path().join("secrets.json");
    assert!(fs::read_to_string(&secrets).unwrap().contains("ghp_legacysecret"));
    assert!(fs::read_to_string(tmp.path().join("config.json.v0.bak")).unwrap().contains("ghp_legacysecret"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in [&path, &secrets, &tmp.path().join("config.json.v0.bak")] {
            assert_eq!(fs::metadata(f).unwrap().permissions().mode() & 0o777, 0o600, "{}", f.display());
        }
    }

    // Round-trips, and clearing the token removes the secrets file.
    let mut reloaded = load_config(&path).unwrap();
    assert_eq!(reloaded, cfg);
    if let AuthMethod::PersonalAccessToken { token, .. } = &mut reloaded.auth {
        token.clear();
    }
    save_config(&path, &reloaded).unwrap();
    assert!(!secrets.exists());
}

#[test]
fn git_credentials_travel_in_the_environment_and_only_to_github() {
    let mut cmd = Command::new("git");
    apply_auth_to_git_command(
        &mut cmd,
        &AuthMethod::PersonalAccessToken {
            token: "ghp_topsecret".into(),
            github_username: "u".into(),
        },
    );
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert!(args.is_empty(), "nothing secret on the command line: {args:?}");
    let envs: Vec<(String, String)> = cmd
        .get_envs()
        .filter_map(|(k, v)| Some((k.to_string_lossy().into_owned(), v?.to_string_lossy().into_owned())))
        .collect();
    let key = envs.iter().find(|(k, _)| k == "GIT_CONFIG_KEY_0").unwrap();
    assert_eq!(key.1, "http.https://github.com/.extraheader");
    let value = envs.iter().find(|(k, _)| k == "GIT_CONFIG_VALUE_0").unwrap();
    assert!(value.1.starts_with("Authorization: Basic "));
    assert!(!value.1.contains("ghp_topsecret"), "token is encoded, not literal");
}
