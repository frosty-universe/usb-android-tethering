#!/bin/sh
# usb-android-tethering installer.
#
#   Install:    curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh
#   Uninstall:  curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh -s -- --uninstall
#
# It downloads the latest release (universal binary: Apple Silicon + Intel), verifies its
# SHA-256 checksum against the release's SHA256SUMS, and runs `sudo usb-android-tethering
# install`, which sets up the background service. Nothing is installed if any step fails.
#
# Environment: UAT_VERSION=v0.1.0 installs a specific release instead of the latest.
set -eu

REPO="frosty-universe/usb-android-tethering"
ASSET="usb-android-tethering-macos-universal.tar.gz"
BIN="/usr/local/bin/usb-android-tethering"

color() { [ -t "$1" ] && [ -z "${NO_COLOR:-}" ]; }
say()  { if color 1; then printf '\033[1;32m==>\033[0m %s\n' "$*"; else printf '==> %s\n' "$*"; fi; }
fail() { if color 2; then printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; else printf 'error: %s\n' "$*" >&2; fi; exit 1; }
fetch() { curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 -o "$1" "$2"; }

[ "$(uname -s)" = "Darwin" ] || fail "this tool is for macOS only"
for tool in curl shasum tar sudo awk; do
  command -v "$tool" >/dev/null 2>&1 || fail "'$tool' is required but was not found"
done

case "${1:-}" in
  "")
    ;;
  --uninstall)
    [ -x "$BIN" ] || fail "usb-android-tethering is not installed"
    say "Removing usb-android-tethering (you may be asked for your password)…"
    sudo "$BIN" uninstall --purge || fail "uninstall failed; run: sudo $BIN uninstall --purge"
    say "Uninstalled. Your network settings are back to how they were."
    exit 0
    ;;
  -h|--help)
    printf 'usage: install.sh [--uninstall]\n\nInstalls usb-android-tethering as a background service.\nUAT_VERSION=v0.1.0 installs a specific release instead of the latest.\n'
    exit 0
    ;;
  *)
    fail "unknown option: $1 (try --help)"
    ;;
esac

case "${UAT_VERSION:-}" in
  *[!0-9A-Za-z.+-]*) fail "UAT_VERSION must look like v0.1.0 (got '$UAT_VERSION')" ;;
esac

if [ -n "${UAT_VERSION:-}" ]; then
  base="https://github.com/$REPO/releases/download/$UAT_VERSION"
else
  base="https://github.com/$REPO/releases/latest/download"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

say "Downloading usb-android-tethering ${UAT_VERSION:-(latest)}…"
fetch "$tmp/$ASSET" "$base/$ASSET" || fail "download failed: $base/$ASSET"
fetch "$tmp/SHA256SUMS" "$base/SHA256SUMS" || fail "checksum download failed: $base/SHA256SUMS"

say "Verifying checksum…"
# Match the asset name exactly (awk, not grep: the name is not a regex), accepting a
# "path/to/asset" second field as well as the plain name.
want=$(awk -v a="$ASSET" '{ n = $2; sub(/^.*\//, "", n); if (n == a) print $1 }' "$tmp/SHA256SUMS") \
  || fail "cannot read $tmp/SHA256SUMS"
[ -n "$want" ] || fail "SHA256SUMS has no line for $ASSET"
got=$(shasum -a 256 "$tmp/$ASSET" | cut -d' ' -f1)
[ "$got" = "$want" ] || fail "checksum mismatch for $ASSET — refusing to install
  expected: $want
  actual:   $got"

tar -xzf "$tmp/$ASSET" -C "$tmp" usb-android-tethering \
  || fail "could not extract usb-android-tethering from $ASSET"
chmod 755 "$tmp/usb-android-tethering"
gotver=$("$tmp/usb-android-tethering" --version 2>&1) || fail "downloaded binary does not run on this Mac"
if [ -n "${UAT_VERSION:-}" ]; then
  case $gotver in
    *" ${UAT_VERSION#v}"*) ;;
    *) fail "downloaded binary reports '$gotver', expected $UAT_VERSION" ;;
  esac
fi

say "Installing the background service (you may be asked for your password)…"
sudo "$tmp/usb-android-tethering" install \
  || fail "install failed; nothing else was changed (run: sudo $BIN install)"

cat <<'EOF'

  ✔ Installed. Next:
    1. Connect your Android phone with a USB data cable.
    2. On the phone, turn on USB tethering
       (Settings → search for "USB tethering").
    3. Check the connection:   usb-android-tethering status

  Uninstall any time:
    curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh -s -- --uninstall

EOF
