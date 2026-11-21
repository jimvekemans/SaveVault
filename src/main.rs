use anyhow::{Context, Result, bail};
use save_game_manager::config::{default_config_path, load_config, save_config};
use save_game_manager::detect::db::{SaveDb, UpdateOutcome, default_db_dir, import_manifest_file, update_database};
use save_game_manager::detect::{AUTO_ENABLE_CONFIDENCE, merge_scan_results, scan_installed_games};
use save_game_manager::models::{AppConfig, Platform};
use save_game_manager::portability::reinstate_full_structure;
use save_game_manager::profiles::{ProfileStart, create_profile, profile_summaries, switch_profile};
use save_game_manager::providers::find_multi_provider_groups;
use save_game_manager::sync_engine::{
    SyncOptions, SyncOutcome, game_health, games_due_on_startup, list_snapshots, live_state,
    pull_cloud_catalog, repo_subpath_for_game, rollback_game_to_commit, sync_games,
};
use save_game_manager::watcher::{ProcessLister, probes_for, running_games};
use std::path::{Path, PathBuf};

fn print_help() {
    println!(
        "\
SaveVault {version} - game save backup, sync, profiles and rollback

USAGE:
    save_game_manager [OPTIONS]

Without options the desktop app opens.

GENERAL:
    -h, --help                        Show this help
    -V, --version                     Show the version
        --minimized                   Open the desktop app minimized (used for autostart)
        --status                      Show settings and every tracked game with its state

DETECTION:
        --scan                        Find installed games and their save locations, and track them
        --detect                      Same as --scan but only prints what it would do
        --protect <GAME_OR_PROVIDER>  Track matching scanned game(s) (e.g. 'retroarch' or 'elden')
        --launch <GAME>               Launch a RetroArch game directly with its KBM profile
        --update-db                   Download / refresh the save location database now
        --import-db <manifest.yaml>   Build the save location database from a local manifest file

BACKUP & SYNC:
        --sync-all                    Snapshot (and cloud-sync) every tracked game, then exit
        --sync-startup                Run only the games scheduled for 'On Startup'
        --sync <GAME>                 Snapshot one game (id or part of its name)
        --pull                        Fetch the cloud catalog and list games saved on other PCs
        --reinstate [REPO_OR_PATH]    Rebuild games and saves from a repository after a reset

HISTORY & PROFILES:
        --history <GAME>              List a game's snapshots
        --restore <GAME> <SNAPSHOT>   Restore a snapshot (a safety zip of current files is kept)
        --profiles <GAME>             List a game's save profiles
        --new-profile <GAME> <NAME>   Create an empty profile (a fresh character / playthrough)
        --switch-profile <GAME> <PROFILE>
                                      Swap the live saves to another profile",
        version = env!("CARGO_PKG_VERSION")
    );
}

fn load() -> Result<(PathBuf, AppConfig)> {
    let path = default_config_path();
    let cfg = load_config(&path)?;
    Ok((path, cfg))
}

fn load_db() -> Option<SaveDb> {
    SaveDb::load_from_dir(&default_db_dir()).ok().flatten()
}

fn ensure_db(cfg: &AppConfig) -> Option<SaveDb> {
    if cfg.auto_update_detection_db
        && let Err(err) = update_database(&default_db_dir(), false)
    {
        eprintln!("Note: {err}");
    }
    let db = load_db();
    if db.is_none() {
        eprintln!("Note: save location database not available; using Steam Cloud folders and smart scan only.");
    }
    db
}

