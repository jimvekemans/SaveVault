//! Ranked guessing for games the save database does not know, plus "learn while playing":
//! finding the folder whose files changed during a play session.

use super::template::PathContext;
use crate::models::{GameEntry, Platform, Provider};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;

/// A possible save folder with a score and a plain-language explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    /// 0–100.
    pub score: u8,
    pub reason: String,
}

/// Folders that exist in every Windows profile / prefix and never hold game saves.
pub fn is_boilerplate_dir(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "microsoft"
            | "temp"
            | "tmp"
            | "packages"
            | "connecteddevicesplatform"
            | "comms"
            | "d3dscache"
            | "nvidia"
            | "nvidia corporation"
            | "amd"
            | "intel"
            | "dxvk"
            | "vkd3d"
            | "easyanticheat"
            | "battleye"
            | "steam"
            | "cache"
            | "caches"
            | "programs"
            | "unrealengine"
            | "unrealenginelauncher"
            | "unreal engine"
            | "epicgameslauncher"
            | "epic games"
            | "crashreportclient"
            | "crashdumps"
            | "crashpad"
            | "cef_cache"
            | "cef"
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
            | "my music"
            | "my pictures"
            | "my videos"
            | "mod.io"
            | "public"
            | "application data"
            | "local settings"
            | "wine"
            | "mono"
            | "gecko"
            | "openvr"
            | "uplay"
            | "ubisoft game launcher"
            | "origin"
            | "ea desktop"
            | "electronic arts"
            | "gog.com"
            | "rockstar games launcher"
            | "social club"
            | "squirreltemp"
            | "virtualstore"
    )
}

/// Wrapper folders worth looking inside rather than treating as the save folder itself.
fn is_container_dir(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "my games" | "saved games" | "savegames" | "games"
    )
}

fn is_transient_name(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    matches!(
        l.as_str(),
        "cache" | "caches" | "logs" | "log" | "shadercache" | "shader_cache" | "d3dscache"
            | "crashreports" | "crashes" | "crash" | "dumps" | "gpucache" | "code cache"
            | "webcache" | "telemetry" | "analytics" | "screenshots" | "temp" | "tmp"
    ) || l.ends_with(".log")
        || l.ends_with(".dmp")
        || l.ends_with(".tmp")
        || l.ends_with(".lock")
        || l.starts_with("crash")
        || l == "player.log"
        || l == "player-prev.log"
        || l == "output_log.txt"
}

fn is_save_like_ext(ext: &str) -> bool {
    matches!(
        ext,
        "sav" | "save" | "sl2" | "ess" | "fos" | "skse" | "dat" | "bin" | "profile" | "prf"
            | "slot" | "savegame" | "gsv" | "rpgsave" | "rvdata2" | "lsv" | "sfs" | "es3"
            | "json" | "xml" | "db" | "sqlite" | "wld" | "plr" | "world" | "character" | "bak"
    )
}

