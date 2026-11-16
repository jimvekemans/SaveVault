#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
ARCH="$(uname -m)"

SKIP_CARGO_BUILD=0
OUTPUT_PATH="${PROJECT_DIR}/dist/SaveVault-${ARCH}.AppImage"

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Builds the release binary (target/release/save_game_manager) and packages it
into an executable Linux AppImage.

Options:
  --skip-cargo-build    Reuse existing target/release/save_game_manager binary
  --output <PATH>       Custom output AppImage path (default: dist/SaveVault-${ARCH}.AppImage)
  -h, --help            Show this help message and exit
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-cargo-build)
            SKIP_CARGO_BUILD=1
            shift
            ;;
        --output)
            if [[ $# -lt 2 ]]; then
                echo "Error: --output requires a file path argument" >&2
                exit 1
            fi
            OUTPUT_PATH="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

RELEASE_BIN="${PROJECT_DIR}/target/release/save_game_manager"
if [[ "${SKIP_CARGO_BUILD}" -eq 0 ]]; then
    echo "==> Building release binary via cargo build --release..."
    cargo build --release --manifest-path "${PROJECT_DIR}/Cargo.toml"
fi

if [[ ! -x "${RELEASE_BIN}" ]]; then
    echo "Error: release binary not found or not executable at ${RELEASE_BIN}" >&2
    exit 1
fi

WORK_DIR="${PROJECT_DIR}/target/appimage"
APPDIR="${WORK_DIR}/SaveVault.AppDir"
rm -rf "${APPDIR}"
mkdir -p \
    "${APPDIR}/usr/bin" \
    "${APPDIR}/usr/share/applications" \
    "${APPDIR}/usr/share/icons/hicolor/256x256/apps" \
    "${APPDIR}/usr/share/icons/hicolor/scalable/apps" \
    "${APPDIR}/usr/share/metainfo"

echo "==> Staging SaveVault.AppDir from ${RELEASE_BIN}..."
install -m 755 "${RELEASE_BIN}" "${APPDIR}/usr/bin/save_game_manager"

ICON_SRC="${PROJECT_DIR}/assets/savevault.svg"
if [[ ! -f "${ICON_SRC}" ]]; then
    echo "Error: icon file missing at ${ICON_SRC}" >&2
    exit 1
fi
install -m 644 "${ICON_SRC}" "${APPDIR}/usr/share/icons/hicolor/scalable/apps/savevault.svg"

ICON_PNG="${APPDIR}/savevault.png"
if command -v rsvg-convert >/dev/null 2>&1; then
    rsvg-convert -w 256 -h 256 "${ICON_SRC}" -o "${ICON_PNG}"
elif command -v magick >/dev/null 2>&1; then
    magick -background none -density 256 "${ICON_SRC}" -resize 256x256 "${ICON_PNG}"
elif command -v convert >/dev/null 2>&1; then
    convert -background none -density 256 "${ICON_SRC}" -resize 256x256 "${ICON_PNG}"
fi
if [[ -f "${ICON_PNG}" ]]; then
    install -m 644 "${ICON_PNG}" "${APPDIR}/.DirIcon"
    install -m 644 "${ICON_PNG}" "${APPDIR}/usr/share/icons/hicolor/256x256/apps/savevault.png"
else
    install -m 644 "${ICON_SRC}" "${APPDIR}/savevault.svg"
    install -m 644 "${ICON_SRC}" "${APPDIR}/.DirIcon"
fi

APP_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "${PROJECT_DIR}/Cargo.toml" | head -n1)"
cat > "${APPDIR}/savevault.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=SaveVault
GenericName=Game Save Manager
Comment=Automatic game save backup, profiles, rollback and sync
Exec=save_game_manager
Icon=savevault
Terminal=false
Categories=Utility;
Keywords=game;save;backup;git;github;steam;gog;epic;
StartupWMClass=savevault
X-AppImage-Version=${APP_VERSION}
EOF
install -m 644 "${APPDIR}/savevault.desktop" "${APPDIR}/usr/share/applications/savevault.desktop"

cat > "${APPDIR}/usr/share/metainfo/io.savevault.app.appdata.xml" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>io.savevault.app</id>
  <name>SaveVault</name>
  <summary>Automatic game save backup, profiles, rollback and sync</summary>
  <metadata_license>MIT</metadata_license>
  <description>
    <p>SaveVault finds your installed games and their save folders, snapshots every change, restores any earlier point, keeps separate profiles per game, records mod setups, and syncs between PCs through a private git repository.</p>
  </description>
  <launchable type="desktop-id">savevault.desktop</launchable>
  <provides>
    <binary>save_game_manager</binary>
  </provides>
</component>
EOF

cat > "${APPDIR}/AppRun" <<'EOF'
#!/usr/bin/env sh
HERE="$(dirname "$(readlink -f "${0}")")"
if [ -n "${XDG_DATA_HOME:-}" ] || [ -n "${HOME:-}" ]; then
    DATA_DIR="${XDG_DATA_HOME:-${HOME}/.local/share}"
    ICON_SVG="${DATA_DIR}/icons/hicolor/scalable/apps/savevault.svg"
    mkdir -p \
        "${DATA_DIR}/applications" \
        "${DATA_DIR}/icons/hicolor/scalable/apps" \
        "${DATA_DIR}/icons/hicolor/256x256/apps"
    cp -f "${HERE}/usr/share/icons/hicolor/scalable/apps/savevault.svg" "${ICON_SVG}" 2>/dev/null || true
    if [ -f "${HERE}/usr/share/icons/hicolor/256x256/apps/savevault.png" ]; then
        cp -f "${HERE}/usr/share/icons/hicolor/256x256/apps/savevault.png" \
            "${DATA_DIR}/icons/hicolor/256x256/apps/savevault.png" 2>/dev/null || true
    fi
    TARGET_EXE="${APPIMAGE:-${HERE}/usr/bin/save_game_manager}"
    cat > "${DATA_DIR}/applications/savevault.desktop" <<DESKTOP_EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=SaveVault
GenericName=Game Save Manager
Comment=Automatic game save backup, profiles, rollback and sync
Exec="${TARGET_EXE}"
Icon=${ICON_SVG}
Terminal=false
Categories=Utility;Game;
StartupWMClass=savevault
DESKTOP_EOF
fi
export PATH="${HERE}/usr/bin:${PATH}"
exec "${HERE}/usr/bin/save_game_manager" "$@"
EOF
chmod 755 "${APPDIR}/AppRun"

resolve_appimagetool() {
    if command -v appimagetool >/dev/null 2>&1; then
        command -v appimagetool
        return 0
    fi

    local cached="${WORK_DIR}/appimagetool-${ARCH}.AppImage"
    if [[ ! -x "${cached}" ]]; then
        local url="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${ARCH}.AppImage"
        echo "==> Downloading appimagetool from ${url}..." >&2
        mkdir -p "${WORK_DIR}"
        if command -v curl >/dev/null 2>&1; then
            curl -fsSL "${url}" -o "${cached}"
        elif command -v wget >/dev/null 2>&1; then
            wget -qO "${cached}" "${url}"
        else
            echo "Error: neither appimagetool nor curl/wget is installed." >&2
            return 1
        fi
        chmod 755 "${cached}"
    fi
    printf '%s\n' "${cached}"
}

APPIMAGETOOL="$(resolve_appimagetool)"
mkdir -p "$(dirname -- "${OUTPUT_PATH}")"

echo "==> Packaging AppImage at ${OUTPUT_PATH}..."
if "${APPIMAGETOOL}" --version >/dev/null 2>&1; then
    ARCH="${ARCH}" "${APPIMAGETOOL}" --no-appstream "${APPDIR}" "${OUTPUT_PATH}"
else
    ARCH="${ARCH}" "${APPIMAGETOOL}" --appimage-extract-and-run --no-appstream "${APPDIR}" "${OUTPUT_PATH}"
fi
chmod 755 "${OUTPUT_PATH}"

DEFAULT_LINK="${PROJECT_DIR}/SaveVault-${ARCH}.AppImage"
if [[ "${OUTPUT_PATH}" == "${PROJECT_DIR}/dist/SaveVault-${ARCH}.AppImage" ]]; then
    ln -sfn "dist/SaveVault-${ARCH}.AppImage" "${DEFAULT_LINK}"
fi

echo "==> Built AppImage successfully:"
ls -lh "${OUTPUT_PATH}"
