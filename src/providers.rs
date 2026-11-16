use crate::models::{GameEntry, Platform, Provider, ProviderLibraryConfig, SyncSelectionMode};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Normalizes a game title into a lowercase snake_case identifier for folder layout and cross-provider matching.
pub fn slugify_game_name(name: &str) -> String {
    let mut cleaned = name.trim().to_lowercase();
    for suffix in [
        " - enhanced edition",
        " (enhanced edition)",
        " enhanced edition",
        " - definitive edition",
        " (definitive edition)",
        " definitive edition",
        " - game of the year edition",
        " - goty edition",
        " - goty",
    ] {
        if let Some(stripped) = cleaned.strip_suffix(suffix) {
            cleaned = stripped.trim().to_string();
        }
    }

    let mut slug = String::with_capacity(cleaned.len());
    let mut prev_underscore = false;
    for ch in cleaned.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            prev_underscore = false;
        } else if !prev_underscore && !slug.is_empty() {
            slug.push('_');
            prev_underscore = true;
        }
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    if slug.is_empty() {
        "unnamed_game".to_string()
    } else {
        slug
    }
}

/// Returns platform-specific default game library paths for the requested provider.
pub fn default_library_paths(provider: Provider, platform: Platform, home: &Path) -> Vec<PathBuf> {
    match (provider, platform) {
        (Provider::Steam, Platform::Linux) => vec![
            home.join(".local/share/Steam"),
            home.join(".steam/steam"),
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        ],
        (Provider::Steam, Platform::Windows) => vec![
            PathBuf::from(r"C:\Program Files (x86)\Steam"),
            PathBuf::from(r"C:\Steam"),
        ],
        (Provider::Steam, Platform::MacOs) => {
            vec![home.join("Library/Application Support/Steam")]
        }
        (Provider::Gog, Platform::Linux) => vec![
            home.join("GOG Games"),
            home.join(".config/heroic/gog_store"),
            home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic/gog_store"),
        ],
        (Provider::Gog, Platform::Windows) => vec![
            PathBuf::from(r"C:\Program Files (x86)\GOG Galaxy\Games"),
            PathBuf::from(r"C:\GOG Games"),
        ],
        (Provider::Gog, Platform::MacOs) => {
            vec![PathBuf::from("/Applications/GOG.com"), home.join("Applications")]
        }
        (Provider::EpicGames, Platform::Linux) => vec![
            home.join("Games/Heroic"),
            home.join(".config/heroic/legendaryConfig/legendary"),
            home.join(".config/legendary"),
        ],
        (Provider::EpicGames, Platform::Windows) => vec![
            PathBuf::from(r"C:\Program Files\Epic Games"),
            PathBuf::from(r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests"),
        ],
        (Provider::EpicGames, Platform::MacOs) => vec![
            PathBuf::from("/Users/Shared/Epic Games"),
            home.join("Library/Application Support/Epic/EpicGamesLauncher/Data/Manifests"),
        ],
        (Provider::Amazon, Platform::Linux) => vec![
            home.join(".config/heroic/nile_store"),
            home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic/nile_store"),
        ],
        (Provider::Amazon, Platform::MacOs) => {
            vec![home.join("Library/Application Support/heroic/nile_store")]
        }
        (Provider::Amazon, Platform::Windows) => vec![PathBuf::from(r"C:\Amazon Games\Library")],
        (Provider::Lutris, Platform::Linux) => vec![
            home.join(".config/lutris/games"),
            home.join(".local/share/lutris/games"),
            home.join(".var/app/net.lutris.Lutris/config/lutris/games"),
            home.join(".var/app/net.lutris.Lutris/data/lutris/games"),
        ],
        (Provider::Bottles, Platform::Linux) => vec![
            home.join(".local/share/bottles/bottles"),
            home.join(".var/app/com.usebottles.bottles/data/bottles/bottles"),
        ],
        (Provider::Other, Platform::Linux) => vec![
            home.join(".config/heroic/sideload_apps"),
            home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic/sideload_apps"),
        ],
        (Provider::Other, Platform::MacOs) => {
            vec![home.join("Library/Application Support/heroic/sideload_apps")]
        }
        (Provider::Lutris | Provider::Bottles, _) | (Provider::Other, Platform::Windows) => Vec::new(),
    }
}

/// Builds default provider library configurations for the host platform.
pub fn default_provider_libraries(platform: Platform, home: &Path) -> Vec<ProviderLibraryConfig> {
    Provider::all()
        .iter()
        .map(|&provider| ProviderLibraryConfig {
            provider,
            library_paths: default_library_paths(provider, platform, home),
            enabled: true,
        })
        .collect()
}

/// Scans all enabled provider library paths and returns discovered game entries.
pub fn scan_provider_libraries(libraries: &[ProviderLibraryConfig]) -> Vec<GameEntry> {
    let mut discovered = Vec::new();
    let mut seen_ids = BTreeSet::new();

    for lib in libraries.iter().filter(|l| l.enabled) {
        let entries = match lib.provider {
            Provider::Steam => scan_steam_paths(&lib.library_paths),
            Provider::Gog => scan_gog_paths(&lib.library_paths),
            Provider::EpicGames => scan_epic_paths(&lib.library_paths),
            Provider::Amazon => scan_amazon_paths(&lib.library_paths),
            Provider::Lutris => scan_lutris_paths(&lib.library_paths),
            Provider::Bottles => scan_bottles_paths(&lib.library_paths),
            Provider::Other => scan_sideload_paths(&lib.library_paths),
        };
        for entry in entries {
            if seen_ids.insert(entry.id.clone()) {
                discovered.push(entry);
            }
        }
    }

    // Non-Steam shortcuts live in Steam's own files, so they follow the Steam library toggle
    // but are listed under "Other".
    if let Some(steam) = libraries
        .iter()
        .find(|l| l.enabled && l.provider == Provider::Steam)
    {
        for entry in scan_steam_shortcuts(&steam.library_paths) {
            if seen_ids.insert(entry.id.clone()) {
                discovered.push(entry);
            }
        }
    }

    discovered.sort_by(|a, b| a.game_slug.cmp(&b.game_slug).then(a.provider.cmp(&b.provider)));
    discovered
}

/// Merges newly discovered games into an existing list without overwriting user customizations.
/// Also flags multi-provider matches when a newly added slug exists on another provider.
pub fn merge_discovered_games(existing: &mut Vec<GameEntry>, discovered: Vec<GameEntry>) -> usize {
    let mut added = 0;
    for entry in discovered {
        if !existing.iter().any(|g| g.id == entry.id) {
            existing.push(entry);
            added += 1;
        }
    }
    added
}

/// Returns a sorted map of `game_slug -> providers` for games present on two or more providers.
pub fn find_multi_provider_groups(games: &[GameEntry]) -> BTreeMap<String, Vec<Provider>> {
    let mut grouped: BTreeMap<String, BTreeSet<Provider>> = BTreeMap::new();
    for game in games {
        grouped
            .entry(game.game_slug.clone())
            .or_default()
            .insert(game.provider);
    }

    grouped
        .into_iter()
        .filter_map(|(slug, providers)| {
            if providers.len() >= 2 {
                Some((slug, providers.into_iter().collect()))
            } else {
                None
            }
        })
        .collect()
}

fn make_game_entry(
    provider: Provider,
    name: String,
    app_id: Option<String>,
    install_dir: Option<PathBuf>,
    save_dir: PathBuf,
) -> GameEntry {
    let game_slug = slugify_game_name(&name);
    let id = format!("{}:{}", provider.slug(), game_slug);
    GameEntry {
        id,
        name,
        game_slug,
        provider,
        app_id,
        install_dir,
        save_dir,
        enabled: true,
        sync_selection: SyncSelectionMode::EntireFolder,
        ..Default::default()
    }
}

/// Returns the folder containing `drive_c` for a Wine / Proton prefix path, if it is one.
pub fn normalize_prefix_dir(prefix: &Path) -> Option<PathBuf> {
    [prefix.join("pfx"), prefix.to_path_buf()]
        .into_iter()
        .find(|p| p.join("drive_c").is_dir())
}

/// Reads Heroic's per-game settings to find the Wine prefix configured for `app_id`.
pub fn heroic_wine_prefix(app_id: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(cfg) = dirs::config_dir() {
        candidates.push(cfg.join("heroic/GamesConfig").join(format!("{app_id}.json")));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(
            home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic/GamesConfig")
                .join(format!("{app_id}.json")),
        );
    }
    for cfg_path in candidates {
        let Ok(raw) = fs::read_to_string(&cfg_path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(prefix) = json
            .get(app_id)
            .and_then(|v| v.get("winePrefix"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            && let Some(norm) = normalize_prefix_dir(Path::new(prefix))
        {
            return Some(norm);
        }
    }
    None
}

fn resolve_save_dir_candidates(candidates: Vec<PathBuf>, fallback: PathBuf) -> PathBuf {
    for candidate in candidates {
        if candidate.is_dir() {
            return candidate;
        }
    }
    fallback
}

fn scan_steam_paths(paths: &[PathBuf]) -> Vec<GameEntry> {
    let mut results = Vec::new();
    let mut all_roots = Vec::new();
    let mut seen_roots = BTreeSet::new();

    for path in paths {
        if !path.exists() {
            continue;
        }
        if seen_roots.insert(path.clone()) {
            all_roots.push(path.clone());
        }
        for vdf_rel in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
            let vdf_path = path.join(vdf_rel);
            if let Ok(content) = fs::read_to_string(&vdf_path) {
                for extra_root in parse_steam_libraryfolders_vdf(&content) {
                    if extra_root.exists() && seen_roots.insert(extra_root.clone()) {
                        all_roots.push(extra_root);
                    }
                }
            }
        }
    }

    for root in &all_roots {
        let steamapps = if root.file_name().and_then(|n| n.to_str()) == Some("steamapps") {
            root.clone()
        } else {
            root.join("steamapps")
        };

        let mut found_manifest = false;
        if let Ok(entries) = fs::read_dir(&steamapps) {
            for entry in entries.flatten() {
                let path = entry.path();
                let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if fname.starts_with("appmanifest_") && fname.ends_with(".acf")
                    && let Ok(content) = fs::read_to_string(&path)
                        && let Some((appid, name, installdir)) = parse_steam_acf(&content) {
                            found_manifest = true;
                            if is_steam_runtime_or_tool(&name) {
                                continue;
                            }
                            let install_path = steamapps.join("common").join(&installdir);
                            let save_dir = resolve_steam_save_dir(
                                &all_roots,
                                &steamapps,
                                &appid,
                                &name,
                                &install_path,
                            );
                            let mut entry = make_game_entry(
                                Provider::Steam,
                                name,
                                Some(appid.clone()),
                                Some(install_path),
                                save_dir,
                            );
                            entry.prefix_dir = all_roots
                                .iter()
                                .map(|r| steamapps_dir_of(r))
                                .chain(std::iter::once(steamapps.clone()))
                                .find_map(|sa| normalize_prefix_dir(&sa.join("compatdata").join(&appid)));
                            entry.library_root = steam_userdata_root(&all_roots)
                                .or_else(|| steamapps.parent().map(Path::to_path_buf));
                            results.push(entry);
                        }
            }
        }

        if !found_manifest && !steamapps.exists() {
            scan_direct_subdirectories(Provider::Steam, root, &mut results);
        }
    }

    results
}

fn steamapps_dir_of(root: &Path) -> PathBuf {
    if root.file_name().and_then(|n| n.to_str()) == Some("steamapps") {
        root.to_path_buf()
    } else {
        root.join("steamapps")
    }
}

/// The Steam root that holds `userdata` (games on secondary drives still keep it there).
fn steam_userdata_root(all_roots: &[PathBuf]) -> Option<PathBuf> {
    all_roots.iter().find(|r| r.join("userdata").is_dir()).cloned()
}

fn is_steam_runtime_or_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("steamworks common redistributables")
        || lower.starts_with("proton ")
        || lower.starts_with("proton-")
        || lower == "proton"
        || lower.starts_with("steam linux runtime")
        || lower.starts_with("steamvr")
        || lower.contains("dedicated server")
        || lower.ends_with(" sdk")
        || lower.starts_with("steam controller")
}

fn is_proton_boilerplate_dir(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "microsoft"
            | "temp"
            | "packages"
            | "connecteddevicesplatform"
            | "comms"
            | "d3dscache"
            | "nvidia"
            | "dxvk"
            | "easyanticheat"
            | "steam"
            | "cache"
            | "programs"
            | "unrealengine"
            | "crashreportclient"
            | "cef_cache"
            | "fontconfig"
            | "pip"
            | "desktop"
            | "music"
            | "pictures"
            | "videos"
            | "downloads"
            | "templates"
            | "favorites"
            | "links"
            | "contacts"
            | "searches"
            | "start menu"
            | "my games"
            | "my music"
            | "my pictures"
            | "my videos"
            | "redengine"
            | "mod.io"
            | "public"
            | "application data"
            | "local settings"
    )
}

pub(crate) fn refine_discovered_save_dir(candidate: PathBuf) -> PathBuf {
    // Prefer focused save subdirectories so transient caches/demos aren't synced.
    let rl_savedata = candidate.join("TAGame").join("SaveData");
    if rl_savedata.is_dir() {
        return rl_savedata;
    }
    let ue_saves = candidate.join("Saved").join("SaveGames");
    if ue_saves.is_dir() {
        return ue_saves;
    }
    // If wrapper vendor folder has no direct files and one non-numeric subfolder (e.g. Rockstar Games/Red Dead Redemption 2).
    if let Ok(entries) = fs::read_dir(&candidate) {
        let mut has_files = false;
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            let fname = entry.file_name();
            let name = fname.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if p.is_file() {
                has_files = true;
            } else if p.is_dir() && !is_proton_boilerplate_dir(&name) {
                subdirs.push(p);
            }
        }
        if !has_files
            && subdirs.len() == 1
            && let Some(only) = subdirs.into_iter().next()
            && only
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !n.chars().all(|c| c.is_ascii_digit()))
        {
            return refine_discovered_save_dir(only);
        }
    }
    candidate
}