/// Lowercase alphanumeric words of a title (dropping filler words).
pub fn name_tokens(name: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut BTreeSet<String>| {
        if !cur.is_empty() {
            let w = std::mem::take(cur);
            if !matches!(w.as_str(), "the" | "of" | "a" | "an" | "and" | "edition" | "game") {
                out.insert(w);
            }
        }
    };
    let mut prev_lower = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            // Split camelCase: "EldenRing" -> elden, ring.
            if ch.is_ascii_uppercase() && prev_lower {
                flush(&mut cur, &mut out);
            }
            prev_lower = ch.is_ascii_lowercase();
            cur.push(ch.to_ascii_lowercase());
        } else {
            prev_lower = false;
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn compact(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// 0.0–1.0 similarity between a game's names and a folder path's trailing components.
pub fn name_similarity(game_names: &[&str], dir_components: &[String]) -> f32 {
    let dir_joined = dir_components.join(" ");
    let dir_tokens = name_tokens(&dir_joined);
    let dir_compact = compact(&dir_joined);
    let mut best = 0f32;
    for name in game_names {
        let tokens = name_tokens(name);
        if tokens.is_empty() {
            continue;
        }
        let hit = tokens.iter().filter(|t| dir_tokens.contains(*t)).count();
        let mut score = hit as f32 / tokens.len() as f32;
        let c = compact(name);
        if c.len() >= 4 && !dir_compact.is_empty() {
            for comp in dir_components {
                let cc = compact(comp);
                if cc.len() >= 4 && (cc == c || c.contains(&cc) || cc.contains(&c)) {
                    score = score.max(if cc == c { 1.0 } else { 0.8 });
                }
            }
        }
        best = best.max(score);
    }
    best
}

#[derive(Debug, Default)]
struct DirEvidence {
    files: usize,
    save_like: usize,
    transient: usize,
    bytes: u64,
    newest: Option<SystemTime>,
    has_save_dir: bool,
}

fn gather_evidence(dir: &Path) -> DirEvidence {
    let mut ev = DirEvidence::default();
    for entry in WalkDir::new(dir)
        .max_depth(5)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_transient_name(&e.file_name().to_string_lossy()))
        .filter_map(|e| e.ok())
        .take(4000)
    {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if entry.file_type().is_dir() {
            if entry.depth() > 0 && (name.contains("save") || name == "profiles" || name == "characters") {
                ev.has_save_dir = true;
            }
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        ev.files += 1;
        if is_transient_name(&name) {
            ev.transient += 1;
        }
        let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if is_save_like_ext(ext) || name.contains("save") {
            ev.save_like += 1;
        }
        if let Ok(meta) = entry.metadata() {
            ev.bytes += meta.len();
            if let Ok(m) = meta.modified() {
                ev.newest = Some(ev.newest.map_or(m, |cur| cur.max(m)));
            }
        }
    }
    ev
}

/// Narrows a wrapper folder to the part that actually holds saves.
pub fn refine_save_dir(candidate: PathBuf) -> PathBuf {
    for sub in [
        &["TAGame", "SaveData"][..],
        &["Saved", "SaveGames"][..],
        &["SaveGames"][..],
    ] {
        let p = sub.iter().fold(candidate.clone(), |acc, s| acc.join(s));
        if p.is_dir() {
            return p;
        }
    }
    // A vendor folder with no files and exactly one real subfolder ("Rockstar Games/RDR2").
    if let Ok(entries) = fs::read_dir(&candidate) {
        let mut has_files = false;
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if p.is_file() {
                has_files = true;
            } else if p.is_dir() && !is_boilerplate_dir(&name) {
                subdirs.push(p);
            }
        }
        if !has_files
            && subdirs.len() == 1
            && subdirs[0]
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !n.chars().all(|c| c.is_ascii_digit()))
        {
            return refine_save_dir(subdirs.remove(0));
        }
    }
    candidate
}

fn game_names(game: &GameEntry) -> Vec<String> {
    let mut names = vec![game.name.clone()];
    if let Some(dir) = game
        .install_dir
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
    {
        names.push(dir.to_string());
    }
    names
}

fn score_candidate(
    path: &Path,
    rel_components: &[String],
    names: &[&str],
    dedicated_prefix: bool,
) -> Option<Candidate> {
    let ev = gather_evidence(path);
    if ev.files == 0 {
        return None;
    }
    let sim = name_similarity(names, rel_components);
    let mut score = 0f32;
    let mut reasons = Vec::new();

    if sim >= 0.5 {
        score += 45.0 * sim;
        reasons.push("folder name matches the game".to_string());
    }
    if ev.save_like > 0 || ev.has_save_dir {
        score += 25.0;
        reasons.push(format!("{} save-like file(s)", ev.save_like.max(1)));
    }
    if dedicated_prefix {
        score += 20.0;
        reasons.push("inside this game's own Proton prefix".to_string());
    }
    if ev
        .newest
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < Duration::from_secs(30 * 24 * 3600))
    {
        score += 5.0;
    }
    if ev.transient * 2 > ev.files {
        score -= 30.0;
    }
    if ev.bytes > 2 * 1024 * 1024 * 1024 {
        score -= 40.0;
    }
    if score < 20.0 {
        return None;
    }
    Some(Candidate {
        path: path.to_path_buf(),
        score: score.clamp(0.0, 75.0) as u8,
        reason: reasons.join(", "),
    })
}

