#!/bin/sh
# Release a version of Mace:
#
#   dev/release.sh 0.2.0
#
# Sets the version, runs the tests, commits, tags v0.2.0 and pushes. GitHub
# then builds the download (.github/workflows/release.yml), and installed
# copies offer it within a day.
set -eu
version=${1:?usage: dev/release.sh VERSION (e.g. 0.2.0)}
echo "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { echo "not a version: $version" >&2; exit 2; }
cd "$(dirname "$0")/.."
[ "$(git branch --show-current)" = main ] || { echo "releases come from main" >&2; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "commit or put away your changes first" >&2; exit 1; }
git rev-parse -q --verify "refs/tags/v$version" >/dev/null && { echo "v$version exists already" >&2; exit 1; }
sed -i "0,/^version = \".*\"$/s//version = \"$version\"/" Cargo.toml
cargo update --workspace --quiet
cargo test --workspace --locked --quiet
if ! git diff --quiet; then
    git commit --quiet -am "Mace $version"
fi
git tag "v$version"
git push --quiet origin main "v$version"
echo "v$version pushed; GitHub builds it now: https://github.com/d29l/mace/actions"
