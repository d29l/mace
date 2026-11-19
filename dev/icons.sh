#!/bin/sh
# Make the in-game panel's icons (layer/icons/NAME.a8) from the SVGs in
# layer/icons/svg: Google's Material Symbols, rounded, filled, weight 400 (made
# to go with Noto Sans, the panel's font). Each file is the icon's coverage,
# one byte a pixel, at 16, 20, 24, 32, 40 and 48 pixels, one after another
# (layer/src/ui.rs reads them in that order). Needs rsvg-convert and magick.
#
#   dev/icons.sh
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
src=$here/layer/icons/svg
out=$here/layer/icons
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for name in refresh link storage games-recent sports_esports group settings close select_window volume_up volume_off; do
    : > "$tmp/$name.a8"
    for size in 16 20 24 32 40 48; do
        rsvg-convert -w "$size" -h "$size" "$src/$name.svg" -o "$tmp/icon.png"
        magick "$tmp/icon.png" -alpha extract -depth 8 gray:- >> "$tmp/$name.a8"
    done
    mv "$tmp/$name.a8" "$out/$name.a8"
done
echo "icons in $out"