/// Ranks likely save folders for a game without database help. Best candidate first.
pub fn guess_save_dirs(game: &GameEntry, ctx: &PathContext) -> Vec<Candidate> {
    let names_owned = game_names(game);
    let names: Vec<&str> = names_owned.iter().map(String::as_str).collect();
    let mut out: Vec<Candidate> = Vec::new();

    // A Steam compatdata prefix belongs to exactly one game.
    let dedicated_prefix = game.provider == Provider::Steam
        && ctx
            .prefix_dir
            .as_deref()
            .is_some_and(|p| p.components().any(|c| c.as_os_str() == "compatdata"));

    for user in ctx.wine_user_dirs() {
        for container in [
            "Saved Games",
            "Documents/My Games",
            "Documents",
            "AppData/Roaming",
            "AppData/Local",
            "AppData/LocalLow",
        ] {
            let container_path = user.join(container);
            let Ok(entries) = fs::read_dir(&container_path) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if !path.is_dir()
                    || name.starts_with('.')
                    || is_boilerplate_dir(&name)
                    || is_container_dir(&name)
                {
                    continue;
                }
                let refined = refine_save_dir(path.clone());
                let rel: Vec<String> = refined
                    .strip_prefix(&container_path)
                    .map(|r| {
                        r.components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect()
                    })
                    .unwrap_or_else(|_| vec![name.clone()]);
                if let Some(c) = score_candidate(&refined, &rel, &names, dedicated_prefix) {
                    out.push(c);
                }
            }
        }
    }

    if let Some(install) = &ctx.install_dir {
        for sub in [
            "saves", "Saves", "save", "Save", "SaveGames", "savegames", "SaveData", "savedata",
            "Saved/SaveGames", "userdata", "UserData", "profiles", "Profiles",
        ] {
            let p = install.join(sub);
            if p.is_dir() {
                let ev = gather_evidence(&p);
                if ev.files > 0 && ev.bytes < 2 * 1024 * 1024 * 1024 {
                    out.push(Candidate {
                        path: p,
                        score: 55,
                        reason: "save folder inside the game's install folder".to_string(),
                    });
                }
            }
        }
    }

    // Native games: only trust name matches, these folders are shared by everything.
    if ctx.host != Platform::Windows && ctx.drive_c().is_none() {
        let mut native_dirs: Vec<(PathBuf, Vec<String>)> = Vec::new();
        for base in [&ctx.xdg_data, &ctx.xdg_config] {
            if let Ok(rd) = fs::read_dir(base) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if e.path().is_dir() && !name.starts_with('.') {
                        native_dirs.push((e.path(), vec![name]));
                    }
                }
            }
        }
        if let Ok(studios) = fs::read_dir(ctx.xdg_config.join("unity3d")) {
            for studio in studios.flatten() {
                if let Ok(products) = fs::read_dir(studio.path()) {
                    for product in products.flatten() {
                        if product.path().is_dir() {
                            native_dirs.push((
                                product.path(),
                                vec![product.file_name().to_string_lossy().into_owned()],
                            ));
                        }
                    }
                }
            }
        }
        for (path, rel) in native_dirs {
            if name_similarity(&names, &rel) >= 0.8
                && let Some(mut c) = score_candidate(&refine_save_dir(path), &rel, &names, false)
            {
                c.score = c.score.min(65);
                out.push(c);
            }
        }
    }

    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.path.cmp(&b.path)));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// Finds the folder whose files changed since `since` (the start of a play session).
