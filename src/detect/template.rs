//! Portable path templates.
//!
//! The save database describes locations with placeholders such as `<winAppData>` or
//! `<base>`. This module resolves those against a concrete game install (including Wine /
//! Proton prefixes, where Windows paths are matched case-insensitively) and performs the
//! reverse: turning an absolute path into a template another machine can resolve.

use crate::models::{GameEntry, Platform};
use crate::pattern::{component_matches, has_glob_chars, normalize_pattern};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Everything needed to resolve a template for one game on this machine.
#[derive(Debug, Clone)]
pub struct PathContext {
    pub host: Platform,
    pub home: PathBuf,
    pub xdg_data: PathBuf,
    pub xdg_config: PathBuf,
    pub install_dir: Option<PathBuf>,
    pub library_root: Option<PathBuf>,
    /// Folder containing `drive_c`.
    pub prefix_dir: Option<PathBuf>,
    pub app_id: Option<String>,
}

impl PathContext {
    /// Context for the current machine without any game specifics.
    pub fn host_only() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            host: Platform::current(),
            xdg_data: dirs::data_dir().unwrap_or_else(|| home.join(".local/share")),
            xdg_config: dirs::config_dir().unwrap_or_else(|| home.join(".config")),
            home,
            install_dir: None,
            library_root: None,
            prefix_dir: None,
            app_id: None,
        }
    }

    pub fn for_game(game: &GameEntry) -> Self {
        let mut ctx = Self::host_only();
        ctx.install_dir = game.install_dir.clone();
        ctx.library_root = game.library_root.clone();
        ctx.prefix_dir = game.prefix_dir.clone().filter(|p| drive_c_of(p).is_some());
        ctx.app_id = game.app_id.clone();
        ctx
    }

    pub fn drive_c(&self) -> Option<PathBuf> {
        self.prefix_dir.as_deref().and_then(drive_c_of)
    }

    /// Windows user profile folders inside the prefix (`drive_c/users/<name>`), the Proton
    /// `steamuser` first.
    pub fn wine_user_dirs(&self) -> Vec<PathBuf> {
        let Some(drive_c) = self.drive_c() else {
            return Vec::new();
        };
        let users = drive_c.join("users");
        let mut out: Vec<PathBuf> = fs::read_dir(&users)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                !name.eq_ignore_ascii_case("Public")
                    && !name.eq_ignore_ascii_case("Default")
                    && !name.eq_ignore_ascii_case("All Users")
                    && !name.starts_with('.')
            })
            .collect();
        out.sort_by_key(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            (name != "steamuser", p.is_symlink(), name.to_string())
        });
        // Proton links `users/<login>` to `users/steamuser`; keep each real folder once.
        let mut seen = std::collections::BTreeSet::new();
        out.retain(|p| seen.insert(p.canonicalize().unwrap_or_else(|_| p.clone())));
        out
    }

    /// True when Windows-style placeholders can be resolved (real Windows or a prefix).
    pub fn has_windows_view(&self) -> bool {
        self.host == Platform::Windows || self.drive_c().is_some()
    }

    /// Base folders for a placeholder. The flag says whether lookups beneath the base
    /// should ignore case.
    fn base_dirs(&self, token: &str, windows_mode: bool) -> Vec<(PathBuf, bool)> {
        let wine = windows_mode && self.host != Platform::Windows;
        let ci = wine || self.host == Platform::Windows;
        let win_homes = || -> Vec<PathBuf> {
            if wine {
                self.wine_user_dirs()
            } else {
                vec![self.home.clone()]
            }
        };
        let per_home = |rel: &[&str]| -> Vec<(PathBuf, bool)> {
            win_homes()
                .into_iter()
                .map(|h| {
                    let (p, _) = descend(&h, rel, ci);
                    (p, ci)
                })
                .collect()
        };
        let on_drive_c = |rel: &[&str], native: &str| -> Vec<(PathBuf, bool)> {
            if wine {
                self.drive_c()
                    .map(|c| vec![(descend(&c, rel, true).0, true)])
                    .unwrap_or_default()
            } else if self.host == Platform::Windows {
                vec![(PathBuf::from(native), true)]
            } else {
                Vec::new()
            }
        };

        match token {
            "<base>" => self
                .install_dir
                .clone()
                .map(|p| vec![(p, ci)])
                .unwrap_or_default(),
            "<root>" => self
                .library_root
                .clone()
                .map(|p| vec![(p, false)])
                .unwrap_or_default(),
            "<home>" => {
                if windows_mode {
                    win_homes().into_iter().map(|h| (h, ci)).collect()
                } else {
                    vec![(self.home.clone(), false)]
                }
            }
            "<nativeHome>" => vec![(self.home.clone(), false)],
            "<xdgData>" => vec![(self.xdg_data.clone(), false)],
            "<xdgConfig>" => vec![(self.xdg_config.clone(), false)],
            "<winAppData>" if windows_mode => {
                if wine {
                    per_home(&["AppData", "Roaming"])
                } else {
                    dirs::data_dir().map(|p| vec![(p, true)]).unwrap_or_default()
                }
            }
            "<winLocalAppData>" if windows_mode => {
                if wine {
                    per_home(&["AppData", "Local"])
                } else {
                    dirs::data_local_dir().map(|p| vec![(p, true)]).unwrap_or_default()
                }
            }
            "<winLocalAppDataLow>" if windows_mode => per_home(&["AppData", "LocalLow"]),
            "<winDocuments>" if windows_mode => {
                if wine {
                    per_home(&["Documents"])
                } else {
                    dirs::document_dir().map(|p| vec![(p, true)]).unwrap_or_default()
                }
            }
            "<winPublic>" if windows_mode => on_drive_c(&["users", "Public"], r"C:\Users\Public"),
            "<winProgramData>" if windows_mode => on_drive_c(&["ProgramData"], r"C:\ProgramData"),
            "<winDir>" if windows_mode => on_drive_c(&["windows"], r"C:\Windows"),
            _ => Vec::new(),
        }
    }
}

