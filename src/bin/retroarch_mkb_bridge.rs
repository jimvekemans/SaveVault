//! Multi-game Mouse & Keyboard (`evdev` -> `uinput`) bridge and playlist utility for RetroArch.

use anyhow::{Context, Result, bail};
use save_game_manager::retroarch::{default_kbm_profiles_path, ensure_playlist_item};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "linux")]
#[path = "../kbm/evdev.rs"]
mod evdev;

#[cfg(target_os = "linux")]
#[path = "../kbm/bridge.rs"]
mod bridge;

#[cfg(target_os = "linux")]
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "linux")]
extern "C" fn handle_signal(_sig: libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::Relaxed);
}

fn print_help() {
    println!(
        "\
retroarch-mkb-bridge - Multi-game RetroArch Mouse & Keyboard bridge

USAGE:
    retroarch-mkb-bridge --watch [--profiles <path>]
    retroarch-mkb-bridge --active [--game <id-or-rom>] [--profiles <path>]
    retroarch-mkb-bridge --self-test
    retroarch-mkb-bridge --ensure-playlist <lpl> <rom> <label> <core_path> <core_name> [db_name]

Modes:
    --watch             Watch running RetroArch processes, select each game's KBM profile, and live hot-reload changes
    --active            Activate virtual Mouse+KB controller immediately
    --self-test         Verify /dev/uinput creation, axis ranges (-32767..32767 / -1..1), and look math
    --ensure-playlist   Ensure a ROM item exists in a RetroArch JSON .lpl playlist"
    );
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }

    if args.iter().any(|a| a == "--ensure-playlist") {
        let rest: Vec<&String> = args.iter().filter(|a| *a != "--ensure-playlist").collect();
        if rest.len() < 5 {
            bail!(
                "Usage: --ensure-playlist <lpl_path> <rom_path> <label> <core_path> <core_name> [db_name]"
            );
        }
        ensure_playlist_item(
            &PathBuf::from(rest[0]),
            &PathBuf::from(rest[1]),
            rest[2],
            &PathBuf::from(rest[3]),
            rest[4],
            rest.get(5).map(|s| s.as_str()),
        )?;
        return Ok(());
    }

    #[cfg(not(target_os = "linux"))]
    {
        bail!("retroarch-mkb-bridge is currently supported on Linux only.");
    }

    #[cfg(target_os = "linux")]
    {
        use bridge::{BridgeMode, run_bridge, self_test};

        if args.iter().any(|a| a == "--self-test") {
            return self_test();
        }

        let mut watch = true;
        let mut game_filter: Option<String> = None;
        let mut profiles_path = default_kbm_profiles_path();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--watch" => watch = true,
                "--active" => watch = false,
                "--game" => {
                    let val = args
                        .get(i + 1)
                        .context("--game requires a game id or ROM name")?;
                    game_filter = Some(val.clone());
                    watch = false;
                    i += 1;
                }
                "--profiles" => {
                    let val = args
                        .get(i + 1)
                        .context("--profiles requires a JSON file path")?;
                    profiles_path = PathBuf::from(val);
                    i += 1;
                }
                other => bail!("Unknown argument: {other}"),
            }
            i += 1;
        }

        let stop = Arc::new(AtomicBool::new(false));
        let stop_watcher = stop.clone();
        unsafe {
            libc::signal(libc::SIGINT, handle_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, handle_signal as *const () as libc::sighandler_t);
        }
        std::thread::spawn(move || {
            while !STOP_REQUESTED.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            stop_watcher.store(true, Ordering::Relaxed);
        });

        let mode = if watch {
            BridgeMode::Watch
        } else {
            BridgeMode::ActiveAll(game_filter)
        };
        run_bridge(mode, &profiles_path, stop)
    }
}
