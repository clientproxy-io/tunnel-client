#!/bin/bash
# Build tunnel-client.exe for Windows (x86_64), package with NSSM, upload to GitHub Release.
#
# Prerequisites:
#   brew install mingw-w64
#   rustup target add x86_64-pc-windows-gnu
#   gh auth login
#
# Usage:
#   ./scripts/build-windows.sh

set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
TARGET="x86_64-pc-windows-gnu"
WINSW_VERSION="3.0.0-alpha.11"
WINSW_URL="https://github.com/winsw/winsw/releases/download/v${WINSW_VERSION}/WinSW-x64.exe"
OUT_DIR="$(pwd)/dist-windows"
ZIP_NAME="tunnel-client-windows-${VERSION}.zip"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info()  { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*"; exit 1; }

# --- Prerequisites ---
command -v x86_64-w64-mingw32-gcc &>/dev/null || error "mingw-w64 not found. Run: brew install mingw-w64"
rustup target list --installed | grep -q "$TARGET"  || error "Rust target missing. Run: rustup target add $TARGET"
command -v gh  &>/dev/null || error "gh CLI not found. Run: brew install gh"
command -v curl &>/dev/null || error "curl not found"
command -v unzip &>/dev/null || error "unzip not found"

# --- Build ---
info "Building tunnel-client.exe for ${TARGET}..."
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc

cargo build --bin tunnel-client --target "$TARGET" --release

info "Stripping binary..."
x86_64-w64-mingw32-strip --strip-all "target/${TARGET}/release/tunnel-client.exe" 2>/dev/null || true

# --- Download WinSW ---
info "Downloading WinSW ${WINSW_VERSION}..."
WINSW_EXE="/tmp/WinSW-x64.exe"
if [[ ! -f "$WINSW_EXE" ]]; then
    curl -fsSL "$WINSW_URL" -o "$WINSW_EXE"
fi

# --- Package ---
info "Creating ZIP package..."
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

cp "target/${TARGET}/release/tunnel-client.exe" "$OUT_DIR/"
cp "$WINSW_EXE" "$OUT_DIR/tunnel-client-svc.exe"
cp "scripts/windows-install.ps1"   "$OUT_DIR/install.ps1"
cp "scripts/windows-uninstall.ps1" "$OUT_DIR/uninstall.ps1"
cp "scripts/tunnel-client-svc.xml" "$OUT_DIR/"

cat > "$OUT_DIR/README.txt" <<EOF
clientproxy.io Tunnel Client ${VERSION} — Windows
==========================================

INSTALL (run PowerShell as Administrator):
  1. Right-click install.ps1 → "Run with PowerShell"
     (or: powershell -ExecutionPolicy Bypass -File install.ps1)
  2. Edit C:\ProgramData\clientproxy\tunnel-client\env.conf with your credentials
  3. Run: Start-Service tunnel-client

LOGS:
  C:\Program Files\clientproxy\tunnel-client\tunnel-client.log

UNINSTALL:
  Right-click uninstall.ps1 → "Run with PowerShell"

For help: https://clientproxy.io/docs
EOF

rm -f "${ZIP_NAME}"
(cd "$OUT_DIR" && zip -r "../${ZIP_NAME}" .)
info "Created ${ZIP_NAME}"
ls -lh "${ZIP_NAME}"

# --- Publish to GitHub Release ---
info "Publishing to GitHub Release v${VERSION}..."
TAG="v${VERSION}"

if gh release view "$TAG" &>/dev/null; then
    info "Release ${TAG} already exists — uploading asset..."
    gh release upload "$TAG" "${ZIP_NAME}" --clobber
else
    info "Creating release ${TAG}..."
    gh release create "$TAG" "${ZIP_NAME}" \
        --title "Release ${TAG}" \
        --notes "tunnel-proxy and tunnel-client v${VERSION}" \
        --draft
fi

rm -rf "$OUT_DIR"

info "Done! Windows package published to GitHub Release ${TAG}."