/// Resolves a user-supplied game reference (exact id, or a unique part of the name / id).
fn find_game_id(cfg: &AppConfig, needle: &str) -> Result<String> {
    if let Some(g) = cfg.games.iter().find(|g| g.id == needle) {
        return Ok(g.id.clone());
    }
    // Compare on slugs so symbols and capitalisation ("DARK SOULS™ III") do not matter.
    let wanted = save_game_manager::providers::slugify_game_name(needle);
    let hits: Vec<&save_game_manager::models::GameEntry> = cfg
        .games
        .iter()
        .filter(|g| {
            g.game_slug.contains(&wanted)
                || save_game_manager::providers::slugify_game_name(&g.name).contains(&wanted)
        })
        .collect();
    match hits.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => bail!("No tracked game matches '{needle}'. Run --status to list games."),
        many => bail!(
            "'{needle}' matches several games: {}",
            many.iter().map(|g| g.id.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn print_status() -> Result<()> {
    let (config_path, cfg) = load()?;
    println!("SaveVault {} on {}", env!("CARGO_PKG_VERSION"), Platform::current().display_name());
    println!("This PC: {}", cfg.machine_name);
    println!("Config: {}", config_path.display());
    println!("Local repository: {}", cfg.local_repo_path.display());
    let cloud = match (cfg.custom_remote(), cfg.auth.has_remote_credentials()) {
        (Some(url), _) => format!("custom remote {url}"),
        (None, true) => format!("GitHub {}/{}", cfg.auth.github_username(), cfg.repo_name),
        (None, false) => "not connected (snapshots stay on this PC)".to_string(),
    };
    println!("Cloud: {cloud} | automatic sync: {}", if cfg.sync_enabled { "on" } else { "off" });
    println!("Schedule: {}", cfg.global_sync_rate.display_label());
    println!(
        "Snapshot when a game closes: {} | safety backups: {} (keep {})",
        cfg.backup_on_game_exit, cfg.safety_backups, cfg.safety_backup_keep
    );
    match load_db() {
        Some(db) => println!(
            "Save location database: {} games{}",
            db.len(),
            db.meta
                .fetched_at
                .map(|t| format!(", fetched {}", t.format("%Y-%m-%d")))
                .unwrap_or_default()
        ),
        None => println!("Save location database: not downloaded yet (run --update-db)"),
    }

    println!("\nLaunchers:");
    for lib in &cfg.provider_libraries {
        let found = lib.library_paths.iter().filter(|p| p.exists()).count();
        println!(
            "  {:<12} {} ({found}/{} folders found)",
            lib.provider.display_name(),
            if lib.enabled { "on " } else { "off" },
            lib.library_paths.len()
        );
    }

    let probes = probes_for(&cfg.games);
    let running = running_games(&probes, &ProcessLister::new().cmdlines());

    println!("\nGames ({}):", cfg.games.len());
    for g in &cfg.games {
        let live = (!g.is_placeholder()).then(|| live_state(&cfg, g));
        let health = game_health(g, running.contains(&g.id), live.as_ref());
        let profile = if g.profiles.is_empty() {
            String::new()
        } else {
            format!(" | profile: {}", g.profile_name(&g.active_profile))
        };
        println!(
            "  [{:<10}] {:<44} {:<20} {} file(s){profile}",
            g.provider.display_name(),
            g.name,
            health.label(),
            live.as_ref().map(|l| l.files).unwrap_or(0),
        );
        println!(
            "      saves: {}",
            if g.is_placeholder() { "(not located on this PC)".to_string() } else { g.save_dir.display().to_string() }
        );
        if let Some(d) = &g.detection {
            println!("      found by: {} ({}%) - {}", d.source.display_name(), d.confidence, d.reason);
        }
        println!(
            "      repository folder: {}",
            repo_subpath_for_game(g, cfg.separate_folder_per_provider).display()
        );
    }
    let multi = find_multi_provider_groups(&cfg.games);
    if !multi.is_empty() {
        println!("\nOwned on several stores:");
        for (slug, providers) in multi {
            let names: Vec<&str> = providers.iter().map(|p| p.display_name()).collect();
            println!("  - {}: {}", slug, names.join(", "));
        }
    }
    Ok(())
}

fn run_scan(dry_run: bool) -> Result<()> {
    let (config_path, mut cfg) = load()?;
    let db = ensure_db(&cfg);
    let scanned = scan_installed_games(&cfg.provider_libraries, db.as_ref(), cfg.include_config_roots);

    println!("Installed games found: {}\n", scanned.len());
    for (g, det) in &scanned {
        let mark = if det.info.confidence >= AUTO_ENABLE_CONFIDENCE { "ok " } else { " ? " };
        println!(
            "[{mark}] [{:<10}] {:<44} {:>3}%  {}",
            g.provider.display_name(),
            g.name,
            det.info.confidence,
            det.info.source.display_name()
        );
        for (i, root) in det.saves.iter().enumerate() {
            let include = if root.include.is_empty() { String::new() } else { format!("  [{}]", root.include.join(", ")) };
            println!(
                "        {} {}{include}  ({} file(s), {:.1} MB)",
                if i == 0 { "saves:" } else { "  also:" },
                root.path.display(),
                root.stats.files,
                root.stats.bytes as f64 / 1_048_576.0
            );
        }
        for root in &det.configs {
            println!("        settings: {} ({} file(s))", root.path.display(), root.stats.files);
        }
        for m in &det.mod_sources {
            println!("        mods: {} ({})", m.path.display(), m.label);
        }
        if det.saves.is_empty() {
            println!("        {}", det.info.reason);
        }
    }

    let summary = merge_scan_results(&mut cfg, scanned);
    println!(
        "\n{} new game(s), {} located, {} with a better location available, {} need a look.",
        summary.added.len(),
        summary.located.len(),
        summary.suggestions.len(),
        summary.needs_review.len()
    );
    for (id, det) in &summary.suggestions {
        if let (Some(g), Some(root)) = (cfg.games.iter().find(|g| &g.id == id), det.saves.first()) {
            println!("  suggestion for {}:\n      now:    {}\n      better: {}", g.name, g.save_dir.display(), root.path.display());
        }
    }
    if dry_run {
        println!("\nDry run: nothing was changed.");
    } else {
        save_config(&config_path, &cfg)?;
        println!("\nSaved. {} game(s) tracked.", cfg.games.len());
    }
    Ok(())
}

fn print_outcome(outcome: &SyncOutcome, cfg: &AppConfig) {
    let name = |id: &String| {
        cfg.games
            .iter()
            .find(|g| &g.id == id)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| id.clone())
    };
    println!(
        "Saved: {} | restored from cloud: {} | unchanged: {} | need your choice: {} | skipped: {}",
        outcome.uploaded.len(),
        outcome.downloaded.len(),
        outcome.unchanged.len(),
        outcome.conflicts.len(),
        outcome.skipped.len()
    );
    for id in &outcome.uploaded {
        println!("  saved      {}", name(id));
    }
    for id in &outcome.downloaded {
        println!("  restored   {}", name(id));
    }
    for id in &outcome.conflicts {
        println!("  CONFLICT   {} (open SaveVault to choose which saves to keep)", name(id));
    }
    for (id, why) in &outcome.skipped {
        println!("  skipped    {} ({why})", name(id));
    }
    for id in &outcome.adopted_from_cloud {
        println!("  new from cloud: {}", name(id));
    }
    for w in &outcome.warnings {
        println!("WARNING: {}", w.message);
    }
    match (&outcome.committed_sha, outcome.pushed_to_remote, &outcome.push_error) {
        (_, _, Some(err)) => println!("Cloud: {err}"),
        (_, true, None) => println!("Cloud copy is up to date."),
        (Some(_), false, None) => println!("Snapshot kept on this PC."),
        (None, false, None) => {}
    }
}

fn run_sync(ids_for: impl Fn(&AppConfig) -> Result<Vec<String>>) -> Result<()> {
    let (config_path, mut cfg) = load()?;
    let ids = ids_for(&cfg)?;
    if ids.is_empty() {
        println!("No games due for sync.");
        return Ok(());
    }
    let running = running_games(&probes_for(&cfg.games), &ProcessLister::new().cmdlines());
    let progress = |_level, msg: &str| println!("{msg}");
    let use_remote = cfg.sync_enabled;
    let result = sync_games(
        &mut cfg,
        &ids,
        SyncOptions {
            use_remote,
            progress: Some(&progress),
            running,
            ..Default::default()
        },
    );
    // State changed by the pass is kept even when a later step failed.
    save_config(&config_path, &cfg)?;
    print_outcome(&result?, &cfg);
    Ok(())
}

fn run_reinstate(source: Option<&str>) -> Result<()> {
    let (config_path, mut cfg) = load()?;
    let fallback = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("save_game_manager")
        .join("restored_saves");
    let outcome = reinstate_full_structure(&mut cfg, source, &fallback)?;
    save_config(&config_path, &cfg)?;
    println!(
        "Re-instated {} game(s), restored {} file(s), and imported {} save state label(s).",
        outcome.games_reinstated, outcome.files_restored, outcome.labels_restored
    );
    Ok(())
}

fn arg<'a>(args: &'a [String], idx: usize, what: &str) -> Result<&'a str> {
    args.get(idx)
        .map(String::as_str)
        .with_context(|| format!("Missing {what}. See --help."))
}

