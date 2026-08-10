#!/bin/sh
# StateRoot one-line installer (POSIX).
#
#   curl -fsSL https://github.com/CognizTech/stateroot/releases/latest/download/install.sh | sh
#
# Downloads the platform binary + checksums.txt from the latest release,
# verifies sha256 (fail closed), installs to ~/.local/bin.
#
# Pre-public testing: point at a local directory containing the assets —
#   STATEROOT_INSTALL_BASE=file:///path/to/assets sh install.sh
set -eu

REPO="${STATEROOT_INSTALL_REPO:-CognizTech/stateroot}"
BASE="${STATEROOT_INSTALL_BASE:-https://github.com/${REPO}/releases/latest/download}"

log() { printf '%s\n' "stateroot-install: $*"; }
fail() { printf '%s\n' "stateroot-install: ERROR: $*" >&2; exit 1; }

# --- platform detection ----------------------------------------------------
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
    Linux)  [ "$ARCH" = "x86_64" ] || fail "unsupported arch: $ARCH"; TARGET="linux-x64" ;;
    Darwin) [ "$ARCH" = "arm64" ] || fail "unsupported arch: $ARCH (need Apple Silicon)"; TARGET="macos-aarch64" ;;
    *)      fail "unsupported OS: $OS (use install.ps1 on Windows)" ;;
esac
ASSET="stateroot-$TARGET"

# --- download --------------------------------------------------------------
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

fetch() {
    # fetch <url-or-file-url> <dest>
    case "$1" in
        file://*)
            SRC="${1#file://}"
            [ -f "$SRC" ] || fail "missing local asset: $SRC"
            cp "$SRC" "$2"
            ;;
        *)
            if command -v curl >/dev/null 2>&1; then
                curl -fsSL "$1" -o "$2" || fail "download failed: $1"
            elif command -v wget >/dev/null 2>&1; then
                wget -q "$1" -O "$2" || fail "download failed: $1"
            else
                fail "need curl or wget"
            fi
            ;;
    esac
}

log "fetching $ASSET (+ checksums.txt) from $BASE"
fetch "$BASE/$ASSET" "$WORK/$ASSET"
fetch "$BASE/checksums.txt" "$WORK/checksums.txt"

# --- verify (fail closed) --------------------------------------------------
EXPECTED="$(grep " $ASSET\$" "$WORK/checksums.txt" | awk '{print $1}')"
[ -n "$EXPECTED" ] || fail "checksums.txt has no entry for $ASSET — refusing to install"
if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL="$(sha256sum "$WORK/$ASSET" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
    ACTUAL="$(shasum -a 256 "$WORK/$ASSET" | awk '{print $1}')"
else
    fail "need sha256sum or shasum to verify the download"
fi
[ "$ACTUAL" = "$EXPECTED" ] || fail "checksum mismatch for $ASSET (expected $EXPECTED, got $ACTUAL)"
log "checksum verified"

# --- install ---------------------------------------------------------------
DEST_DIR="$HOME/.local/bin"
mkdir -p "$DEST_DIR"
install -m 0755 "$WORK/$ASSET" "$DEST_DIR/stateroot" 2>/dev/null || cp "$WORK/$ASSET" "$DEST_DIR/stateroot"
chmod 0755 "$DEST_DIR/stateroot" 2>/dev/null || true
log "installed to $DEST_DIR/stateroot"

# --- PATH ------------------------------------------------------------------
case ":$PATH:" in
    *":$DEST_DIR:"*) ;;
    *)
        log "note: $DEST_DIR is not on your PATH — add this to your shell profile:"
        printf '  export PATH="$HOME/.local/bin:$PATH"\n'
        ;;
esac

cat <<'EOF'

Quickstart:
  1. cd your-project && stateroot init
  2. work in any harness (Claude, Codex, Cursor, Kimi, OpenClaw, Hermes)
  3. stateroot resume — anywhere, picks up the full working state
EOF
