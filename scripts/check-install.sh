#!/bin/sh
# Confirms a release tag's source archive downloads and that Cargo.toml matches the tag.
# No prebuilt binary is published. Nothing is installed and nothing outside the temp dir is touched.
#
#   scripts/check-install.sh              check the latest release
#   scripts/check-install.sh vX.Y.Z       check a specific tag
set -eu

REPO="frosty-universe/usb-android-tethering"
version="${1:-${UAT_VERSION:-}}"

say()  { printf '==> %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
fetch() { curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 -o "$1" "$2"; }

[ "$(uname -s)" = "Darwin" ] || fail "this tool is for macOS only"
for tool in curl tar sed; do
  command -v "$tool" >/dev/null 2>&1 || fail "'$tool' is required but was not found"
done

case "$version" in
  "") ;;
  v[0-9]*) ;;
  *) fail "version must look like vX.Y.Z (got '$version')" ;;
esac

if [ -z "$version" ]; then
  say "Finding the latest release"
  release_json=$(curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 \
    "https://api.github.com/repos/$REPO/releases/latest") \
    || fail "could not read the latest release"
  version=$(printf '%s\n' "$release_json" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
fi
case $version in
  v[0-9]*) ;;
  *) fail "could not determine a release tag (got '${version:-}')" ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

url="https://github.com/$REPO/archive/refs/tags/$version.tar.gz"
say "Downloading $url"
fetch "$tmp/src.tar.gz" "$url" || fail "download failed: $url"
tar -xzf "$tmp/src.tar.gz" -C "$tmp" || fail "could not extract the source archive"
dir="$tmp/usb-android-tethering-${version#v}"
[ -f "$dir/Cargo.toml" ] || fail "source archive has no Cargo.toml"
got=$(sed -n 's/^version = "\(.*\)"/\1/p' "$dir/Cargo.toml" | head -1)
[ -n "$got" ] || fail "Cargo.toml has no version"
[ "v$got" = "$version" ] || fail "Cargo.toml says $got, tag is $version"
[ -f "$dir/src/main.rs" ] || fail "source archive has no src/main.rs"

say "source archive OK: $version (Cargo.toml $got)"
say "PASS ($version)"