fn run_cli(args: &[String]) -> Result<bool> {
    let Some(flag) = args.first().map(String::as_str) else {
        return Ok(false);
    };
    match flag {
        "-h" | "--help" => print_help(),
        "-V" | "--version" => println!("SaveVault {}", env!("CARGO_PKG_VERSION")),
        "--minimized" => return Ok(false),
        "--status" => print_status()?,
        "--scan" => run_scan(false)?,
        "--detect" => run_scan(true)?,
        "--protect" => {
            let needle = arg(args, 1, "game or provider")?.to_lowercase();
            let (config_path, mut cfg) = load()?;
            let db = load_db();
            let scanned = scan_installed_games(&cfg.provider_libraries, db.as_ref(), cfg.include_config_roots);
            let mut added = Vec::new();
            for (g, _) in scanned {
                let matches = g.id.to_lowercase().contains(&needle)
                    || g.name.to_lowercase().contains(&needle)
                    || g.provider.slug() == needle;
                if matches {
                    if !cfg.games.iter().any(|e| e.id == g.id) {
                        cfg.ignored_game_ids.retain(|id| id != &g.id);
                        added.push(g.id.clone());
                        cfg.games.push(g);
                    }
                }
            }
            save_config(&config_path, &cfg)?;
            println!("Protected {} matching game(s): {}", added.len(), added.join(", "));
        }
        "--launch" => {
            let needle = arg(args, 1, "game")?;
            let (config_path, cfg) = load()?;
            let id = find_game_id(&cfg, needle)?;
            let game = cfg.games.iter().find(|g| g.id == id).context("game vanished")?;
            let pid = save_game_manager::retroarch::launch_retroarch_game(&config_path, &cfg, game)?;
            println!("Launched {} in RetroArch (PID {pid}).", game.name);
        }
        "--update-db" => match update_database(&default_db_dir(), true)? {
            UpdateOutcome::Updated { games } => println!("Save location database updated: {games} games."),
            UpdateOutcome::NotModified | UpdateOutcome::Fresh => println!("Save location database is already current."),
        },
        "--import-db" => {
            let n = import_manifest_file(Path::new(arg(args, 1, "manifest file")?), &default_db_dir())?;
            println!("Save location database built from file: {n} games.");
        }
        "--sync-startup" => run_sync(|cfg| {
            if !cfg.sync_enabled {
                println!("Automatic sync is off. Turn it on in Settings first.");
                return Ok(Vec::new());
            }
            Ok(games_due_on_startup(cfg))
        })?,
        "--sync-all" => run_sync(|cfg| Ok(cfg.games.iter().filter(|g| g.enabled).map(|g| g.id.clone()).collect()))?,
        "--sync" => {
            let needle = arg(args, 1, "game")?.to_string();
            run_sync(move |cfg| Ok(vec![find_game_id(cfg, &needle)?]))?
        }
        "--pull" => {
            let (config_path, mut cfg) = load()?;
            let added = pull_cloud_catalog(&mut cfg)?;
            save_config(&config_path, &cfg)?;
            println!("{} game(s) found in the cloud that are not set up on this PC:", added.len());
            for id in added {
                println!("  {id}");
            }
            println!("Run --scan after installing them to restore their saves.");
        }
        "--reinstate" => run_reinstate(args.get(1).map(String::as_str))?,
        "--history" => {
            let (_, cfg) = load()?;
            let id = find_game_id(&cfg, arg(args, 1, "game")?)?;
            let game = cfg.games.iter().find(|g| g.id == id).context("game vanished")?;
            println!("{} - profile \"{}\"", game.name, game.profile_name(&game.active_profile));
            for c in list_snapshots(&cfg, &id, &game.active_profile, 100)? {
                println!(
                    "  {}  {}  {:<10} {}{}",
                    c.short_hash,
                    c.timestamp,
                    c.machine.as_deref().unwrap_or("-"),
                    c.message,
                    c.label.map(|l| format!("  [{l}]")).unwrap_or_default()
                );
            }
        }
        "--restore" => {
            let (config_path, mut cfg) = load()?;
            let id = find_game_id(&cfg, arg(args, 1, "game")?)?;
            let snapshot = arg(args, 2, "snapshot id")?.to_string();
            let use_remote = cfg.sync_enabled;
            let n = rollback_game_to_commit(&mut cfg, &id, &snapshot, use_remote);
            save_config(&config_path, &cfg)?;
            println!("Restored {} file(s). The previous files are in the safety backups folder.", n?);
        }
        "--profiles" => {
            let (_, cfg) = load()?;
            let id = find_game_id(&cfg, arg(args, 1, "game")?)?;
            for p in profile_summaries(&cfg, &id) {
                println!(
                    "  {} {:<24} id: {:<16} last snapshot: {}",
                    if p.active { "*" } else { " " },
                    p.name,
                    if p.id.is_empty() { "(main)" } else { &p.id },
                    p.last_snapshot.map(|c| c.timestamp).unwrap_or_else(|| "never".to_string())
                );
            }
        }
        "--new-profile" => {
            let (config_path, mut cfg) = load()?;
            let id = find_game_id(&cfg, arg(args, 1, "game")?)?;
            let name = arg(args, 2, "profile name")?.to_string();
            let pid = create_profile(&mut cfg, &id, &name, "", ProfileStart::Empty);
            save_config(&config_path, &cfg)?;
            println!("Created profile \"{name}\" (id: {}). Use --switch-profile to start using it.", pid?);
        }
        "--switch-profile" => {
            let (config_path, mut cfg) = load()?;
            let id = find_game_id(&cfg, arg(args, 1, "game")?)?;
            let wanted = arg(args, 2, "profile")?.to_string();
            let game = cfg.games.iter().find(|g| g.id == id).context("game vanished")?;
            let pid = game
                .profile_ids()
                .into_iter()
                .find(|p| {
                    p == &wanted
                        || game.profile_name(p).eq_ignore_ascii_case(&wanted)
                        || (p.is_empty() && wanted.eq_ignore_ascii_case("main"))
                })
                .with_context(|| format!("No profile '{wanted}'. See --profiles."))?;
            if running_games(&probes_for(&cfg.games), &ProcessLister::new().cmdlines()).contains(&id) {
                bail!("Close the game before switching profiles.");
            }
            let use_remote = cfg.sync_enabled;
            let n = switch_profile(&mut cfg, &id, &pid, use_remote);
            save_config(&config_path, &cfg)?;
            println!("Switched; {} file(s) put in place.", n?);
        }
        other => {
            eprintln!("Unknown option: {other}\n");
            print_help();
            std::process::exit(2);
        }
    }
    Ok(true)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        // `savevault --status | head` closes the pipe early; that is not an error worth a panic.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.to_string().contains("Broken pipe") {
                std::process::exit(0);
            }
            default_hook(info);
        }));
    }
    if run_cli(&args)? {
        return Ok(());
    }
    let minimized = args.first().map(String::as_str) == Some("--minimized");
    save_game_manager::app::run(minimized)
}
