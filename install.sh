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

# Native macOS apps use the system proxy, but curl only reads proxy environment
# variables. Apply a static HTTPS system proxy to this download subprocess when
# the user has not already supplied an explicit proxy. PAC scripts are not run.
# Used by the binary download AND the install ping.
maybe_apply_system_proxy() {
    case "$OS:$1" in
        Darwin:https://*)
            if [ -z "${https_proxy:-}${HTTPS_PROXY:-}${all_proxy:-}${ALL_PROXY:-}" ] && command -v scutil >/dev/null 2>&1; then
                SYSTEM_PROXY_SETTINGS="$(scutil --proxy 2>/dev/null)" || SYSTEM_PROXY_SETTINGS=""
                SYSTEM_PROXY_ENABLED="$(printf '%s\n' "$SYSTEM_PROXY_SETTINGS" | awk '$1 == "HTTPSEnable" { print $3; exit }')"
                SYSTEM_PROXY_HOST="$(printf '%s\n' "$SYSTEM_PROXY_SETTINGS" | awk '$1 == "HTTPSProxy" { print $3; exit }')"
                SYSTEM_PROXY_PORT="$(printf '%s\n' "$SYSTEM_PROXY_SETTINGS" | awk '$1 == "HTTPSPort" { print $3; exit }')"
                case "$SYSTEM_PROXY_PORT" in
                    ''|*[!0-9]*) SYSTEM_PROXY_ENABLED=0 ;;
                esac
                if [ "$SYSTEM_PROXY_ENABLED" = "1" ] && [ -n "$SYSTEM_PROXY_HOST" ]; then
                    case "$SYSTEM_PROXY_HOST" in
                        \[*\]) ;;
                        *:*) SYSTEM_PROXY_HOST="[$SYSTEM_PROXY_HOST]" ;;
                    esac
                    https_proxy="http://$SYSTEM_PROXY_HOST:$SYSTEM_PROXY_PORT"
                    export https_proxy
                    SYSTEM_PROXY_EXCEPTIONS="$(printf '%s\n' "$SYSTEM_PROXY_SETTINGS" | awk '
                        $1 == "ExceptionsList" { exceptions = 1; next }
                        exceptions && $1 == "}" { exceptions = 0 }
                        exceptions && $2 == ":" {
                            host = $3; sub(/^\*\./, ".", host)
                            printf "%s%s", separator, host; separator = ","
                        }')"
                    if [ -n "$SYSTEM_PROXY_EXCEPTIONS" ]; then
                        no_proxy="${no_proxy:-${NO_PROXY:-}}"
                        no_proxy="${no_proxy:+$no_proxy,}$SYSTEM_PROXY_EXCEPTIONS"
                        export no_proxy
                    fi
                    log "using macOS system HTTPS proxy for download"
                fi
            fi
            ;;
    esac
}

fetch_with_curl() (
    maybe_apply_system_proxy "$1"
    curl --http1.1 -fsSL --connect-timeout 15 --max-time 300 --speed-time 30 --speed-limit 1024 \
        --retry 2 --retry-delay 1 "$1" -o "$2"
)

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
                fetch_with_curl "$1" "$2" || fail "download failed: $1 (check connectivity to GitHub and retry)"
            elif command -v wget >/dev/null 2>&1; then
                wget -q --timeout=60 --tries=3 "$1" -O "$2" || fail "download failed: $1 (check connectivity to GitHub and retry)"
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

# --- harness integration (global persona + hooks) --------------------------
INTEGRATION_OK=0
if [ "${STATEROOT_SKIP_INTEGRATION:-}" = "1" ]; then
    log "CLI-only installation; run stateroot install when ready to configure harness integrations"
else
    log "configuring harness integrations (global persona, hooks, MCP)"
    if "$DEST_DIR/stateroot" install; then
        INTEGRATION_OK=1
        log "harness integration complete"
    else
        log "ERROR: CLI installed, but harness integration failed — retry: stateroot install"
    fi
fi

# --- PATH ------------------------------------------------------------------
# The install must leave stateroot runnable, not just present: when
# $DEST_DIR is missing from PATH, append a managed export line to the user's
# shell profiles (rustup-style, marker-gated, idempotent).
case ":$PATH:" in
    *":$DEST_DIR:"*) ;;
    *)
        PATH_LINE='export PATH="$HOME/.local/bin:$PATH"'
        MARKER='# stateroot PATH'
        # A clean Mac may have no dotfiles yet. zsh reads .zshrc for both
        # login and non-login interactive terminals; respect ZDOTDIR too.
        case "${SHELL:-/bin/sh}" in
            */zsh) PROFILE="${ZDOTDIR:-$HOME}/.zshrc" ;;
            */bash) PROFILE="$HOME/.bashrc" ;;
            *) PROFILE="$HOME/.profile" ;;
        esac
        if ! grep -qsF "$MARKER" "$PROFILE" 2>/dev/null; then
            if mkdir -p "$(dirname "$PROFILE")" && printf '\n%s\n%s\n' "$MARKER" "$PATH_LINE" >> "$PROFILE"; then
                log "added $DEST_DIR to your PATH in $PROFILE — open a new terminal"
            else
                log "could not update $PROFILE; add this line manually:"
                printf '  %s\n' "$PATH_LINE"
            fi
        else
            log "PATH for $DEST_DIR already configured in $PROFILE (open a new terminal if needed)"
        fi
        ;;
esac

# --- anonymous install ping ------------------------------------------------
# One GET with os + version + channel, counted in the stateroot.dev web
# server logs. Never blocks (3s cap), never fails the install.
# Disable with: STATEROOT_NO_PING=1
if [ "${STATEROOT_NO_PING:-}" != "1" ]; then
    VER="$("$DEST_DIR/stateroot" --version 2>/dev/null | awk '{print $NF}')"
    VIA="${STATEROOT_INSTALL_VIA:-script}"
    PING_URL="https://stateroot.dev/api/install-ping?os=$TARGET&v=${VER:-unknown}&via=$VIA"
    if command -v curl >/dev/null 2>&1; then
        ( maybe_apply_system_proxy "$PING_URL"; curl -fsSL --max-time 3 "$PING_URL" -o /dev/null ) 2>/dev/null || true
    elif command -v wget >/dev/null 2>&1; then
        wget -q -T 3 -O /dev/null "$PING_URL" 2>/dev/null || true
    fi
fi

cat <<'EOF'

Quickstart:
  1. cd your-project && stateroot init
  2. work in any harness (Claude, Codex, Cursor, Kimi, OpenClaw, Hermes)
EOF
if [ "${STATEROOT_SKIP_INTEGRATION:-}" = "1" ]; then
    printf '%s\n' '  3. run stateroot install when ready to configure global persona + hooks'
elif [ "$INTEGRATION_OK" = "1" ]; then
    printf '%s\n' '  3. persona + hooks are already configured globally from this install'
else
    printf '%s\n' '  3. setup incomplete — run stateroot install to retry agent integration'
    exit 1
fi
