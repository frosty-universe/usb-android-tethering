#!/bin/sh
# usb-android-tethering installer.
#
#   Install:    curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh
#   Uninstall:  curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh -s -- --uninstall
#
# No prebuilt binary is published. This downloads the tagged source archive, builds it
# with cargo (Rust 1.88+), and runs `sudo usb-android-tethering install`.
# Nothing is installed if any step fails.
#
# Environment: UAT_VERSION=vX.Y.Z builds that tag instead of the latest release.
# Homebrew does the same build: brew install frosty-universe/usb-android-tethering/usb-android-tethering
set -eu

REPO="frosty-universe/usb-android-tethering"
BIN="/Library/PrivilegedHelperTools/usb-android-tethering"

color() { [ -t "$1" ] && [ -z "${NO_COLOR:-}" ]; }
say()  { if color 1; then printf '\033[1;32m==>\033[0m %s\n' "$*"; else printf '==> %s\n' "$*"; fi; }
fail() { if color 2; then printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; else printf 'error: %s\n' "$*" >&2; fi; exit 1; }
fetch() { curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 -o "$1" "$2"; }

[ "$(uname -s)" = "Darwin" ] || fail "this tool is for macOS only"
for tool in curl tar sudo awk sed; do
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
    printf 'usage: install.sh [--uninstall]\n\nBuilds usb-android-tethering from the tagged source and installs the background service.\nRequires Rust 1.88+ (cargo). No prebuilt binary is downloaded.\nUAT_VERSION=vX.Y.Z builds that tag instead of the latest release.\n\nHomebrew: brew install frosty-universe/usb-android-tethering/usb-android-tethering\n'
    exit 0
    ;;
  *)
    fail "unknown option: $1 (try --help)"
    ;;
esac

case "${UAT_VERSION:-}" in
  "") ;;
  v[0-9]*) ;;
  *) fail "UAT_VERSION must look like vX.Y.Z (got '$UAT_VERSION')" ;;
esac
case "${UAT_VERSION:-}" in
  *[!0-9A-Za-z.+-]*) fail "UAT_VERSION must look like vX.Y.Z (got '$UAT_VERSION')" ;;
esac

if [ -n "${UAT_VERSION:-}" ]; then
  tag=$UAT_VERSION
else
  say "Finding the latest release…"
  release_json=$(curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 \
    "https://api.github.com/repos/$REPO/releases/latest") \
    || fail "could not read the latest release"
  tag=$(printf '%s\n' "$release_json" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
fi
case $tag in
  v[0-9]*) ;;
  *) fail "could not determine a release tag (got '${tag:-}')" ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

say "Downloading source $tag…"
fetch "$tmp/src.tar.gz" "https://github.com/$REPO/archive/refs/tags/$tag.tar.gz" \
  || fail "download failed: https://github.com/$REPO/archive/refs/tags/$tag.tar.gz"
tar -xzf "$tmp/src.tar.gz" -C "$tmp" \
  || fail "could not extract the source archive"
dir="$tmp/usb-android-tethering-${tag#v}"
[ -f "$dir/Cargo.toml" ] || fail "source archive has no Cargo.toml"

command -v cargo >/dev/null 2>&1 || fail "cargo was not found. Install Rust 1.88+ (https://rustup.rs), then run this again.
  Or install with Homebrew, which builds the same source:
    brew install frosty-universe/usb-android-tethering/usb-android-tethering
    sudo usb-android-tethering install"

say "Building $tag from source (this can take a few minutes)…"
( cd "$dir" && cargo build --locked --release ) \
  || fail "build failed; nothing was installed"
built="$dir/target/release/usb-android-tethering"
[ -x "$built" ] || fail "build did not produce usb-android-tethering"
gotver=$("$built" --version 2>&1) || fail "built binary does not run on this Mac"
case $gotver in
  *" ${tag#v}"*) ;;
  *) fail "built binary reports '$gotver', expected $tag" ;;
esac

say "Installing the background service (you may be asked for your password)…"
sudo "$built" install \
  || fail "service install failed; the binary at $BIN may have been updated"

cat <<'EOF'

  ✔ Installed. Next:
    1. Connect your Android phone with a USB data cable.
    2. On the phone, turn on USB tethering
       (Settings → search for "USB tethering").
    3. Check the connection:   usb-android-tethering status

  Uninstall any time:
    curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh -s -- --uninstall

EOF
