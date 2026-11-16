# SaveVault (`save_game_manager`) — Exported User Requirements

This document exports the complete set of functional, UX, reliability, and packaging requirements specified for **SaveVault** (`save_game_manager`), organized chronologically by milestone together with implementation & verification traceability.

---

## Chronological Requirements Export

### Phase 1 — Core Git Save Sync & Multi-Provider Manager
- **REQ-01 — Lightweight Desktop Rust Application**: Build a lightweight desktop Rust application (`eframe` / `egui`) that synchronizes game saves to a Git repository so users can easily browse history and roll back game saves themselves.
- **REQ-02 — Flexible GitHub Authentication**: Support connecting to a GitHub account using any of:
  1. An **existing SSH key** (`~/.ssh/id_ed25519`, `id_rsa`, or user-specified path),
  2. Creating a **new Ed25519 SSH keypair** directly from the application (with optional PAT upload / copyable public key), or
  3. A **Personal Access Token (PAT)** for HTTPS authentication.
- **REQ-03 — Automatic Private Repository Creation**: Automatically check for and create a private GitHub repository named `save_game_manager` (configurable) if it does not exist yet.
- **REQ-04 — Per-Provider / Per-Game Folder Layout**: Provide an option to organize the save repository with a separate folder per provider and per game (for example `epic_games/metro_exodus` and `steam/metro_exodus`), or a flat per-game layout (`metro_exodus`).
- **REQ-05 — Cross-Provider Save Synchronization**: When the same game appears across multiple storefronts/providers, provide a toggle to keep their save files synchronized across providers automatically (propagating the newest save state).
- **REQ-06 — Global & Per-Game Sync Rates**: Allow configuring the synchronization schedule both globally and per game (`On Startup` default, `Manual Only`, or `Every N Minutes`).
- **REQ-07 — Large Save Splitting & Warnings**: Warn the user when save files approach or exceed GitHub / GitHub LFS size thresholds, and provide an option to split oversized files into verifiable SHA-256 chunks (`.sgm_part.NNN` + `.sgm_manifest.json`) that reassemble transparently during rollback.
- **REQ-08 — Platform-Aware Multi-Provider Library Discovery**: Support **Steam**, **GOG**, and **Epic Games** game/save locations, automatically detecting platform-appropriate default library paths on Linux, Windows, and macOS while allowing custom paths.
- **REQ-09 — Entire Folder vs. Selected Files Sync**: Allow each game to sync either its entire save directory or a specific user-selected subset of files/subdirectories.

### Phase 2 — Theme, Save State Labels, System Reset Re-instatement & Save Imports
- **REQ-10 — `jizihub`-Inspired Visual Theme**: Stylize the desktop application using the visual language inspired by `~/Documents/jizihub` (Smoked Obsidian dark palette, Warm Sandstone light palette, ambient radial orbs, background grid, and 3-tier frosted glass panels).
- **REQ-11 — Custom Save State Labels**: Allow users to attach descriptive labels to any game's save state during sync or retroactively on historical commits, persisted in Git commit messages and portable repository metadata.
- **REQ-12 — Full System-Reset Re-instatement**: Reconstruct the full game configuration, provider folder layout, save state labels, and local save directories from an existing save repository after a system reset (`sgm_repo_metadata.json` via GUI or `--reinstate`).
- **REQ-13 — External Repo & Online Save Import**:
  - Allow another user (or second machine) to inspect an accessible shared repository and import specific game save states or historical commits into their own setup.
  - Allow importing standalone save files, save folders, or archives downloaded from online sources (`.zip`, `.tar.gz`, `.tgz`, `.tar`) with an optional pre-import backup commit for safe rollback.

### Phase 3 — Safe Opt-In Live Sync & Simplified Settings Theme Toggle
- **REQ-14 — Explicit Opt-In Live Sync**: Prevent automatic "on-startup sync" from running repeatedly before the user has configured GitHub / created the repository or explicitly opted in; require enabling **"Enable Sync"** in Settings before live/automatic runs execute.
- **REQ-15 — Settings Light / Dark Toggle**: Move/replace the top-bar theme switcher with a clean **Light / Dark** toggle inside Settings.

