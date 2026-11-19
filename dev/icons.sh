#!/bin/sh
# Make the in-game panel's icons (layer/icons/NAME.a8) from GNOME's Adwaita
# icon theme, the icons Mace Settings shows. Each file is the icon's coverage,
# one byte a pixel, at 16, 20, 24, 32, 40 and 48 pixels, one after another
# (layer/src/ui.rs reads them in that order). Needs rsvg-convert and magick.
#
# Adwaita's icons are the GNOME project's, under CC-BY-SA-3.0 OR LGPL-3.0-only.
#
#   dev/icons.sh [ADWAITA]     (default /usr/share/icons/Adwaita)
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
src=${1:-/usr/share/icons/Adwaita}/symbolic
out=$here/layer/icons
mkdir -p "$out"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for name in view-refresh edit-copy network-server input-gaming system-users emblem-system \
            window-close focus-windows audio-volume-high audio-volume-muted; do
    svg=$(find "$src" -name "$name-symbolic.svg" | head -n 1)
    [ -n "$svg" ] || { echo "no $name-symbolic in $src" >&2; exit 1; }
    : > "$tmp/$name.a8"
    for size in 16 20 24 32 40 48; do
        rsvg-convert -w "$size" -h "$size" "$svg" -o "$tmp/icon.png"
        magick "$tmp/icon.png" -alpha extract -depth 8 gray:- >> "$tmp/$name.a8"
    done
    mv "$tmp/$name.a8" "$out/$name.a8"
done
echo "icons in $out"