fn scan_proton_prefix_save_dirs(compat_user: &Path) -> Option<PathBuf> {
    if !compat_user.is_dir() {
        return None;
    }
    let roots = [
        compat_user.join("Saved Games"),
        compat_user.join("Documents").join("My Games"),
        compat_user.join("Documents"),
        compat_user.join("AppData").join("Roaming"),
        compat_user.join("AppData").join("Local"),
        compat_user.join("AppData").join("LocalLow"),
    ];
    for container in roots {
        let Ok(entries) = fs::read_dir(&container) else {
            continue;
        };
        let mut candidates: Vec<PathBuf> = entries
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                if !path.is_dir() {
                    return None;
                }
                let name = path.file_name().and_then(|n| n.to_str())?;
                if name.starts_with('.') || is_proton_boilerplate_dir(name) {
                    return None;
                }
                if fs::read_dir(&path).ok()?.next().is_some() {
                    Some(refine_discovered_save_dir(path))
                } else {
                    None
                }
            })
            .collect();
        candidates.sort();
        if let Some(found) = candidates.into_iter().next() {
            return Some(found);
        }
    }
    None
}

fn resolve_steam_save_dir(
    all_roots: &[PathBuf],
    steamapps: &Path,
    appid: &str,
    game_name: &str,
    install_path: &Path,
) -> PathBuf {
    let compat_user = steamapps
        .join("compatdata")
        .join(appid)
        .join("pfx/drive_c/users/steamuser");

    let exact_candidates = [
        compat_user.join("Saved Games").join(game_name),
        compat_user.join("Documents/My Games").join(game_name),
        compat_user.join("Documents").join(game_name),
        compat_user.join("AppData/Local").join(game_name),
        compat_user.join("AppData/Roaming").join(game_name),
        compat_user.join("AppData/LocalLow").join(game_name),
    ];
    for cand in exact_candidates {
        if cand.is_dir() {
            return refine_discovered_save_dir(cand);
        }
    }

    // Check Steam Cloud userdata/<uid>/<appid>/remote across ALL library roots
    // (games installed on a secondary drive still store userdata under the main Steam root).
    let mut userdata_parents: Vec<PathBuf> = all_roots.to_vec();
    if let Some(parent) = steamapps.parent()
        && !userdata_parents.iter().any(|p| p == parent)
    {
        userdata_parents.push(parent.to_path_buf());
    }
    for root in userdata_parents {
        let userdata_dir = root.join("userdata");
        if let Ok(users) = fs::read_dir(&userdata_dir) {
            for user_entry in users.flatten() {
                let remote = user_entry.path().join(appid).join("remote");
                if remote.is_dir() {
                    return remote;
                }
            }
        }
    }

    // Scan dedicated Proton prefix user folders for studio/game-named save folders.
    if let Some(prefix_found) = scan_proton_prefix_save_dirs(&compat_user) {
        return prefix_found;
    }

    let install_candidates = vec![
        install_path.join("saves"),
        install_path.join("Saves"),
        install_path.join("SaveGames"),
    ];

    let fallback = if compat_user.exists() {
        compat_user.join("Saved Games").join(game_name)
    } else {
        install_path.join("saves")
    };

    resolve_save_dir_candidates(install_candidates, fallback)
}

