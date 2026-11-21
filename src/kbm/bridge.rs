//! 250 Hz Linux evdev -> uinput Mouse & Keyboard live translation daemon.

use crate::evdev::{
    ABS_HAT0X, ABS_HAT0Y, ABS_RX, ABS_RY, ABS_X, ABS_Y, ALL_VIRTUAL_BUTTONS, BTN_TR, EV_ABS,
    EV_KEY, EV_REL, EvdevSource, InputEvent, REL_WHEEL, REL_X, REL_Y, UinputGamepad,
    VIRTUAL_DEVICE_NAME, scan_keyboard_and_mouse_devices, verify_virtual_device_node,
};
use anyhow::{Result, bail};
use save_game_manager::kbm::keys::{KbmProfile, PSEUDO_WHEEL_DOWN, PSEUDO_WHEEL_UP};
use save_game_manager::kbm::mapping::{ComboResolver, MouseLookState, ResolvedControllerState};
use save_game_manager::retroarch::{
    KbmGameProfileEntry, KbmProfileRegistry, is_retroarch_executable_name,
    load_kbm_profiles_registry,
};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read as _;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

struct LiveMapper {
    mouse: MouseLookState,
    resolver: ComboResolver,
    pressed_codes: HashSet<u16>,
    wheel_pulses: HashMap<u16, u8>,
    last_state: ResolvedControllerState,
    last_rx: i32,
    last_ry: i32,
}

impl LiveMapper {
    fn new(profile: &KbmProfile, game_hint: &str) -> Self {
        Self {
            mouse: MouseLookState::from_profile_for_game(profile, game_hint),
            resolver: ComboResolver::from_profile(profile),
            pressed_codes: HashSet::new(),
            wheel_pulses: HashMap::new(),
            last_state: ResolvedControllerState::default(),
            last_rx: 0,
            last_ry: 0,
        }
    }

    fn update_profile(
        &mut self,
        ui: &UinputGamepad,
        profile: &KbmProfile,
        game_hint: &str,
    ) -> Result<()> {
        self.mouse.update_from_profile_for_game(profile, game_hint);
        self.resolver.rebuild(profile);
        if self.reconcile(ui)? {
            ui.syn()?;
        }
        Ok(())
    }

    fn handle_key(&mut self, ui: &UinputGamepad, code: u16, value: i32) -> Result<bool> {
        if value == 2 {
            return Ok(false);
        }
        let down = value == 1;
        if down {
            if !self.pressed_codes.insert(code) {
                return Ok(false);
            }
        } else if !self.pressed_codes.remove(&code) {
            return Ok(false);
        }
        self.reconcile(ui)
    }

    fn trigger_wheel(&mut self, ui: &UinputGamepad, wheel_code: u16) -> Result<bool> {
        let already_active = self.wheel_pulses.contains_key(&wheel_code);
        self.wheel_pulses.insert(wheel_code, 10); // ~40ms at 250Hz
        if !already_active {
            return self.reconcile(ui);
        }
        Ok(false)
    }

    fn decay_wheel_pulses(&mut self, ui: &UinputGamepad) -> Result<bool> {
        let mut expired = Vec::new();
        for (code, ticks) in self.wheel_pulses.iter_mut() {
            if *ticks <= 1 {
                expired.push(*code);
            } else {
                *ticks -= 1;
            }
        }
        if expired.is_empty() {
            return Ok(false);
        }
        for code in expired {
            self.wheel_pulses.remove(&code);
        }
        self.reconcile(ui)
    }

    fn reconcile(&mut self, ui: &UinputGamepad) -> Result<bool> {
        let mut held = self.pressed_codes.clone();
        for &code in self.wheel_pulses.keys() {
            held.insert(code);
        }
        let next = self.resolver.resolve(&held);
        let mut dirty = false;

        for (idx, (&was, &is)) in self
            .last_state
            .buttons
            .iter()
            .zip(next.buttons.iter())
            .enumerate()
        {
            if was != is {
                ui.write_event(EV_KEY, ALL_VIRTUAL_BUTTONS[idx], if is { 1 } else { 0 })?;
                dirty = true;
            }
        }
        if next.lx != self.last_state.lx {
            ui.write_event(EV_ABS, ABS_X, next.lx)?;
            dirty = true;
        }
        if next.ly != self.last_state.ly {
            ui.write_event(EV_ABS, ABS_Y, next.ly)?;
            dirty = true;
        }
        if next.hx != self.last_state.hx {
            ui.write_event(EV_ABS, ABS_HAT0X, next.hx)?;
            dirty = true;
        }
        if next.hy != self.last_state.hy {
            ui.write_event(EV_ABS, ABS_HAT0Y, next.hy)?;
            dirty = true;
        }
        self.last_state = next;
        Ok(dirty)
    }

    fn tick_mouse(&mut self, ui: &UinputGamepad, dx: f64, dy: f64) -> Result<bool> {
        let (rx, ry) = self.mouse.compute_right_stick(dx, dy);
        let mut dirty = false;
        if rx != self.last_rx {
            ui.write_event(EV_ABS, ABS_RX, rx)?;
            self.last_rx = rx;
            dirty = true;
        }
        if ry != self.last_ry {
            ui.write_event(EV_ABS, ABS_RY, ry)?;
            self.last_ry = ry;
            dirty = true;
        }
        Ok(dirty)
    }
}

