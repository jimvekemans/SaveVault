//! Background watcher: notices when tracked games start and stop, and when their save
//! files change, so SaveVault can snapshot right after a play session without being asked.

use crate::models::{AppConfig, GameEntry, Provider};
use crate::sync_engine::{LiveState, live_state};
use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// What is needed to recognise one game's process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameProbe {
    pub id: String,
    /// Steam app id, matched against Steam's `AppId=<id>` launcher argument.
    pub steam_app_id: Option<String>,
    /// Install folder with forward slashes and no trailing slash.
    pub install_dir: Option<String>,
    /// Full path to a RetroArch ROM file when launched via `retroarch -L <core> <rom>`.
    pub retroarch_rom: Option<String>,
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_string()
}

/// Builds probes for every tracked game that can be recognised.
pub fn probes_for(games: &[GameEntry]) -> Vec<GameProbe> {
    games
        .iter()
        .filter_map(|g| {
            let retroarch_rom = (g.provider == Provider::RetroArch)
                .then(|| {
                    g.retroarch
                        .as_ref()
                        .map(|r| normalize(&r.rom_path.to_string_lossy()))
                })
                .flatten()
                .filter(|p| !p.is_empty());
            let install_dir = g
                .install_dir
                .as_deref()
                .map(|p| normalize(&p.to_string_lossy()))
                .filter(|p| p.len() >= 8);
            let steam_app_id = (g.provider == Provider::Steam)
                .then(|| g.app_id.clone())
                .flatten()
                .filter(|id| !id.is_empty());
            if install_dir.is_none() && steam_app_id.is_none() && retroarch_rom.is_none() {
                return None;
            }
            Some(GameProbe {
                id: g.id.clone(),
                steam_app_id,
                install_dir,
                retroarch_rom,
            })
        })
        .collect()
}

fn looks_like_executable(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    lower.ends_with(".exe")
        || lower.ends_with(".x86_64")
        || lower.ends_with(".x86")
        || lower.ends_with(".sh")
        || lower.ends_with(".appimage")
        || lower.ends_with(".bin")
}

/// Strips a Windows drive prefix ("Z:/home/..." -> "/home/...").
fn strip_drive(arg: &str) -> &str {
    let bytes = arg.as_bytes();
    if bytes.len() > 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        &arg[2..]
    } else {
        arg
    }
}

fn process_matches(probe: &GameProbe, args: &[String]) -> bool {
    if let Some(rom) = &probe.retroarch_rom {
        let exe_is_ra = args
            .first()
            .is_some_and(|a| crate::retroarch::is_retroarch_executable_name(a));
        if exe_is_ra && args.iter().skip(1).any(|a| normalize(a) == *rom) {
            return true;
        }
    }
    if let Some(app_id) = &probe.steam_app_id {
        let needle = format!("AppId={app_id}");
        if args.iter().any(|a| a == &needle) {
            return true;
        }
    }
    let Some(install) = &probe.install_dir else {
        return false;
    };
    let prefix = format!("{install}/");
    let tail = install.rsplit('/').next().unwrap_or("");
    let tail_needle = format!("/{tail}/");
    for (i, raw) in args.iter().enumerate() {
        let arg = normalize(raw);
        let arg = strip_drive(&arg);
        // The program itself lives in the install folder…
        if i == 0 && arg.starts_with(&prefix) {
            return true;
        }
        // …or an executable inside it is being launched through Wine / a wrapper.
        if looks_like_executable(arg)
            && (arg.starts_with(&prefix) || (tail.len() >= 4 && arg.contains(&tail_needle)))
        {
            return true;
        }
    }
    false
}

/// Returns the ids of games that have a matching process in `cmdlines`.
pub fn running_games(probes: &[GameProbe], cmdlines: &[Vec<String>]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for args in cmdlines {
        if args.is_empty() {
            continue;
        }
        for probe in probes {
            if !out.contains(&probe.id) && process_matches(probe, args) {
                out.insert(probe.id.clone());
            }
        }
    }
    out
}

/// Reads the command lines of all processes visible to the current user.
pub struct ProcessLister {
    system: Option<System>,
}

impl Default for ProcessLister {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessLister {
    pub fn new() -> Self {
        Self {
            system: System::new().ok(),
        }
    }

    pub fn cmdlines(&mut self) -> Vec<Vec<String>> {
        let Some(system) = self.system.as_mut() else {
            return Vec::new();
        };
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );
        let me = std::process::id();
        system
            .processes()
            .iter()
            .filter(|(pid, _)| pid.as_u32() != me)
            .map(|(_, p)| {
                p.cmd()
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<String>>()
            })
            .filter(|args| !args.is_empty())
            .collect()
    }
}

