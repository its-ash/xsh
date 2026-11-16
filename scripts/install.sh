#!/bin/sh
# Installs target/xsh.app (from `make build`) and the `xsh` command.
#
# Usage: scripts/install.sh [install|uninstall]
#   APP_DIR  where the app goes          (default: /Applications)
#   BIN_DIR  where the `xsh` link goes   (default: /usr/local/bin)
#
# The command is a symbolic link into the app, so app updates also update the
# command. sudo is used only when BIN_DIR is not writable. Settings, history,
# and bundles live in your user directories and are never touched.
set -eu

ACTION="${1:-install}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/target/xsh.app"
APP_DIR="${APP_DIR:-/Applications}"
BIN_DIR="${BIN_DIR:-/usr/local/bin}"
APP="$APP_DIR/xsh.app"
BIN="$BIN_DIR/xsh"
TARGET="$APP/Contents/MacOS/xsh"
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

say() { printf '%s\n' "$*"; }
fail() { printf 'install: %s\n' "$*" >&2; exit 1; }

# Runs a command, with sudo when DIR is not writable by this user.
as_needed() {
    dir="$1"
    shift
    if [ -w "$dir" ]; then
        "$@"
    else
        say "  ($dir needs administrator rights: running with sudo)"
        sudo "$@"
    fi
}

# Whether BIN is ours: a link into an xsh.app, or a program that says it is xsh.
bin_is_xsh() {
    if [ -L "$BIN" ]; then
        case "$(readlink "$BIN")" in
            */xsh.app/Contents/MacOS/xsh) return 0 ;;
        esac
    fi
    [ -x "$BIN" ] && "$BIN" --version 2>/dev/null | grep -q '^xsh '
}

install_app() {
    [ -d "$SRC" ] || fail "$SRC is missing; run \`make build\` first"
    mkdir -p "$APP_DIR" 2>/dev/null || true
    [ -d "$APP_DIR" ] || fail "$APP_DIR does not exist"
    if pgrep -xq XshApp; then
        say "note: xsh.app is running; quit and reopen it to use the new version"
    fi
    say "installing the app to $APP"
    # Copy next to the old app first, then swap, so a failed copy leaves the
    # installed app as it was. ditto keeps the code signature intact.
    NEW="$APP_DIR/.xsh.app.new.$$"
    as_needed "$APP_DIR" rm -rf "$NEW"
    as_needed "$APP_DIR" ditto "$SRC" "$NEW"
    as_needed "$APP_DIR" rm -rf "$APP"
    as_needed "$APP_DIR" mv "$NEW" "$APP"
    # Let Finder and "Make xsh the Default" see the new copy.
    [ -x "$LSREGISTER" ] && "$LSREGISTER" -f "$APP" >/dev/null 2>&1 || true
}

# Refuses to replace another program, before anything changes.
check_bin() {
    if [ -e "$BIN" ] || [ -L "$BIN" ]; then
        bin_is_xsh || fail "$BIN exists and is not xsh; move it away or set BIN_DIR"
    fi
}

install_bin() {
    say "linking $BIN -> $TARGET"
    if [ ! -d "$BIN_DIR" ]; then
        as_needed "$(dirname "$BIN_DIR")" mkdir -p "$BIN_DIR"
    fi
    as_needed "$BIN_DIR" ln -sfn "$TARGET" "$BIN"
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) say "note: $BIN_DIR is not on your PATH; add it to use \`xsh\` in other shells" ;;
    esac
}

uninstall() {
    if [ -L "$BIN" ] && [ "$(readlink "$BIN")" = "$TARGET" ]; then
        say "removing $BIN"
        as_needed "$BIN_DIR" rm -f "$BIN"
    elif [ -e "$BIN" ]; then
        say "leaving $BIN: it is not a link to $APP"
    fi
    if [ -d "$APP" ]; then
        say "removing $APP"
        as_needed "$APP_DIR" rm -rf "$APP"
    fi
    say "done; your settings, history, and bundles are kept"
}

case "$ACTION" in
    install)
        check_bin
        install_app
        install_bin
        say "installed $("$BIN" --version 2>/dev/null || echo xsh)"
        say "open it with: open \"$APP\""
        ;;
    uninstall) uninstall ;;
    *) fail "unknown action \`$ACTION\`; use install or uninstall" ;;
esac