/// Returns `<prefix>/drive_c` (or `<prefix>/pfx/drive_c` for raw Steam compatdata folders).
pub fn drive_c_of(prefix: &Path) -> Option<PathBuf> {
    [prefix.join("drive_c"), prefix.join("pfx").join("drive_c")]
        .into_iter()
        .find(|p| p.is_dir())
}

/// Walks `rel` beneath `base`, optionally matching each component case-insensitively.
/// Returns the resulting path and whether every component existed. Missing components are
/// appended verbatim so callers still get the location the game would create.
pub fn descend<S: AsRef<str>>(base: &Path, rel: &[S], case_insensitive: bool) -> (PathBuf, bool) {
    let mut cur = base.to_path_buf();
    let mut found = cur.exists();
    for comp in rel {
        let comp = comp.as_ref();
        if !found {
            cur.push(comp);
            continue;
        }
        let direct = cur.join(comp);
        if direct.exists() {
            cur = direct;
            continue;
        }
        if case_insensitive
            && let Some(hit) = fs::read_dir(&cur).ok().and_then(|rd| {
                rd.flatten().map(|e| e.path()).find(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.to_lowercase() == comp.to_lowercase())
                })
            })
        {
            cur = hit;
            continue;
        }
        cur = direct;
        found = false;
    }
    (cur, found)
}

/// A template resolved to a folder on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// Folder to track.
    pub root: PathBuf,
    /// Pattern relative to `root`; `None` means the whole folder.
    pub include: Option<String>,
    /// Portable template of `root`.
    pub template: String,
    /// Whether `root` exists right now.
    pub exists: bool,
}