/// A play session boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Started { game_id: String },
    Ended { game_id: String, started_at: SystemTime },
}

/// Turns successive "which games are running" observations into start / end events.
/// A game must be absent for `grace` consecutive observations before its session ends,
/// because launchers and anti-cheat wrappers briefly restart processes.
#[derive(Debug, Default)]
pub struct SessionTracker {
    active: HashMap<String, (SystemTime, u8)>,
    grace: u8,
}

impl SessionTracker {
    pub fn new(grace: u8) -> Self {
        Self {
            active: HashMap::new(),
            grace,
        }
    }

    pub fn running_ids(&self) -> BTreeSet<String> {
        self.active.keys().cloned().collect()
    }

    pub fn update(&mut self, running: &BTreeSet<String>, now: SystemTime) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        for id in running {
            match self.active.get_mut(id) {
                Some(slot) => slot.1 = 0,
                None => {
                    self.active.insert(id.clone(), (now, 0));
                    events.push(SessionEvent::Started { game_id: id.clone() });
                }
            }
        }
        let mut ended = Vec::new();
        for (id, (started, misses)) in self.active.iter_mut() {
            if running.contains(id) {
                continue;
            }
            *misses += 1;
            if *misses > self.grace {
                ended.push((id.clone(), *started));
            }
        }
        for (id, started_at) in ended {
            self.active.remove(&id);
            events.push(SessionEvent::Ended { game_id: id, started_at });
        }
        events
    }
}

/// Message from the background watcher to the UI.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    Session(SessionEvent),
    /// Fresh live states for tracked games (used for "new progress" badges).
    LiveStates(HashMap<String, LiveState>),
}

/// Handle to the background watcher thread.
pub struct WatcherHandle {
    shared: Arc<Mutex<AppConfig>>,
    stop: Arc<Mutex<bool>>,
}

impl WatcherHandle {
    /// Gives the watcher the latest configuration (call after every change).
    pub fn update_config(&self, config: &AppConfig) {
        if let Ok(mut slot) = self.shared.lock() {
            *slot = config.clone();
        }
    }
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        if let Ok(mut stop) = self.stop.lock() {
            *stop = true;
        }
    }
}