/// Extracts key-value `"path"` entries from Steam `libraryfolders.vdf`.
pub fn parse_steam_libraryfolders_vdf(content: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for line in content.lines() {
        if let Some((k, v)) = parse_vdf_kv_line(line)
            && k.eq_ignore_ascii_case("path") && !v.is_empty() {
                paths.push(PathBuf::from(v));
            }
    }
    paths
}

/// Parses `"appid"`, `"name"`, and `"installdir"` from a Steam `appmanifest_*.acf` file.
pub fn parse_steam_acf(content: &str) -> Option<(String, String, String)> {
    let mut appid = None;
    let mut name = None;
    let mut installdir = None;

    for line in content.lines() {
        if let Some((k, v)) = parse_vdf_kv_line(line) {
            match k.to_ascii_lowercase().as_str() {
                "appid" => appid = Some(v),
                "name" => name = Some(v),
                "installdir" => installdir = Some(v),
                _ => {}
            }
        }
    }

    Some((appid?, name?, installdir?))
}

fn parse_vdf_kv_line(line: &str) -> Option<(String, String)> {
    let mut parts = Vec::new();
    let mut rest = line.trim();
    while let Some(start) = rest.find('"') {
        let after_start = &rest[start + 1..];
        let mut end_idx = None;
        let mut escaped = false;
        for (i, ch) in after_start.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                end_idx = Some(i);
                break;
            }
        }
        let end = end_idx?;
        let val = after_start[..end].replace("\\\\", "\\");
        parts.push(val);
        rest = &after_start[end + 1..];
        if parts.len() == 2 {
            break;
        }
    }
    if parts.len() == 2 {
        Some((parts[0].clone(), parts[1].clone()))
    } else {
        None
    }
}