### Phase 4 — Branding, Logs, Tailored Auth UI, Startup Autostart, Sync Fix & Save Editor
- **REQ-16 — Dedicated Logs Tab**: Provide a dedicated **Logs** tab showing live streaming activity/sync/Git logs and persisted log history with level filters (`INFO`, `OK`, `WARN`, `ERROR`), search, copy, and clear actions.
- **REQ-17 — Create Repository Hyperlink**: Provide a clickable hyperlink in **GitHub & Settings** pointing to `https://github.com/new` so users can open GitHub repository creation directly.
- **REQ-18 — Mode-Specific GitHub Authentication UI**: Render distinct authentication controls depending on the selected method (`Use Existing SSH Key`, `Create New SSH Key`, `Personal Access Token (PAT)`), hiding irrelevant SSH key inputs and public key previews when PAT is active.
- **REQ-19 — Proper App Name, Logo & System Startup Launch**: Give the application a dedicated name (**SaveVault**), custom shield-vault logo (`assets/savevault.svg` + 64x64 RGBA window icon + vector top-bar badge), and a **"Run SaveVault automatically on system startup"** setting (Linux `.desktop`, macOS LaunchAgent `.plist`, Windows Startup `.cmd`).
- **REQ-20 — Root-Cause Fix for Hanging Initial "On-Startup Sync"**: Ensure library discovery never falls back to raw game installation directories or metadata folders, resolves cross-library Steam `userdata/<uid>/<appid>/remote` and Proton prefix save folders accurately, and skips transient cache/demo folders (`Cache`, `ShaderCache`, `Demos`, etc.) so sync commits only actual save files and pushes promptly.
- **REQ-21 — Save File Viewer & Editor**: Provide an integrated **Save Editor** tab (and quick **View / Edit** actions from **Games & Sync**) to inspect any save file, edit supported formats (`.json` with format/validation, `.ini`/`.cfg`/`.xml`/`.toml`/`.yaml`/`.vdf`, plain text) with optional `.sgm.bak` backup creation, and inspect binary saves safely in read-only **Hex + ASCII** and **Extracted Strings** views.

### Phase 5 — Safe Game Removal Prompt, Documentation & AppImage Packaging
- **REQ-22 — Prompt to Remove Game Folder from Git**: When removing a game from the tracked list in the app, prompt the user whether they also want to delete the game's tracked folder from the Git repository (with commit/push) or keep its folder in Git and remove it only from the app list, leaving local saves on disk untouched.
- **REQ-23 — Requirements Export, Architecture Documentation & AppImage Build**: Export the project's requirements (`REQUIREMENTS.md`), provide a comprehensive features and architecture `README.md`, and provide a build pipeline (`scripts/build_appimage.sh`) that packages the release binary (`target/release/save_game_manager`) into an executable AppImage.

### Phase 6 — v0.2: Detection, Two-Way Sync, Profiles, Mod Awareness & Stack Hardening
- **REQ-24 — Automatic Save Detection**: Find save locations without user input using the PCGamingWiki-derived save location database, Steam Cloud folders, a ranked scan of the game's prefix / install folder, and by learning from files that change during a play session; report a confidence and reason, and only auto-protect confident results.
- **REQ-25 — More Launchers**: Discover games from Steam non-Steam shortcuts, Heroic (GOG, Epic, Amazon, sideload) including their Wine prefixes, Lutris and Bottles.
- **REQ-26 — Two-Way Sync with Conflict Safety**: Bring newer saves from the repository to this PC, never overwrite when both sides changed (ask, or follow a configured policy), combine diverged histories per game, restore saves after a reinstall, leave unplugged drives alone, and let a new PC adopt the cloud catalog.
- **REQ-27 — Safety Backups**: Zip the live files before any restore, profile switch, import or incoming cloud update, with retention and one-click undo.
- **REQ-28 — Save Profiles**: Several independent sets of saves per game (characters / playthroughs) with their own timelines, created empty, as a copy, or from an old snapshot; switchable and synced between PCs.
- **REQ-29 — Modded Setups**: Multiple tracked folders per game, include / exclude patterns, shared-across-profiles folders, and a mod fingerprint recorded with every snapshot and compared before restore.
- **REQ-30 — Automatic Snapshots Around Play Sessions**: Recognise running games, skip them while running, and snapshot when they close.
- **REQ-31 — Hardened Storage & Credentials**: Atomic user-only config writes, schema migration with backup, credentials outside the config file and passed to git through the environment scoped to `github.com`, git run without hooks / signing / line-ending conversion, archive extraction that cannot escape its destination.
- **REQ-32 — Local-First & Any Remote**: Everything works without a cloud; GitHub or any git remote / folder can be used for the cloud copy.
- **REQ-33 — Respect the User's List**: Locations chosen by hand are never overridden, better locations are suggestions, removed games are not re-added, and upgraded installs are offered new games instead of having them added silently.

---

## Implementation & Test Traceability Matrix