/// Inspects running RetroArch processes (native, Flatpak, or AppImage) and returns any matching
/// enabled KBM profile entry.
fn detect_active_kbm_game(reg: &KbmProfileRegistry) -> Option<KbmGameProfileEntry> {
    let Ok(procs) = fs::read_dir("/proc") else {
        return None;
    };
    for entry in procs.flatten() {
        let pid = entry.file_name();
        if !pid.to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let pdir = entry.path();
        let comm = fs::read_to_string(pdir.join("comm")).unwrap_or_default();
        let mut cmdline = Vec::new();
        if let Ok(mut f) = fs::File::open(pdir.join("cmdline")) {
            let _ = f.read_to_end(&mut cmdline);
        }
        let first_arg = cmdline
            .split(|&b| b == 0)
            .next()
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .unwrap_or_default();
        if !is_retroarch_executable_name(&first_arg) && !is_retroarch_executable_name(comm.trim()) {
            continue;
        }
        let cmd_str = String::from_utf8_lossy(&cmdline).replace('\0', " ");

        let mut open_fds = Vec::new();
        if let Ok(fds) = fs::read_dir(pdir.join("fd")) {
            for fd in fds.flatten() {
                if let Ok(target) = fs::read_link(fd.path()) {
                    open_fds.push(target.to_string_lossy().into_owned());
                }
            }
        }

        for game in &reg.games {
            if !game.enabled {
                continue;
            }
            let rom_str = game.rom_path.to_string_lossy();
            if (!rom_str.is_empty() && cmd_str.contains(rom_str.as_ref()))
                || (!game.rom_stem.is_empty() && cmd_str.contains(&game.rom_stem))
                || open_fds.iter().any(|t| {
                    (!rom_str.is_empty() && t == rom_str.as_ref())
                        || (!game.rom_stem.is_empty() && t.contains(&game.rom_stem))
                })
            {
                return Some(game.clone());
            }
        }
    }
    None
}

pub fn self_test() -> Result<()> {
    let ui = UinputGamepad::create(VIRTUAL_DEVICE_NAME)?;
    let mut mouse = MouseLookState::from_profile(&KbmProfile::default());
    let (rx, ry) = mouse.compute_right_stick(12.0, -4.0);
    if rx <= 0 || ry >= 0 {
        bail!("unexpected right stick test output: ({rx}, {ry})");
    }
    ui.write_event(EV_ABS, ABS_RX, rx)?;
    ui.write_event(EV_ABS, ABS_RY, ry)?;
    ui.write_event(EV_KEY, BTN_TR, 1)?;
    ui.syn()?;

    std::thread::sleep(Duration::from_millis(200));
    let node = verify_virtual_device_node()?;

    ui.write_event(EV_KEY, BTN_TR, 0)?;
    ui.write_event(EV_ABS, ABS_RX, 0)?;
    ui.write_event(EV_ABS, ABS_RY, 0)?;
    ui.syn()?;
    println!(
        "[OK]   uinput virtual controller created at {} (test right stick={rx},{ry})",
        node.display()
    );
    Ok(())
}

pub enum BridgeMode {
    Watch,
    ActiveAll(Option<String>),
}