fn scan_gog_paths(paths: &[PathBuf]) -> Vec<GameEntry> {
    let mut results = Vec::new();

    for root in paths {
        if !root.exists() {
            continue;
        }

        // Check Heroic GOG installed.json if present.
        let heroic_installed = root.join("installed.json");
        if heroic_installed.is_file()
            && let Ok(content) = fs::read_to_string(&heroic_installed) {
                results.extend(parse_heroic_gog_installed(&content));
            }

        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let subdir = entry.path();
                if !subdir.is_dir() {
                    continue;
                }
                let dir_name = subdir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                if dir_name.starts_with('.') {
                    continue;
                }

                if let Some((name, game_id)) = find_gog_info_in_dir(&subdir) {
                    let save_dir = resolve_generic_save_dir(&subdir);
                    results.push(make_game_entry(
                        Provider::Gog,
                        name,
                        game_id,
                        Some(subdir),
                        save_dir,
                    ));
                } else if has_any_game_like_content(&subdir) {
                    let save_dir = resolve_generic_save_dir(&subdir);
                    results.push(make_game_entry(
                        Provider::Gog,
                        dir_name,
                        None,
                        Some(subdir),
                        save_dir,
                    ));
                }
            }
        }
    }

    results
}

fn find_gog_info_in_dir(dir: &Path) -> Option<(String, Option<String>)> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if fname.starts_with("goggame-") && fname.ends_with(".info")
            && let Ok(content) = fs::read_to_string(&path)
                && let Ok(json) = serde_json::from_str::<Value>(&content) {
                    let name = json.get("name").and_then(|v| v.as_str())?.to_string();
                    let game_id = json
                        .get("gameId")
                        .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())));
                    return Some((name, game_id));
                }
    }
    None
}

