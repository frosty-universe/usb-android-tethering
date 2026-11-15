#!/bin/sh
# Verifies a published release without installing anything: downloads the release asset to a
# temp dir, checks it against SHA256SUMS, confirms it is universal, and runs `--version`
# and `--help`.
#
#   scripts/check-install.sh              check the latest release
#   scripts/check-install.sh v0.1.0       check a specific tag
#
# No root needed; nothing outside the temp dir is touched. Exit 0 means the release is good.
set -eu

REPO="frosty-universe/usb-android-tethering"
ASSET="usb-android-tethering-macos-universal.tar.gz"
version="${1:-${UAT_VERSION:-}}"

say()  { printf '==> %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
fetch() { curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 -o "$1" "$2"; }

[ "$(uname -s)" = "Darwin" ] || fail "this tool is for macOS only"
for tool in curl shasum tar awk; do
  command -v "$tool" >/dev/null 2>&1 || fail "'$tool' is required but was not found"
done

if [ -n "$version" ]; then
  base="https://github.com/$REPO/releases/download/$version"
else
  base="https://github.com/$REPO/releases/latest/download"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

say "Downloading $base/$ASSET"
fetch "$tmp/$ASSET" "$base/$ASSET" \
  || fail "download failed: $base/$ASSET"
fetch "$tmp/SHA256SUMS" "$base/SHA256SUMS" \
  || fail "checksum download failed: $base/SHA256SUMS"

# Match the asset name exactly (awk, not grep: the name is not a regex), accepting a
# "path/to/asset" second field as well as the plain name.
want=$(awk -v a="$ASSET" '{ n = $2; sub(/^.*\//, "", n); if (n == a) print $1 }' "$tmp/SHA256SUMS") \
  || fail "cannot read $tmp/SHA256SUMS"
[ -n "$want" ] || fail "SHA256SUMS has no line for $ASSET"
got=$(shasum -a 256 "$tmp/$ASSET" | cut -d' ' -f1)
[ "$got" = "$want" ] || fail "checksum mismatch for $ASSET
  expected: $want
  actual:   $got"

tar -xzf "$tmp/$ASSET" -C "$tmp" usb-android-tethering \
  || fail "could not extract usb-android-tethering from the tarball"
chmod 755 "$tmp/usb-android-tethering"

if command -v lipo >/dev/null 2>&1; then
  archs=$(lipo -archs "$tmp/usb-android-tethering" 2>&1) \
    || fail "lipo cannot read the binary: $archs"
  case $archs in *arm64*) ;; *) fail "binary has no arm64 slice (archs: $archs)" ;; esac
  case $archs in *x86_64*) ;; *) fail "binary has no x86_64 slice (archs: $archs)" ;; esac
  say "universal: $archs"
fi

gotver=$("$tmp/usb-android-tethering" --version 2>&1) \
  || fail "the binary does not run: $gotver"
[ -n "$gotver" ] || fail "the binary printed nothing for --version"
if [ -n "$version" ]; then
  case $gotver in
    *" ${version#v}"*) ;;
    *) fail "binary reports '$gotver', expected $version" ;;
  esac
fi
"$tmp/usb-android-tethering" --help >/dev/null 2>&1 \
  || fail "the binary does not run --help"

say "checksum OK, binary runs: $gotver"
say "PASS (${version:-latest})"
