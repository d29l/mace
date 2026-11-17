#!/bin/sh
# Install Mace for this user: build it, put `mace` in ~/.local/bin and its files
# (the in-game panel, the icon) in ~/.local/share/mace/app, add the menu
# entries, and, if asked, the join notifications and in-game panel services.
#
#   ./install.sh [--toasts] [--overlay] [--dev] [--no-links]
#
#   --toasts    "Joined <game> · Server in <place>" notifications for every account
#   --overlay   the in-game panel (needs Quickshell)
#   --dev       link to this folder's build instead of copying it (for working on Mace)
#   --no-links  don't make roblox.com's Play button open Mace
set -eu
here=$(cd "$(dirname "$0")" && pwd)
toasts= overlay= dev= links=1
for arg in "$@"; do
    case $arg in
        --toasts) toasts=1 ;;
        --overlay) overlay=1 ;;
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

(cd "$here" && cargo build --release --locked)
mkdir -p "$bin"
if [ -n "$dev" ]; then
    ln -sf "$here/target/release/mace" "$bin/mace"
    rm -rf "$app"  # this folder's own files are used
    files=$here
else
    install -m 755 "$here/target/release/mace" "$bin/mace"
    rm -rf "$app.new"
    mkdir -p "$app.new"
    cp -r "$here/overlay" "$here/data" "$app.new/"
    rm -rf "$app"
    mv "$app.new" "$app"
    files=$app
fi
echo "installed: $bin/mace"
case ":$PATH:" in *":$bin:"*) ;; *) echo "note: $bin is not on your PATH" >&2 ;; esac

"$bin/mace" migrate
[ -n "$links" ] && PATH="$bin:$PATH" "$bin/mace" integrate

units=$config/systemd/user
if [ -n "$toasts" ]; then
    mkdir -p "$units"
    cp "$here/data/systemd/mace-toasts.service" "$units/"
    systemctl --user daemon-reload
    systemctl --user enable --now mace-toasts.service
    systemctl --user restart mace-toasts.service
    echo "join notifications: on (mace-toasts.service)"
fi
if [ -n "$overlay" ]; then
    command -v qs >/dev/null || fail "the in-game panel needs Quickshell (qs)"
    mkdir -p "$units"
    sed "s|@OVERLAY@|$files/overlay|; s|@MACE_BIN@|$bin/mace|" "$here/data/systemd/mace-overlay.service" \
        > "$units/mace-overlay.service"
    systemctl --user daemon-reload
    systemctl --user enable --now mace-overlay.service
    systemctl --user restart mace-overlay.service
    echo "in-game panel: on (mace-overlay.service)"
    echo "  on Hyprland it opens with Super+Tab; elsewhere bind a key to:"
    echo "  qs -p $files/overlay ipc call overlay toggle"
fi