fn parse_heroic_gog_installed(content: &str) -> Vec<GameEntry> {
    let mut results = Vec::new();
    let Ok(json) = serde_json::from_str::<Value>(content) else {
        return results;
    };
    let Some(arr) = json.get("installed").and_then(|v| v.as_array()) else {
        return results;
    };
    for item in arr {
        let Some(install_path_str) = item.get("install_path").and_then(|v| v.as_str()) else {
            continue;
        };
        let install_path = PathBuf::from(install_path_str);
        let app_id = item
            .get("appName")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let name = item
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                install_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            });
        if let Some(name) = name {
            let save_dir = app_id
                .as_deref()
                .and_then(|id| resolve_heroic_wine_save_dir(id, &name))
                .unwrap_or_else(|| resolve_generic_save_dir(&install_path));
            let mut entry = make_game_entry(
                Provider::Gog,
                name,
                app_id.clone(),
                Some(install_path),
                save_dir,
            );
            entry.prefix_dir = app_id.as_deref().and_then(heroic_wine_prefix);
            results.push(entry);
        }
    }
    results
}

fn scan_epic_paths(paths: &[PathBuf]) -> Vec<GameEntry> {
    let mut results = Vec::new();

    for root in paths {
        if !root.exists() {
            continue;
        }

        let mut found_manifest = false;

        // 1. Heroic / Legendary `installed.json`
        let installed_json = root.join("installed.json");
        if installed_json.is_file()
            && let Ok(content) = fs::read_to_string(&installed_json) {
                let parsed = parse_legendary_installed_json(&content);
                if !parsed.is_empty() {
                    found_manifest = true;
                    results.extend(parsed);
                }
            }

        // 2. Epic Games Launcher `.item` manifests
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("item")
                    && let Ok(content) = fs::read_to_string(&path)
                        && let Some(game) = parse_epic_item_manifest(&content) {
                            found_manifest = true;
                            results.push(game);
                        }
            }
        }

        // 3. Direct subdirectories inside an Epic install library (e.g., ~/Games/Heroic/<Game>)
        // Never treat Heroic/Legendary metadata/config directories themselves as game installs.
        let root_name = root.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_launcher_config_dir = matches!(
            root_name,
            "legendary" | "legendaryConfig" | "gog_store" | "metadata"
        ) || root
            .components()
            .any(|c| c.as_os_str() == "legendaryConfig" || c.as_os_str() == "gog_store");
        if !found_manifest && !is_launcher_config_dir {
            scan_direct_subdirectories(Provider::EpicGames, root, &mut results);
        }
    }

    results
}

