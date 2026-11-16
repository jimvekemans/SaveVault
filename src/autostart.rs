use crate::logo::{SVG_LOGO, build_app_icon_png};
use crate::models::Platform;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Returns the platform-standard startup entry location for SaveVault.
pub fn default_autostart_path(platform: Platform) -> PathBuf {
    match platform {
        Platform::Linux => dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("autostart")
            .join("savevault.desktop"),
        Platform::MacOs => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library")
            .join("LaunchAgents")
            .join("io.savevault.app.plist"),
        Platform::Windows => dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
            .join("SaveVault.cmd"),
    }
}

/// Writes or removes the startup launcher at `target_path` for `platform`.
pub fn configure_autostart_at(
    enabled: bool,
    platform: Platform,
    target_path: &Path,
    exe_path: &Path,
    icon_path: Option<&Path>,
) -> Result<()> {
    if !enabled {
        if target_path.exists() {
            fs::remove_file(target_path).with_context(|| {
                format!("Failed to remove startup entry {}", target_path.display())
            })?;
        }
        return Ok(());
    }

    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create startup directory {}", parent.display())
        })?;
    }

    let exe_str = exe_path.display().to_string();
    let contents = match platform {
        Platform::Linux => {
            let icon_line = icon_path
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "savevault".to_string());
            format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Version=1.0\n\
                 Name=SaveVault\n\
                 Comment=Lightweight Git-backed Game Save Sync & Rollback\n\
                 Exec=\"{exe_str}\" --minimized\n\
                 Icon={icon_line}\n\
                 Terminal=false\n\
                 StartupNotify=false\n\
                 StartupWMClass=savevault\n\
                 X-GNOME-Autostart-enabled=true\n"
            )
        }
        Platform::MacOs => {
            let escaped = exe_str
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <plist version=\"1.0\">\n\
                 <dict>\n\
                   <key>Label</key>\n\
                   <string>io.savevault.app</string>\n\
                   <key>ProgramArguments</key>\n\
                   <array>\n\
                     <string>{escaped}</string>\n\
                     <string>--minimized</string>\n\
                   </array>\n\
                   <key>RunAtLoad</key>\n\
                   <true/>\n\
                 </dict>\n\
                 </plist>\n"
            )
        }
        Platform::Windows => {
            format!("@echo off\r\nstart \"\" \"{exe_str}\" --minimized\r\n")
        }
    };

    fs::write(target_path, contents)
        .with_context(|| format!("Failed to write startup entry {}", target_path.display()))?;
    Ok(())
}

fn resolve_launch_exe() -> Result<PathBuf> {
    // Inside an AppImage the running binary lives in a temporary mount that disappears on exit;
    // the AppImage file itself is what has to be launched.
    match std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file()) {
        Some(appimage) => Ok(appimage),
        None => std::env::current_exe().context("Could not determine current executable path"),
    }
}

fn write_asset(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, bytes);
}

/// Installs SaveVault's `.desktop` entry and icons under `$XDG_DATA_HOME` so Linux
/// Wayland/X11 docks and taskbars match the running window (`app_id = "savevault"`) to its icon.
pub fn ensure_linux_desktop_integration() {
    if Platform::current() != Platform::Linux {
        return;
    }
    let Some(data_dir) = dirs::data_local_dir() else {
        return;
    };
    let Ok(exe_path) = resolve_launch_exe() else {
        return;
    };

    let svg_hicolor = data_dir
        .join("icons")
        .join("hicolor")
        .join("scalable")
        .join("apps")
        .join("savevault.svg");
    let svg_root = data_dir.join("icons").join("savevault.svg");
    write_asset(&svg_hicolor, SVG_LOGO.as_bytes());
    write_asset(&svg_root, SVG_LOGO.as_bytes());

    let png_bytes = build_app_icon_png();
    let png_64 = data_dir
        .join("icons")
        .join("hicolor")
        .join("64x64")
        .join("apps")
        .join("savevault.png");
    write_asset(&png_64, &png_bytes);

    let png_256 = data_dir
        .join("icons")
        .join("hicolor")
        .join("256x256")
        .join("apps")
        .join("savevault.png");
    if !png_256.is_file() {
        let pixmap = data_dir.join("pixmaps").join("savevault.png");
        write_asset(&pixmap, &png_bytes);
    }

    let desktop_path = data_dir.join("applications").join("savevault.desktop");
    let desktop_contents = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name=SaveVault\n\
         GenericName=Game Save Manager\n\
         Comment=Automatic game save backup, profiles, rollback and sync\n\
         Exec=\"{}\"\n\
         Icon={}\n\
         Terminal=false\n\
         Categories=Utility;Game;\n\
         StartupWMClass=savevault\n",
        exe_path.display(),
        svg_hicolor.display()
    );
    write_asset(&desktop_path, desktop_contents.as_bytes());
}

/// Configures SaveVault to run (or stop running) automatically at login on the current platform.
pub fn set_system_autostart(enabled: bool) -> Result<PathBuf> {
    let platform = Platform::current();
    let target_path = default_autostart_path(platform);
    let exe_path = resolve_launch_exe()?;

    let icon_path = if enabled && platform == Platform::Linux {
        let icon_file = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("icons")
            .join("hicolor")
            .join("scalable")
            .join("apps")
            .join("savevault.svg");
        write_asset(&icon_file, SVG_LOGO.as_bytes());
        Some(icon_file)
    } else {
        None
    };

    configure_autostart_at(
        enabled,
        platform,
        &target_path,
        &exe_path,
        icon_path.as_deref(),
    )?;
    Ok(target_path)
}
