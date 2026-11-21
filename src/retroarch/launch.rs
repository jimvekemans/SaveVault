//! RetroArch process detection, command-line parsing, game launching, and KBM bridge spawning.

use crate::models::{AppConfig, GameEntry};
use crate::retroarch::cfg::apply_kbm_profile_to_retroarch;
use crate::retroarch::registry::{kbm_profiles_path_for, write_kbm_profiles_registry_at};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Splits a user-configured command line into executable and arguments, honoring quotes and escapes.
pub fn split_command_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();

    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' => {
                if let Some(&next) = chars.peek()
                    && (next == '"' || next == '\\')
                {
                    cur.push(next);
                    chars.next();
                } else {
                    cur.push('\\');
                }
            }
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    in_word = true;
                    if let Some(next) = chars.next() {
                        cur.push(next);
                    }
                }
                c if c.is_whitespace() => {
                    if in_word {
                        words.push(std::mem::take(&mut cur));
                        in_word = false;
                    }
                }
                _ => {
                    cur.push(c);
                    in_word = true;
                }
            },
        }
    }
    if in_word || !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// Returns true if an executable path/name refers to RetroArch (native, Flatpak, or AppImage)
/// while excluding `retroarch-mkb-bridge`.
pub fn is_retroarch_executable_name(arg: &str) -> bool {
    let base = arg
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if base.is_empty() || base.contains("mkb-bridge") || base.contains("mkb_bridge") {
        return false;
    }
    base == "retroarch"
        || base == "retroarch.exe"
        || base.starts_with("retroarch-")
        || base.starts_with("retroarch.")
        || base == "org.libretro.retroarch"
}

/// Locates `retroarch-mkb-bridge` next to the current executable, in `~/.local/bin`, or on `$PATH`.
pub fn find_mkb_bridge_binary() -> Option<PathBuf> {
    let bin_name = format!("retroarch-mkb-bridge{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(&bin_name);
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let local_bin = home.join(".local/bin").join(&bin_name);
        if local_bin.is_file() {
            return Some(local_bin);
        }
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join(&bin_name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn is_mkb_bridge_daemon_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let pid_str = entry.file_name();
        if !pid_str.to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        if comm.trim().starts_with("retroarch-mkb") {
            return true;
        }
    }
    false
}

/// Starts `retroarch-mkb-bridge --watch --profiles <profiles_path>` directly when not already running.
pub fn ensure_mkb_bridge_running(profiles_path: &Path) {
    #[cfg(target_os = "linux")]
    {
        if is_mkb_bridge_daemon_running() {
            return;
        }
        if let Some(bin) = find_mkb_bridge_binary() {
            let _ = Command::new(bin)
                .arg("--watch")
                .arg("--profiles")
                .arg(profiles_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = profiles_path;
}

/// Launches `game` in RetroArch using `config.retroarch_binary` (`<cmd...> -L <core_path> <rom_path>`),
/// updating per-game `.cfg`/`.opt` overrides and starting `retroarch-mkb-bridge` when KBM is enabled.
pub fn launch_retroarch_game(
    config_path: &Path,
    config: &AppConfig,
    game: &GameEntry,
) -> Result<u32> {
    let meta = game
        .retroarch
        .as_ref()
        .context("This game is not configured for RetroArch launching.")?;
    if !meta.rom_path.is_file() {
        bail!("ROM file not found: {}", meta.rom_path.display());
    }
    if !meta.core_path.is_file() {
        bail!("Libretro core not found: {}", meta.core_path.display());
    }

    let _ = apply_kbm_profile_to_retroarch(game)?;
    if meta.kbm_enabled {
        let reg_path = kbm_profiles_path_for(config_path);
        write_kbm_profiles_registry_at(&reg_path, config)?;
        ensure_mkb_bridge_running(&reg_path);
    }

    let mut words = split_command_words(&config.retroarch_binary);
    if words.is_empty() {
        words.push("retroarch".to_string());
    }
    let child = Command::new(&words[0])
        .args(&words[1..])
        .arg("-L")
        .arg(&meta.core_path)
        .arg(&meta.rom_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to launch RetroArch command '{}'", words.join(" ")))?;
    Ok(child.id())
}