/// Parses a Legendary / Heroic `installed.json` object mapping `app_name` -> metadata.
pub fn parse_legendary_installed_json(content: &str) -> Vec<GameEntry> {
    let mut results = Vec::new();
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(content) else {
        return results;
    };

    for (app_name, meta) in map {
        let Some(title) = meta.get("title").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(install_path_str) = meta.get("install_path").and_then(|v| v.as_str()) else {
            continue;
        };
        let install_path = PathBuf::from(install_path_str);
        let save_dir = resolve_generic_save_dir(&install_path);
        let mut entry = make_game_entry(
            Provider::EpicGames,
            title.to_string(),
            Some(app_name.clone()),
            Some(install_path),
            save_dir,
        );
        entry.prefix_dir = heroic_wine_prefix(&app_name);
        results.push(entry);
    }

    results
}

/// Parses an Epic Games Launcher `.item` manifest JSON file.
pub fn parse_epic_item_manifest(content: &str) -> Option<GameEntry> {
    let json: Value = serde_json::from_str(content).ok()?;
    let display_name = json.get("DisplayName").and_then(|v| v.as_str())?.to_string();
    let install_loc = PathBuf::from(json.get("InstallLocation").and_then(|v| v.as_str())?);
    let app_name = json
        .get("AppName")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let save_dir = resolve_generic_save_dir(&install_loc);
    Some(make_game_entry(
        Provider::EpicGames,
        display_name,
        app_name,
        Some(install_loc),
        save_dir,
    ))
}

fn scan_direct_subdirectories(provider: Provider, root: &Path, out: &mut Vec<GameEntry>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let subdir = entry.path();
        if !subdir.is_dir() {
            continue;
        }
        let dir_name = subdir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if dir_name.starts_with('.') || dir_name.eq_ignore_ascii_case("Prefixes") {
            continue;
        }
        if has_any_game_like_content(&subdir) {
            let save_dir = resolve_generic_save_dir(&subdir);
            out.push(make_game_entry(
                provider,
                dir_name,
                None,
                Some(subdir),
                save_dir,
            ));
        }
    }
}

