#!/bin/sh
# Install Mace for this user: build it, put `mace` in ~/.local/bin and its files
# (the in-game panel's library, the icon) in ~/.local/share/mace/app, and add
# the menu entries.
#
#   ./install.sh [--dev] [--no-links]
#
#   --dev       link to this folder's build instead of copying it (for working on Mace)
#   --no-links  don't make roblox.com's Play button open Mace
#
# The in-game panel (and its toasts) needs nothing else: Mace draws it inside
# the games it starts.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
dev= links=1
for arg in "$@"; do
    case $arg in
        --toasts | --overlay) ;; # options of the old panel and notifications: both are in the game now
        --dev) dev=1 ;;
        --no-links) links= ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done
fail() { echo "$*" >&2; exit 1; }

command -v cargo >/dev/null || fail "Mace is built with Rust: install it from rustup.rs or your distribution (e.g. pacman -S rust)"
pkg-config --exists gtk4 'libadwaita-1 >= 1.5' 2>/dev/null ||
    fail "Mace needs GTK 4 and libadwaita 1.5 or newer (e.g. pacman -S gtk4 libadwaita)"

data=${XDG_DATA_HOME:-$HOME/.local/share}
config=${XDG_CONFIG_HOME:-$HOME/.config}
bin=$HOME/.local/bin
app=$data/mace/app

# mace, and the in-game panel's library
(cd "$here" && cargo build --release --locked)
mkdir -p "$bin"
if [ -n "$dev" ]; then
    ln -sf "$here/target/release/mace" "$bin/mace"
    rm -rf "$app"  # this folder's own files are used, and the library beside the build
else
    install -m 755 "$here/target/release/mace" "$bin/mace"
    rm -rf "$app.new"
    mkdir -p "$app.new/lib"
    cp -r "$here/data" "$app.new/"
    install -m 644 "$here/target/release/libmace_panel.so" "$app.new/lib/"
    rm -rf "$app"
    mv "$app.new" "$app"
fi
echo "installed: $bin/mace"
case ":$PATH:" in *":$bin:"*) ;; *) echo "note: $bin is not on your PATH" >&2 ;; esac

"$bin/mace" migrate
[ -n "$links" ] && PATH="$bin:$PATH" "$bin/mace" integrate

# Older installs: join notifications came from their own service, and the panel
# was a Quickshell window whose key Hyprland bound at runtime. All of that is in
# the game now; the services go, and the key is let go so the game receives it.
units=$config/systemd/user
removed=
for unit in mace-toasts.service mace-overlay.service; do
    if [ -f "$units/$unit" ]; then
        systemctl --user disable --now "$unit" 2>/dev/null || true
        rm -f "$units/$unit"
        removed="$removed $unit"
    fi
done
if [ -n "$removed" ]; then
    systemctl --user daemon-reload
    echo "removed old services:$removed"
fi
case $removed in *mace-overlay*)
    if [ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ] && command -v hyprctl >/dev/null; then
        key=$(sed -n 's/.*"overlay_key": *"\([^"]*\)".*/\1/p' "$config/mace/settings.json" 2>/dev/null | head -n 1)
        hyprctl eval "hl.unbind(\"${key:-SUPER + TAB}\")" >/dev/null 2>&1 || true
    fi ;;
esac
echo "in-game panel: Insert by default (changeable in settings)"