fn looks_like_file_name(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && (1..=6).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && !ext.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Resolves a database / SaveVault template for `ctx`.
///
/// `windows_mode` selects the Windows interpretation of `<home>` and the `<win…>`
/// placeholders (inside the prefix on Linux / macOS).
pub fn resolve_template(template: &str, ctx: &PathContext, windows_mode: bool) -> Vec<ResolvedPath> {
    let cleaned = template.trim().replace('\\', "/");
    let mut parts = cleaned.split('/').filter(|p| !p.is_empty());
    let Some(token) = parts.next() else {
        return Vec::new();
    };
    if !(token.starts_with('<') && token.ends_with('>')) {
        // Absolute literal path (only produced by SaveVault itself for unmapped locations).
        let p = PathBuf::from(&cleaned);
        if p.is_absolute() {
            return vec![ResolvedPath {
                exists: p.is_dir(),
                root: p,
                include: None,
                template: cleaned,
            }];
        }
        return Vec::new();
    }

    let install_name = ctx
        .install_dir
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(str::to_string);

    // (resolved component, original template component)
    let comps: Vec<(String, String)> = parts
        .map(|raw| {
            let mut c = raw.to_string();
            c = c.replace("<storeUserId>", "*").replace("<osUserName>", "*");
            c = c.replace("<game>", install_name.as_deref().unwrap_or("*"));
            c = c.replace("<storeGameId>", ctx.app_id.as_deref().unwrap_or("*"));
            (normalize_pattern(&c), raw.to_string())
        })
        .filter(|(c, _)| !c.is_empty())
        .collect();

    let mut out = Vec::new();
    for (base, ci) in ctx.base_dirs(token, windows_mode) {
        let static_len = comps.iter().take_while(|(c, _)| !has_glob_chars(c)).count();
        let must_expand = static_len < comps.len() && (token == "<root>" || static_len == 0);

        if must_expand {
            expand_concrete(&base, ci, token, &comps, &mut out);
            continue;
        }

        let static_names: Vec<&str> = comps[..static_len].iter().map(|(c, _)| c.as_str()).collect();
        let (mut root, found) = descend(&base, &static_names, ci);
        let mut template_comps: Vec<&str> =
            comps[..static_len].iter().map(|(_, raw)| raw.as_str()).collect();
        let mut include = if static_len < comps.len() {
            Some(
                comps[static_len..]
                    .iter()
                    .map(|(c, _)| c.as_str())
                    .collect::<Vec<_>>()
                    .join("/"),
            )
        } else {
            None
        };

        if include.is_none() && static_len > 0 {
            let last = &comps[static_len - 1].0;
            let is_file = if found { root.is_file() } else { looks_like_file_name(last) };
            if is_file {
                include = root.file_name().and_then(|n| n.to_str()).map(str::to_string);
                root.pop();
                template_comps.pop();
            }
        }

        let mut tpl = token.to_string();
        for c in &template_comps {
            tpl.push('/');
            tpl.push_str(c);
        }
        out.push(ResolvedPath {
            exists: root.is_dir(),
            root,
            include,
            template: tpl,
        });
    }
    out
}

/// Expands wildcard components against the filesystem so the tracked folder is specific to
/// the game (used when the fixed part of a template is a shared container such as
/// `<root>/userdata`).
fn expand_concrete(
    base: &Path,
    ci: bool,
    token: &str,
    comps: &[(String, String)],
    out: &mut Vec<ResolvedPath>,
) {
    // (current path, index of next component)
    let mut frontier: Vec<PathBuf> = vec![base.to_path_buf()];
    let mut consumed = 0usize;
    for (idx, (comp, _)) in comps.iter().enumerate() {
        if comp.contains("**") {
            break;
        }
        // Once a wildcard has been expanded and the remainder is game-specific, stop: the
        // rest becomes an include pattern relative to the expanded folder.
        let wildcard_done = comps[..idx].iter().any(|(c, _)| has_glob_chars(c));
        let static_after = comps[..idx]
            .iter()
            .rev()
            .take_while(|(c, _)| !has_glob_chars(c))
            .count();
        if wildcard_done && static_after >= 1 && has_glob_chars(comp) {
            break;
        }
        let mut next = Vec::new();
        for cur in &frontier {
            if has_glob_chars(comp) {
                if let Ok(rd) = fs::read_dir(cur) {
                    for entry in rd.flatten() {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        if component_matches(comp, &name) {
                            next.push(entry.path());
                        }
                    }
                }
            } else {
                let (p, found) = descend(cur, &[comp.as_str()], ci);
                if found {
                    next.push(p);
                }
            }
        }
        if next.is_empty() {
            return;
        }
        next.sort();
        frontier = next;
        consumed = idx + 1;
    }

    let mut tpl = token.to_string();
    for (_, raw) in &comps[..consumed] {
        tpl.push('/');
        tpl.push_str(raw);
    }
    let rest: Vec<&str> = comps[consumed..].iter().map(|(c, _)| c.as_str()).collect();
    for path in frontier {
        if path.is_file() {
            let Some(parent) = path.parent() else { continue };
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let parent_tpl = tpl.rsplit_once('/').map(|(a, _)| a.to_string()).unwrap_or_else(|| tpl.clone());
            out.push(ResolvedPath {
                root: parent.to_path_buf(),
                include: Some(name),
                template: parent_tpl,
                exists: true,
            });
        } else {
            out.push(ResolvedPath {
                exists: path.is_dir(),
                root: path,
                include: if rest.is_empty() { None } else { Some(rest.join("/")) },
                template: tpl.clone(),
            });
        }
    }
}

fn rel_components(base: &Path, path: &Path) -> Option<Vec<String>> {
    let rel = path.strip_prefix(base).ok()?;
    Some(
        rel.components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect(),
    )
}

fn join_template(token: &str, comps: &[String]) -> String {
    let mut out = token.to_string();
    for c in comps {
        out.push('/');
        out.push_str(c);
    }
    out
}

/// Turns an absolute path into a portable template, when it lies in a known location.
pub fn tokenize_path(path: &Path, ctx: &PathContext) -> Option<String> {
    // Steam Cloud: <root>/userdata/<user>/...
    if let Some(root) = &ctx.library_root
        && let Some(comps) = rel_components(&root.join("userdata"), path)
        && comps.len() >= 2
    {
        let mut rest = vec!["userdata".to_string(), "<storeUserId>".to_string()];
        rest.extend(comps[1..].iter().cloned());
        return Some(join_template("<root>", &rest));
    }

    if let Some(drive_c) = ctx.drive_c() {
        if let Some(comps) = rel_components(&drive_c.join("users"), path)
            && !comps.is_empty()
        {
            let user = &comps[0];
            let rest = &comps[1..];
            if user.eq_ignore_ascii_case("Public") {
                return Some(join_template("<winPublic>", rest));
            }
            let lower: Vec<String> = rest.iter().map(|c| c.to_lowercase()).collect();
            return Some(match lower.as_slice() {
                [a, b, ..] if a == "appdata" && b == "roaming" => {
                    join_template("<winAppData>", &rest[2..])
                }
                [a, b, ..] if a == "appdata" && b == "local" => {
                    join_template("<winLocalAppData>", &rest[2..])
                }
                [a, ..] if a == "documents" || a == "my documents" => {
                    join_template("<winDocuments>", &rest[1..])
                }
                _ => join_template("<home>", rest),
            });
        }
        if let Some(comps) = rel_components(&drive_c.join("ProgramData"), path) {
            return Some(join_template("<winProgramData>", &comps));
        }
    }

    if let Some(install) = &ctx.install_dir
        && let Some(comps) = rel_components(install, path)
    {
        return Some(join_template("<base>", &comps));
    }

    if ctx.host == Platform::Windows {
        if let Some(c) = dirs::data_dir().and_then(|d| rel_components(&d, path)) {
            return Some(join_template("<winAppData>", &c));
        }
        if let Some(c) = dirs::data_local_dir().and_then(|d| rel_components(&d, path)) {
            return Some(join_template("<winLocalAppData>", &c));
        }
        if let Some(c) = dirs::document_dir().and_then(|d| rel_components(&d, path)) {
            return Some(join_template("<winDocuments>", &c));
        }
        if let Some(c) = rel_components(&ctx.home, path) {
            return Some(join_template("<home>", &c));
        }
        return None;
    }

    if let Some(c) = rel_components(&ctx.xdg_config, path) {
        return Some(join_template("<xdgConfig>", &c));
    }
    if let Some(c) = rel_components(&ctx.xdg_data, path) {
        return Some(join_template("<xdgData>", &c));
    }
    if let Some(c) = rel_components(&ctx.home, path) {
        return Some(join_template("<nativeHome>", &c));
    }
    None
}

/// Resolves a SaveVault-produced template to the single most plausible folder.
pub fn resolve_own_template(template: &str, ctx: &PathContext) -> Option<PathBuf> {
    let native_only = template.starts_with("<xdg") || template.starts_with("<nativeHome>");
    let windows_mode = !native_only && ctx.has_windows_view();
    let mut hits = resolve_template(template, ctx, windows_mode);
    if hits.is_empty() && windows_mode {
        hits = resolve_template(template, ctx, false);
    }
    // Existing folders first, then most recently modified.
    hits.sort_by_key(|r| {
        let mtime = fs::metadata(&r.root).and_then(|m| m.modified()).ok();
        (!r.exists, std::cmp::Reverse(mtime))
    });
    hits.into_iter().next().map(|r| r.root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ctx_with_prefix(tmp: &Path) -> PathContext {
        let prefix = tmp.join("compatdata/42/pfx");
        fs::create_dir_all(prefix.join("drive_c/users/steamuser/AppData/Roaming")).unwrap();
        fs::create_dir_all(prefix.join("drive_c/users/Public")).unwrap();
        PathContext {
            host: Platform::Linux,
            home: tmp.join("home"),
            xdg_data: tmp.join("home/.local/share"),
            xdg_config: tmp.join("home/.config"),
            install_dir: Some(tmp.join("steamapps/common/My Game")),
            library_root: Some(tmp.join("steamroot")),
            prefix_dir: Some(prefix),
            app_id: Some("42".into()),
        }
    }

    #[test]
    fn resolves_windows_placeholders_inside_prefix_case_insensitively() {
        let tmp = TempDir::new().unwrap();
        let ctx = ctx_with_prefix(tmp.path());
        let user = ctx.wine_user_dirs().remove(0);
        fs::create_dir_all(user.join("AppData/Roaming/eldenring/0110000100000001")).unwrap();

        let hits = resolve_template("<winAppData>/EldenRing/<storeUserId>", &ctx, true);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].root, user.join("AppData/Roaming/eldenring"));
        assert_eq!(hits[0].include.as_deref(), Some("*"));
        assert_eq!(hits[0].template, "<winAppData>/EldenRing");
        assert!(hits[0].exists);

        // Windows placeholders are meaningless without a Windows view.
        assert!(resolve_template("<winAppData>/EldenRing", &ctx, false).is_empty());
    }

    #[test]
    fn file_templates_track_parent_folder_with_include() {
        let tmp = TempDir::new().unwrap();
        let ctx = ctx_with_prefix(tmp.path());
        let user = ctx.wine_user_dirs().remove(0);
        fs::create_dir_all(user.join("AppData/LocalLow/Crema/Temtem")).unwrap();
        fs::write(user.join("AppData/LocalLow/Crema/Temtem/savegame.dat"), b"x").unwrap();

        let hits = resolve_template("<home>/AppData/LocalLow/Crema/Temtem/savegame.dat", &ctx, true);
        assert_eq!(hits[0].root, user.join("AppData/LocalLow/Crema/Temtem"));
        assert_eq!(hits[0].include.as_deref(), Some("savegame.dat"));
        assert_eq!(hits[0].template, "<home>/AppData/LocalLow/Crema/Temtem");

        // Missing file: decided by the extension.
        let hits = resolve_template("<winAppData>/Foo/config.xml", &ctx, true);
        assert_eq!(hits[0].include.as_deref(), Some("config.xml"));
        assert!(!hits[0].exists);
        assert!(hits[0].root.ends_with("AppData/Roaming/Foo"));
    }

    #[test]
    fn steam_userdata_wildcards_expand_to_concrete_user_folder() {
        let tmp = TempDir::new().unwrap();
        let ctx = ctx_with_prefix(tmp.path());
        let root = ctx.library_root.clone().unwrap();
        fs::create_dir_all(root.join("userdata/111/42/remote/win64_save")).unwrap();
        fs::create_dir_all(root.join("userdata/222/99/remote")).unwrap();

        let hits = resolve_template("<root>/userdata/<storeUserId>/42/remote/win64_save", &ctx, true);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].root, root.join("userdata/111/42/remote/win64_save"));
        assert_eq!(hits[0].include, None);
        assert_eq!(hits[0].template, "<root>/userdata/<storeUserId>/42/remote/win64_save");
    }

    #[test]
    fn glob_file_patterns_keep_static_folder_as_root() {
        let tmp = TempDir::new().unwrap();
        let ctx = ctx_with_prefix(tmp.path());
        let hits = resolve_template("<winDocuments>/The Witcher 3/gamesaves/*.sav", &ctx, true);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].root.ends_with("Documents/The Witcher 3/gamesaves"));
        assert_eq!(hits[0].include.as_deref(), Some("*.sav"));
        assert_eq!(hits[0].template, "<winDocuments>/The Witcher 3/gamesaves");
    }

    #[test]
    fn tokenize_and_resolve_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let ctx = ctx_with_prefix(tmp.path());
        let user = ctx.wine_user_dirs().remove(0);

        let docs = user.join("Documents/My Games/Skyrim/Saves");
        fs::create_dir_all(&docs).unwrap();
        let tpl = tokenize_path(&docs, &ctx).unwrap();
        assert_eq!(tpl, "<winDocuments>/My Games/Skyrim/Saves");
        assert_eq!(resolve_own_template(&tpl, &ctx).unwrap(), docs);

        let low = user.join("AppData/LocalLow/Studio/Game");
        assert_eq!(tokenize_path(&low, &ctx).unwrap(), "<home>/AppData/LocalLow/Studio/Game");

        let base = ctx.install_dir.clone().unwrap().join("saves");
        assert_eq!(tokenize_path(&base, &ctx).unwrap(), "<base>/saves");

        let cloud = ctx.library_root.clone().unwrap().join("userdata/111/42/remote");
        assert_eq!(
            tokenize_path(&cloud, &ctx).unwrap(),
            "<root>/userdata/<storeUserId>/42/remote"
        );

        let native = ctx.xdg_config.join("unity3d/Team/Game");
        assert_eq!(tokenize_path(&native, &ctx).unwrap(), "<xdgConfig>/unity3d/Team/Game");
        assert_eq!(
            resolve_own_template("<xdgConfig>/unity3d/Team/Game", &ctx).unwrap(),
            native
        );
    }
}
