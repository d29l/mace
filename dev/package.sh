#!/bin/sh
# Build Mace and pack it as a release download: OUT/mace-x86_64-linux.tar.gz,
# a mace-VERSION folder with the program, the in-game panel's library, Mace's
# files and install.sh, and OUT/mace-x86_64-linux.tar.gz.sha256. The release
# workflow (.github/workflows/release.yml) runs it for every v* tag.
#
# Every Mace already installed updates itself from what this makes, so keep
# it as it is: the two file names, one folder holding `mace`,
# `libmace_panel.so` and `data/`, and `mace --version` printing
# "mace VERSION". An older copy can't install a release that differs.
#
#   dev/package.sh [OUT]     (default: dist)
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
out=$(mkdir -p "${1:-dist}" && cd "${1:-dist}" && pwd)
cd "$here"
cargo build --release --locked
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
name=mace-$version
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/$name"
cp target/release/mace target/release/libmace_panel.so "$stage/$name/"
cp -r data install.sh uninstall.sh README.md LICENSE "$stage/$name/"
tar -C "$stage" --owner=0 --group=0 --numeric-owner -czf "$out/mace-x86_64-linux.tar.gz" "$name"
(cd "$out" && sha256sum mace-x86_64-linux.tar.gz > mace-x86_64-linux.tar.gz.sha256)
echo "$out/mace-x86_64-linux.tar.gz: $name"
