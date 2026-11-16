#!/bin/sh
# Prints the themes for scripts/theme-editor.html as JavaScript: the built-in
# themes of crates/rushy-tui/themes, then the user's own themes. Each file is
# kept as its TOML text; the page parses it.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
user="${XSH_CONFIG_DIR:-$HOME/Library/Application Support/xsh}/themes"

echo 'window.XSH_THEMES = [];'
for f in "$root"/crates/rushy-tui/themes/*.toml "$user"/*.toml; do
    [ -f "$f" ] || continue
    # The text goes into a JavaScript template string, which a backtick ends.
    if grep -q '`' "$f"; then
        echo "theme-presets: skipping $f: it contains a backtick" >&2
        continue
    fi
    printf 'window.XSH_THEMES.push(String.raw`'
    cat "$f"
    printf '`);\n'
done