pub fn run_bridge(mode: BridgeMode, profiles_path: &Path, stop_flag: Arc<AtomicBool>) -> Result<()> {
    let mut last_mtime: Option<SystemTime> =
        fs::metadata(profiles_path).and_then(|m| m.modified()).ok();
    let mut reg = load_kbm_profiles_registry(profiles_path);

    while !stop_flag.load(Ordering::Relaxed) {
        let active_entry = match &mode {
            BridgeMode::Watch => match detect_active_kbm_game(&reg) {
                Some(g) => g,
                None => {
                    if let Ok(m) = fs::metadata(profiles_path).and_then(|m| m.modified())
                        && Some(m) != last_mtime
                    {
                        last_mtime = Some(m);
                        reg = load_kbm_profiles_registry(profiles_path);
                    }
                    std::thread::sleep(Duration::from_millis(400));
                    continue;
                }
            },
            BridgeMode::ActiveAll(Some(target)) => reg
                .games
                .iter()
                .find(|g| {
                    g.game_id == *target
                        || g.rom_stem.contains(target)
                        || g.name.contains(target)
                })
                .cloned()
                .unwrap_or_else(|| KbmGameProfileEntry {
                    game_id: target.clone(),
                    name: target.clone(),
                    rom_path: PathBuf::from(target),
                    rom_stem: target.clone(),
                    core_path: PathBuf::new(),
                    core_name: String::new(),
                    enabled: true,
                    profile: reg.default_profile.clone(),
                }),
            BridgeMode::ActiveAll(None) => reg
                .games
                .iter()
                .find(|g| g.enabled)
                .cloned()
                .unwrap_or_else(|| KbmGameProfileEntry {
                    game_id: "default".to_string(),
                    name: "Default KBM".to_string(),
                    rom_path: PathBuf::new(),
                    rom_stem: String::new(),
                    core_path: PathBuf::new(),
                    core_name: String::new(),
                    enabled: true,
                    profile: reg.default_profile.clone(),
                }),
        };

        let mut current_game_id = active_entry.game_id.clone();
        let mut current_game_hint = format!("{} {}", active_entry.name, active_entry.rom_stem);
        let ui = UinputGamepad::create(VIRTUAL_DEVICE_NAME)?;
        let mut mapper = LiveMapper::new(&active_entry.profile, &current_game_hint);
        let mut sources: HashMap<PathBuf, EvdevSource> = scan_keyboard_and_mouse_devices()
            .into_iter()
            .collect();

        let mut last_proc_check = Instant::now();
        let mut last_dev_scan = Instant::now();
        let mut last_cfg_check = Instant::now();
        let tick_dt = Duration::from_millis(4);

        while !stop_flag.load(Ordering::Relaxed) {
            let frame_start = Instant::now();

            if last_cfg_check.elapsed() >= Duration::from_millis(200) {
                last_cfg_check = Instant::now();
                if let Ok(m) = fs::metadata(profiles_path).and_then(|m| m.modified())
                    && Some(m) != last_mtime
                {
                    last_mtime = Some(m);
                    reg = load_kbm_profiles_registry(profiles_path);
                    if let Some(updated) = reg.games.iter().find(|g| g.game_id == current_game_id) {
                        if matches!(mode, BridgeMode::Watch) && !updated.enabled {
                            break;
                        }
                        current_game_hint = format!("{} {}", updated.name, updated.rom_stem);
                        let _ = mapper.update_profile(&ui, &updated.profile, &current_game_hint);
                    }
                }
            }

            if matches!(mode, BridgeMode::Watch)
                && last_proc_check.elapsed() >= Duration::from_secs(1)
            {
                last_proc_check = Instant::now();
                match detect_active_kbm_game(&reg) {
                    Some(now_game) if now_game.game_id == current_game_id => {}
                    Some(now_game) => {
                        current_game_id = now_game.game_id;
                        current_game_hint = format!("{} {}", now_game.name, now_game.rom_stem);
                        let _ = mapper.update_profile(&ui, &now_game.profile, &current_game_hint);
                    }
                    None => break,
                }
            }

            if last_dev_scan.elapsed() >= Duration::from_secs(3) {
                last_dev_scan = Instant::now();
                let found: HashMap<PathBuf, EvdevSource> =
                    scan_keyboard_and_mouse_devices().into_iter().collect();
                sources.retain(|p, _| found.contains_key(p));
                for (p, s) in found {
                    sources.entry(p).or_insert(s);
                }
            }

            let mut pollfds: Vec<libc::pollfd> = sources
                .values()
                .map(|s| libc::pollfd {
                    fd: s.fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            if !pollfds.is_empty() {
                unsafe {
                    libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, 4);
                }
            } else {
                std::thread::sleep(tick_dt);
            }

            let mut dx = 0.0f64;
            let mut dy = 0.0f64;
            let mut dirty = false;
            let mut dead_paths = Vec::new();
            let ev_size = std::mem::size_of::<InputEvent>();

            for (path, src) in &sources {
                let mut buf = [InputEvent::default(); 64];
                loop {
                    let ptr = buf.as_mut_ptr() as *mut libc::c_void;
                    let n = unsafe { libc::read(src.fd.as_raw_fd(), ptr, ev_size * buf.len()) };
                    if n < 0 {
                        let err = std::io::Error::last_os_error();
                        if err.kind() == std::io::ErrorKind::WouldBlock {
                            break;
                        }
                        dead_paths.push(path.clone());
                        break;
                    }
                    if n == 0 {
                        break;
                    }
                    let count = (n as usize) / ev_size;
                    for ev in &buf[..count] {
                        match ev.type_ {
                            EV_REL => match ev.code {
                                REL_X => dx += ev.value as f64,
                                REL_Y => dy += ev.value as f64,
                                REL_WHEEL => {
                                    let wheel_code = if ev.value > 0 {
                                        PSEUDO_WHEEL_UP
                                    } else {
                                        PSEUDO_WHEEL_DOWN
                                    };
                                    dirty |= mapper.trigger_wheel(&ui, wheel_code)?;
                                }
                                _ => {}
                            },
                            EV_KEY => {
                                dirty |= mapper.handle_key(&ui, ev.code, ev.value)?;
                            }
                            _ => {}
                        }
                    }
                }
            }

            for dead in dead_paths {
                sources.remove(&dead);
            }

            dirty |= mapper.decay_wheel_pulses(&ui)?;
            dirty |= mapper.tick_mouse(&ui, dx, dy)?;
            if dirty {
                ui.syn()?;
            }

            let elapsed = frame_start.elapsed();
            if elapsed < tick_dt {
                std::thread::sleep(tick_dt - elapsed);
            }
        }
    }
    Ok(())
}