| Requirement ID | Primary Implementation Modules | Automated Verification (`tests/integration_tests.rs`) |
|---|---|---|
| `REQ-01`, `REQ-04`, `REQ-09` | `src/sync_engine.rs`, `src/git_ops.rs`, `src/app/` | `test_repo_subpath_per_provider_and_flat`, `test_git_commit_history_and_rollback_workflow`, `test_entire_folder_vs_selected_files_sync_and_rollback` |
| `REQ-02`, `REQ-03`, `REQ-17`, `REQ-18` | `src/auth.rs`, `src/app/` | `test_ssh_key_generation_and_auth_helpers` |
| `REQ-05` | `src/sync_engine.rs`, `src/providers.rs` | `test_cross_provider_save_sync`, `test_cross_provider_rollback_and_legendary_discovery` |
| `REQ-06`, `REQ-14` | `src/models.rs`, `src/config.rs`, `src/sync_engine.rs` | `test_sync_rate_defaults_and_scheduling`, `test_config_roundtrip` |
| `REQ-07` | `src/chunker.rs`, `src/sync_engine.rs` | `test_large_file_warning_split_and_reassembly` |
| `REQ-08`, `REQ-20` | `src/providers.rs`, `src/config.rs`, `src/sync_engine.rs` | `test_platform_provider_defaults_and_library_discovery`, `test_steam_cross_library_userdata_and_proton_save_discovery` |
| `REQ-10`, `REQ-15` | `src/theme.rs`, `src/app/` | `test_jizihub_theme_palettes` |
| `REQ-11`, `REQ-12`, `REQ-13` | `src/portability.rs`, `src/sync_engine.rs`, `src/app/` | `test_save_state_labels_on_sync_and_commits`, `test_reinstate_full_structure_after_system_reset`, `test_import_from_shared_external_repository`, `test_import_online_save_file_and_folder_with_pre_backup` |
| `REQ-16` | `src/models.rs`, `src/app/`, `src/sync_engine.rs` | Verified via `sync_multiple_games_with_progress` & `cargo clippy --all-targets` |
| `REQ-19` | `assets/savevault.svg`, `src/logo.rs`, `src/autostart.rs`, `src/main.rs` | `test_logo_icon_generation`, `test_system_autostart_configuration` |
| `REQ-21` | `src/save_editor.rs`, `src/app/` | `test_save_file_viewer_and_editor_formats` |
| `REQ-22` | `src/sync_engine.rs`, `src/app/` | `test_remove_game_keep_or_delete_git_folder` |
| `REQ-23` | `REQUIREMENTS.md`, `README.md`, `scripts/build_appimage.sh` | Release binary build + AppImage execution & `cargo test` |
| `REQ-24`, `REQ-25`, `REQ-33` | `src/detect/*`, `src/providers.rs` | `detection_finds_exact_save_locations_from_database_cloud_and_smart_scan`, `cloud_placeholder_is_located_by_scan_and_restored_into_the_right_prefix`, unit tests in `src/detect/*` |
| `REQ-26`, `REQ-32` | `src/sync_engine.rs`, `src/git_ops.rs` | `saves_travel_between_two_machines_with_safety_backup`, `conflicting_progress_is_never_overwritten_silently`, `conflict_policy_can_prefer_a_side_automatically`, `offline_snapshots_on_both_machines_are_combined_per_game`, `new_machine_learns_games_from_cloud_catalog_and_restores_on_install`, `reinstalled_game_gets_its_saves_back_and_missing_drive_is_left_alone`, `removing_a_game_from_the_cloud_pauses_it_elsewhere_without_touching_saves`, `unchanged_saves_do_not_create_snapshots` |
| `REQ-27` | `src/safety.rs` | covered in the two-machine, conflict and profile tests; `zip_extraction_never_escapes_destination`, `tar_gz_extraction_roundtrip` |
| `REQ-28` | `src/profiles.rs` | `profiles_keep_separate_characters_for_single_slot_games`, `profiles_sync_between_machines_without_cross_talk` |
| `REQ-29` | `src/detect/mods.rs`, `src/pattern.rs`, `src/sync_engine.rs` | `extra_roots_and_mod_fingerprints_are_part_of_every_snapshot`, unit tests in `src/detect/mods.rs`, `src/pattern.rs` |
| `REQ-30` | `src/watcher.rs`, `src/app/mod.rs` | unit tests in `src/watcher.rs` |
| `REQ-31` | `src/config.rs`, `src/git_ops.rs`, `src/safety.rs` | `legacy_config_is_migrated_with_backup_and_secrets_leave_the_config_file`, `git_credentials_travel_in_the_environment_and_only_to_github` |