/// Starts the watcher. `wake` is called after every event so the UI can repaint.
pub fn spawn_watcher(
    config: &AppConfig,
    tx: Sender<WatchEvent>,
    wake: Box<dyn Fn() + Send>,
) -> WatcherHandle {
    let shared = Arc::new(Mutex::new(config.clone()));
    let stop = Arc::new(Mutex::new(false));
    let handle = WatcherHandle {
        shared: shared.clone(),
        stop: stop.clone(),
    };

    thread::Builder::new()
        .name("savevault-watcher".to_string())
        .spawn(move || {
            const PROCESS_INTERVAL: Duration = Duration::from_secs(4);
            const FILES_INTERVAL: Duration = Duration::from_secs(20);
            let mut lister = ProcessLister::new();
            let mut tracker = SessionTracker::new(2);
            let mut last_files = Instant::now() - FILES_INTERVAL;

            loop {
                if stop.lock().map(|s| *s).unwrap_or(true) {
                    break;
                }
                let config = match shared.lock() {
                    Ok(c) => c.clone(),
                    Err(_) => break,
                };

                let probes = probes_for(&config.games);
                let running = running_games(&probes, &lister.cmdlines());
                let mut woke = false;
                for event in tracker.update(&running, SystemTime::now()) {
                    if tx.send(WatchEvent::Session(event)).is_err() {
                        return;
                    }
                    woke = true;
                }

                if last_files.elapsed() >= FILES_INTERVAL {
                    last_files = Instant::now();
                    let states: HashMap<String, LiveState> = config
                        .games
                        .iter()
                        .filter(|g| !g.is_placeholder())
                        .map(|g| (g.id.clone(), live_state(&config, g)))
                        .collect();
                    if tx.send(WatchEvent::LiveStates(states)).is_err() {
                        return;
                    }
                    woke = true;
                }

                if woke {
                    wake();
                }
                thread::sleep(PROCESS_INTERVAL);
            }
        })
        .ok();

    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn probe(id: &str, app: Option<&str>, dir: Option<&str>) -> GameProbe {
        GameProbe {
            id: id.into(),
            steam_app_id: app.map(str::to_string),
            install_dir: dir.map(str::to_string),
            retroarch_rom: None,
        }
    }

    #[test]
    fn recognises_retroarch_rom_processes() {
        let probes = vec![GameProbe {
            id: "retroarch:black_europe".into(),
            steam_app_id: None,
            install_dir: Some("/home/u/Games/ROMs/ps2".into()),
            retroarch_rom: Some("/home/u/Games/ROMs/ps2/Black (Europe).iso".into()),
        }];
        let procs = vec![
            args(&[
                "/usr/bin/retroarch",
                "-L",
                "/usr/lib/libretro/pcsx2_libretro.so",
                "/home/u/Games/ROMs/ps2/Black (Europe).iso",
            ]),
            args(&["/usr/bin/dolphin", "/home/u/Games/ROMs/ps2"]),
        ];
        let running = running_games(&probes, &procs);
        assert_eq!(running.len(), 1);
        assert!(running.contains("retroarch:black_europe"));
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recognises_steam_launcher_proton_and_native_processes() {
        let probes = vec![
            probe("steam:elden_ring", Some("1245620"), Some("/mnt/T7/SteamLibrary/steamapps/common/ELDEN RING")),
            probe("gog:cyberpunk", None, Some("/mnt/T7/Cyberpunk 2077")),
            probe("other:native", None, Some("/home/u/Games/Native Game")),
        ];
        let procs = vec![
            args(&["/usr/bin/dolphin", "/mnt/T7/SteamLibrary/steamapps/common/ELDEN RING"]),
            args(&["/home/u/.steam/ubuntu12_32/reaper", "SteamLaunch", "AppId=1245620", "--", "proton"]),
            args(&[r"Z:\mnt\T7\Cyberpunk 2077\bin\x64\Cyberpunk2077.exe", "-skipStartScreen"]),
            args(&["/home/u/Games/Native Game/game.x86_64"]),
            args(&["/usr/bin/firefox"]),
        ];
        let running = running_games(&probes, &procs);
        assert_eq!(running.len(), 3);

        // A file manager showing the install folder is not the game.
        let only_dolphin = running_games(&probes, &procs[..1]);
        assert!(only_dolphin.is_empty());

        // Different AppId with the same prefix must not match.
        let other = running_games(&probes[..1], &[args(&["reaper", "SteamLaunch", "AppId=12456200"])]);
        assert!(other.is_empty());

        // Wine drive letters other than Z: still match on the install folder name.
        let s_drive = running_games(&probes[..1], &[args(&[r"S:\common\ELDEN RING\Game\eldenring.exe"])]);
        assert_eq!(s_drive.len(), 1);
    }

    #[test]
    fn probes_skip_games_without_identifying_facts() {
        let games = vec![
            GameEntry {
                id: "steam:a".into(),
                provider: Provider::Steam,
                app_id: Some("10".into()),
                ..Default::default()
            },
            GameEntry {
                id: "other:b".into(),
                provider: Provider::Other,
                install_dir: Some(PathBuf::from("/x")),
                ..Default::default()
            },
            GameEntry {
                id: "gog:c".into(),
                provider: Provider::Gog,
                app_id: Some("123".into()),
                install_dir: Some(PathBuf::from("/games/Some Game/")),
                ..Default::default()
            },
        ];
        let probes = probes_for(&games);
        assert_eq!(probes.len(), 2);
        assert_eq!(probes[0].steam_app_id.as_deref(), Some("10"));
        assert_eq!(probes[1].install_dir.as_deref(), Some("/games/Some Game"));
        assert_eq!(probes[1].steam_app_id, None, "GOG ids are not Steam launcher ids");
    }

    #[test]
    fn sessions_end_only_after_grace_period() {
        let mut t = SessionTracker::new(2);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let on: BTreeSet<String> = ["g".to_string()].into();
        let off = BTreeSet::new();

        assert_eq!(t.update(&on, t0), vec![SessionEvent::Started { game_id: "g".into() }]);
        assert!(t.update(&on, t0).is_empty());
        assert!(t.update(&off, t0).is_empty(), "first miss is tolerated");
        assert!(t.update(&on, t0).is_empty(), "came back: same session");
        assert!(t.update(&off, t0).is_empty());
        assert!(t.update(&off, t0).is_empty());
        assert_eq!(
            t.update(&off, t0),
            vec![SessionEvent::Ended { game_id: "g".into(), started_at: t0 }]
        );
        assert!(t.running_ids().is_empty());
    }
}