fn has_any_game_like_content(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

fn resolve_heroic_wine_save_dir(app_id: &str, game_name: &str) -> Option<PathBuf> {
    let config_dir = dirs::config_dir()?;
    let candidates = [
        config_dir.join("heroic/GamesConfig").join(format!("{app_id}.json")),
        dirs::home_dir()?
            .join(".var/app/com.heroicgameslauncher.hgl/config/heroic/GamesConfig")
            .join(format!("{app_id}.json")),
    ];
    for cfg_path in candidates {
        let Ok(raw) = fs::read_to_string(&cfg_path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(prefix_str) = json
            .get(app_id)
            .and_then(|v| v.get("winePrefix"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
        else {
            continue;
        };
        let prefix = PathBuf::from(prefix_str);
        for drive_c in [prefix.join("pfx/drive_c"), prefix.join("drive_c")] {
            let user_dir = drive_c.join("users/steamuser");
            if !user_dir.is_dir() {
                continue;
            }
            for exact in [
                user_dir.join("Saved Games/CD Projekt Red").join(game_name),
                user_dir.join("Saved Games").join(game_name),
                user_dir.join("Documents/My Games").join(game_name),
                user_dir.join("Documents").join(game_name),
                user_dir.join("AppData/Local").join(game_name),
                user_dir.join("AppData/Roaming").join(game_name),
            ] {
                if exact.is_dir() {
                    return Some(exact);
                }
            }
            if let Some(found) = scan_proton_prefix_save_dirs(&user_dir) {
                return Some(found);
            }
        }
    }
    None
}

fn resolve_generic_save_dir(install_dir: &Path) -> PathBuf {
    let candidates = vec![
        install_dir.join("saves"),
        install_dir.join("Saves"),
        install_dir.join("Saved"),
        install_dir.join("Saved Games"),
        install_dir.join("SaveGames"),
    ];
    resolve_save_dir_candidates(candidates, install_dir.join("saves"))
}

// ---- Additional launchers -----------------------------------------------------------

/// Amazon Games via Heroic's `nile_store` (`installed.json` + `library.json`).
fn scan_amazon_paths(paths: &[PathBuf]) -> Vec<GameEntry> {
    let mut results = Vec::new();
    for root in paths {
        let Ok(raw) = fs::read_to_string(root.join("installed.json")) else {
            continue;
        };
        let Ok(installed) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let titles: std::collections::HashMap<String, String> = fs::read_to_string(root.join("library.json"))
            .ok()
            .and_then(|r| serde_json::from_str::<Value>(&r).ok())
            .and_then(|v| v.get("library").and_then(|l| l.as_array()).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|g| {
                Some((
                    g.get("app_name")?.as_str()?.to_string(),
                    g.get("title")?.as_str()?.to_string(),
                ))
            })
            .collect();
        let items = installed
            .as_array()
            .cloned()
            .or_else(|| installed.get("installed").and_then(|v| v.as_array()).cloned())
            .unwrap_or_default();
        for item in items {
            let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(path) = item.get("path").and_then(|v| v.as_str()) else {
                continue;
            };
            let install = PathBuf::from(path);
            let name = titles.get(id).cloned().or_else(|| {
                install.file_name().and_then(|n| n.to_str()).map(str::to_string)
            });
            let Some(name) = name else { continue };
            let save_dir = resolve_generic_save_dir(&install);
            let mut entry = make_game_entry(
                Provider::Amazon,
                name,
                Some(id.to_string()),
                Some(install),
                save_dir,
            );
            entry.prefix_dir = heroic_wine_prefix(id);
            results.push(entry);
        }
    }
    results
}

/// Heroic sideloaded apps (`sideload_apps/library.json`).
fn scan_sideload_paths(paths: &[PathBuf]) -> Vec<GameEntry> {
    let mut results = Vec::new();
    for root in paths {
        let Ok(raw) = fs::read_to_string(root.join("library.json")) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        for game in json.get("games").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            let Some(title) = game.get("title").and_then(|v| v.as_str()) else {
                continue;
            };
            let app_name = game.get("app_name").and_then(|v| v.as_str()).map(str::to_string);
            let exe = game
                .get("install")
                .and_then(|i| i.get("executable"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let install = game
                .get("folder_name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| exe.and_then(|e| Path::new(e).parent().map(Path::to_path_buf)));
            let save_dir = install
                .as_deref()
                .map(resolve_generic_save_dir)
                .unwrap_or_default();
            let mut entry = make_game_entry(
                Provider::Other,
                title.to_string(),
                app_name.clone(),
                install,
                save_dir,
            );
            entry.prefix_dir = app_name.as_deref().and_then(heroic_wine_prefix);
            results.push(entry);
        }
    }
    results
}

/// Lutris game configs (`games/<slug>-<timestamp>.yml`).
fn scan_lutris_paths(paths: &[PathBuf]) -> Vec<GameEntry> {
    let mut results = Vec::new();
    for root in paths {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yml") {
                continue;
            }
            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(doc) = serde_norway::from_str::<Value>(&raw) else {
                continue;
            };
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            // "<slug>-<unix timestamp>"
            let file_slug = match stem.rsplit_once('-') {
                Some((slug, ts)) if ts.chars().all(|c| c.is_ascii_digit()) => slug,
                _ => stem,
            };
            let slug = doc
                .get("game_slug")
                .or_else(|| doc.get("slug"))
                .and_then(|v| v.as_str())
                .unwrap_or(file_slug)
                .to_string();
            let name = doc
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    slug.split('-')
                        .filter(|w| !w.is_empty())
                        .map(|w| {
                            let mut c = w.chars();
                            c.next()
                                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                });
            let game = doc.get("game");
            let exe = game
                .and_then(|g| g.get("exe"))
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
            let prefix = game
                .and_then(|g| g.get("prefix"))
                .and_then(|v| v.as_str())
                .and_then(|p| normalize_prefix_dir(Path::new(p)));
            let working_dir = game
                .and_then(|g| g.get("working_dir"))
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
            let install = working_dir.or_else(|| {
                exe.as_deref()
                    .and_then(|e| e.parent())
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(Path::to_path_buf)
            });
            if install.is_none() && prefix.is_none() {
                continue;
            }
            let save_dir = install
                .as_deref()
                .map(resolve_generic_save_dir)
                .unwrap_or_default();
            let mut e = make_game_entry(Provider::Lutris, name, Some(slug), install, save_dir);
            e.prefix_dir = prefix;
            results.push(e);
        }
    }
    results
}

/// Bottles: every bottle is a prefix; its `External_Programs` are the games.
fn scan_bottles_paths(paths: &[PathBuf]) -> Vec<GameEntry> {
    let mut results = Vec::new();
    for root in paths {
        let Ok(bottles) = fs::read_dir(root) else {
            continue;
        };
        for bottle in bottles.flatten() {
            let bottle_dir = bottle.path();
            let Some(prefix) = normalize_prefix_dir(&bottle_dir) else {
                continue;
            };
            let doc = fs::read_to_string(bottle_dir.join("bottle.yml"))
                .ok()
                .and_then(|raw| serde_norway::from_str::<Value>(&raw).ok());
            let programs = doc
                .as_ref()
                .and_then(|d| d.get("External_Programs"))
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            for (_, prog) in programs {
                let Some(name) = prog.get("name").and_then(|v| v.as_str()) else {
                    continue;
                };
                let install = prog
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(|p| p.replace('\\', "/"))
                    .and_then(|p| Path::new(&p).parent().map(Path::to_path_buf))
                    .filter(|p| p.is_dir());
                let save_dir = install
                    .as_deref()
                    .map(resolve_generic_save_dir)
                    .unwrap_or_default();
                let mut e = make_game_entry(Provider::Bottles, name.to_string(), None, install, save_dir);
                e.prefix_dir = Some(prefix.clone());
                results.push(e);
            }
        }
    }
    results
}

/// One entry of Steam's binary `shortcuts.vdf`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SteamShortcut {
    pub app_id: u32,
    pub name: String,
    pub exe: String,
    pub start_dir: String,
}

/// Parses Steam's binary `shortcuts.vdf` (non-Steam games added to the library).
pub fn parse_steam_shortcuts_vdf(data: &[u8]) -> Vec<SteamShortcut> {
    fn read_cstr(data: &[u8], pos: &mut usize) -> Option<String> {
        let start = *pos;
        let end = data[start..].iter().position(|&b| b == 0)? + start;
        *pos = end + 1;
        Some(String::from_utf8_lossy(&data[start..end]).into_owned())
    }

    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut depth = 0usize;
    let mut current: Option<SteamShortcut> = None;

    while pos < data.len() {
        let ty = data[pos];
        pos += 1;
        match ty {
            0x00 => {
                if read_cstr(data, &mut pos).is_none() {
                    break;
                }
                depth += 1;
                if depth == 2 {
                    current = Some(SteamShortcut::default());
                }
            }
            0x01 => {
                let (Some(key), Some(val)) = (read_cstr(data, &mut pos), read_cstr(data, &mut pos)) else {
                    break;
                };
                if depth == 2 && let Some(cur) = current.as_mut() {
                    let unquoted = val.trim_matches('"').to_string();
                    match key.to_ascii_lowercase().as_str() {
                        "appname" => cur.name = val,
                        "exe" => cur.exe = unquoted,
                        "startdir" => cur.start_dir = unquoted,
                        _ => {}
                    }
                }
            }
            0x02 => {
                let Some(key) = read_cstr(data, &mut pos) else { break };
                if pos + 4 > data.len() {
                    break;
                }
                let val = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
                pos += 4;
                if depth == 2 && key.eq_ignore_ascii_case("appid") && let Some(cur) = current.as_mut() {
                    cur.app_id = val;
                }
            }
            0x08 => {
                if depth == 2 && let Some(cur) = current.take() && !cur.name.is_empty() {
                    out.push(cur);
                }
                depth = depth.saturating_sub(1);
            }
            _ => break,
        }
    }
    out
}

fn scan_steam_shortcuts(steam_paths: &[PathBuf]) -> Vec<GameEntry> {
    let mut results = Vec::new();
    let mut seen_roots = BTreeSet::new();
    for root in steam_paths {
        let Ok(canon) = root.canonicalize() else {
            continue;
        };
        if !seen_roots.insert(canon.clone()) {
            continue;
        }
        let Ok(users) = fs::read_dir(canon.join("userdata")) else {
            continue;
        };
        for user in users.flatten() {
            let Ok(data) = fs::read(user.path().join("config/shortcuts.vdf")) else {
                continue;
            };
            for sc in parse_steam_shortcuts_vdf(&data) {
                let prefix = normalize_prefix_dir(
                    &canon.join("steamapps/compatdata").join(sc.app_id.to_string()),
                );
                let install = Some(PathBuf::from(&sc.start_dir))
                    .filter(|p| p.is_dir())
                    .or_else(|| Path::new(&sc.exe).parent().map(Path::to_path_buf).filter(|p| p.is_dir()));
                if prefix.is_none() && install.is_none() {
                    continue;
                }
                let mut e = make_game_entry(
                    Provider::Other,
                    sc.name.clone(),
                    Some(sc.app_id.to_string()),
                    install,
                    PathBuf::new(),
                );
                e.prefix_dir = prefix;
                e.library_root = Some(canon.clone());
                results.push(e);
            }
        }
    }
    results
}