///
/// Scans the game's prefix profile folders, its install folder, and — for native games —
/// the user's data / config folders, ignoring caches, logs, and shader data.
pub fn learn_from_session(game: &GameEntry, ctx: &PathContext, since: SystemTime) -> Option<Candidate> {
    // (scan root, depth of the "owning folder" below it)
    let mut scan_roots: Vec<(PathBuf, usize)> = Vec::new();
    for user in ctx.wine_user_dirs() {
        for (container, depth) in [
            ("Saved Games", 1),
            ("Documents/My Games", 1),
            ("Documents", 1),
            ("AppData/Roaming", 1),
            ("AppData/Local", 1),
            ("AppData/LocalLow", 2),
        ] {
            scan_roots.push((user.join(container), depth));
        }
    }
    if let Some(install) = &ctx.install_dir {
        scan_roots.push((install.clone(), 1));
    }
    if ctx.drive_c().is_none() && ctx.host != Platform::Windows {
        scan_roots.push((ctx.xdg_data.clone(), 1));
        scan_roots.push((ctx.xdg_config.clone(), 1));
        scan_roots.push((ctx.xdg_config.join("unity3d"), 2));
    }

    let names_owned = game_names(game);
    let names: Vec<&str> = names_owned.iter().map(String::as_str).collect();
    let native_shared = ctx.drive_c().is_none();

    // owning folder -> (changed files, changed save-like files)
    let mut hits: BTreeMap<PathBuf, (usize, usize)> = BTreeMap::new();
    for (root, owner_depth) in &scan_roots {
        if !root.is_dir() {
            continue;
        }
        for entry in WalkDir::new(root)
            .max_depth(7)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                e.depth() == 0
                    || !(is_transient_name(&name)
                        || (e.depth() == 1 && is_boilerplate_dir(&name))
                        || name.starts_with('.'))
            })
            .filter_map(|e| e.ok())
            .take(60_000)
        {
            if !entry.file_type().is_file() || entry.depth() <= *owner_depth {
                continue;
            }
            let Some(modified) = entry.metadata().ok().and_then(|m| m.modified().ok()) else {
                continue;
            };
            if modified < since {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if is_transient_name(&name) {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(root) else {
                continue;
            };
            let mut owner = root.clone();
            let comps: Vec<_> = rel.components().collect();
            // "My Games" style wrappers push the owning folder one level deeper.
            let mut depth = *owner_depth;
            if comps
                .first()
                .is_some_and(|c| is_container_dir(&c.as_os_str().to_string_lossy()))
            {
                depth += 1;
            }
            if comps.len() <= depth {
                continue;
            }
            for c in comps.iter().take(depth) {
                owner.push(c);
            }
            let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
            let slot = hits.entry(owner).or_default();
            slot.0 += 1;
            if is_save_like_ext(ext) || name.contains("save") {
                slot.1 += 1;
            }
        }
    }

    let mut ranked: Vec<(PathBuf, usize, usize, f32)> = hits
        .into_iter()
        .filter_map(|(owner, (changed, save_like))| {
            let rel = vec![owner.file_name()?.to_string_lossy().into_owned()];
            let sim = name_similarity(&names, &rel);
            // In shared native folders, require the name to match; anything else changing
            // during the session is some other program.
            if native_shared && ctx.install_dir.as_deref().is_none_or(|i| !owner.starts_with(i)) && sim < 0.5 {
                return None;
            }
            Some((owner, changed, save_like, sim))
        })
        .collect();
    ranked.sort_by(|a, b| {
        let key = |r: &(PathBuf, usize, usize, f32)| (r.2 > 0, (r.3 * 100.0) as i32, r.1);
        key(b).cmp(&key(a))
    });

    let (owner, changed, _save_like, _) = ranked.into_iter().next()?;
    let refined = refine_save_dir(owner);
    Some(Candidate {
        path: refined,
        score: 85,
        reason: format!("{changed} file(s) changed here while you were playing"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ctx(tmp: &Path) -> PathContext {
        let prefix = tmp.join("steamapps/compatdata/77/pfx");
        fs::create_dir_all(prefix.join("drive_c/users/steamuser")).unwrap();
        PathContext {
            host: Platform::Linux,
            home: tmp.join("home"),
            xdg_data: tmp.join("home/.local/share"),
            xdg_config: tmp.join("home/.config"),
            install_dir: Some(tmp.join("steamapps/common/Cool Quest")),
            library_root: Some(tmp.to_path_buf()),
            prefix_dir: Some(prefix),
            app_id: Some("77".into()),
        }
    }

    fn game(c: &PathContext) -> GameEntry {
        GameEntry {
            id: "steam:cool_quest".into(),
            name: "Cool Quest".into(),
            game_slug: "cool_quest".into(),
            provider: Provider::Steam,
            install_dir: c.install_dir.clone(),
            prefix_dir: c.prefix_dir.clone(),
            ..Default::default()
        }
    }

    #[test]
    fn tokens_split_camel_case_and_drop_filler() {
        let t = name_tokens("The EldenRing: Shadow of the Erdtree");
        assert!(t.contains("elden") && t.contains("ring") && t.contains("erdtree"));
        assert!(!t.contains("the") && !t.contains("of"));
        assert!(name_similarity(&["ELDEN RING"], &["EldenRing".to_string()]) > 0.9);
        assert!(name_similarity(&["Satisfactory"], &["FactoryGame".to_string()]) < 0.5);
    }

    #[test]
    fn guesses_prefer_named_save_folder_over_boilerplate_and_caches() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path());
        let user = c.wine_user_dirs().remove(0);
        fs::create_dir_all(user.join("AppData/Local/Microsoft/Windows")).unwrap();
        fs::write(user.join("AppData/Local/Microsoft/Windows/x.dat"), b"x").unwrap();
        fs::create_dir_all(user.join("AppData/Local/AAA Studio/Logs")).unwrap();
        fs::write(user.join("AppData/Local/AAA Studio/Logs/a.log"), b"x").unwrap();
        fs::create_dir_all(user.join("Documents/My Games/CoolQuest/Saves")).unwrap();
        fs::write(user.join("Documents/My Games/CoolQuest/Saves/slot1.sav"), b"x").unwrap();

        let ranked = guess_save_dirs(&game(&c), &c);
        assert!(!ranked.is_empty());
        // A wrapper folder holding nothing but one save folder is narrowed to that folder.
        assert_eq!(ranked[0].path, user.join("Documents/My Games/CoolQuest/Saves"));
        assert!(ranked[0].score >= 60, "score {}", ranked[0].score);
        assert!(ranked.iter().all(|r| !r.path.to_string_lossy().contains("Microsoft")));
    }

    #[test]
    fn session_learning_finds_folder_that_changed_while_playing() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path());
        let user = c.wine_user_dirs().remove(0);
        let old = user.join("AppData/Roaming/OldStuff");
        let saves = user.join("AppData/LocalLow/Tiny Studio/Mystery Title");
        let cache = user.join("AppData/Local/Mystery/ShaderCache");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&saves).unwrap();
        fs::create_dir_all(&cache).unwrap();
        fs::write(old.join("a.sav"), b"old").unwrap();
        let past = SystemTime::now() - Duration::from_secs(3600);
        fs::File::options().write(true).open(old.join("a.sav")).unwrap().set_modified(past).unwrap();

        let start = SystemTime::now() - Duration::from_secs(60);
        fs::write(saves.join("profile.es3"), b"new").unwrap();
        fs::write(cache.join("blob.bin"), b"new").unwrap();

        let found = learn_from_session(&game(&c), &c, start).unwrap();
        assert_eq!(found.path, saves);
        assert_eq!(found.score, 85);
        assert!(found.reason.contains("changed"));
    }
}
