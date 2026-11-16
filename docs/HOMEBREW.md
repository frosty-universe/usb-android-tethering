# Homebrew

The formula lives in a separate tap repository, `frosty-universe/homebrew-usb-android-tethering`, so users install it with the fully qualified name:

```sh
brew install frosty-universe/usb-android-tethering/usb-android-tethering
sudo usb-android-tethering install
```

The tap is live at [frosty-universe/homebrew-usb-android-tethering](https://github.com/frosty-universe/homebrew-usb-android-tethering) and carries the formula for v0.1.0. `brew audit --strict --new` passes.

## Updating the formula by hand (maintainers)

The release workflow does this on every tag when `TAP_TOKEN` is set; this is the manual path.

```sh
git clone https://github.com/frosty-universe/homebrew-usb-android-tethering
TAP_DIR=$PWD/homebrew-usb-android-tethering scripts/brew/update-tap.sh vX.Y.Z
cd homebrew-usb-android-tethering && git add Formula && git commit -m "usb-android-tethering vX.Y.Z" && git push
```

The script downloads the tag's source tarball, computes its SHA-256, writes the formula to a temp file, checks it parses with `ruby -c`, and only then replaces the live one. `--check vX.Y.Z` re-verifies a formula already in place without writing.

## Automatic updates on every release

`.github/workflows/release.yml` runs `scripts/brew/update-tap.sh` after it publishes a release. It needs a repository secret named `TAP_TOKEN`:

1. Create a fine-grained personal access token with **Contents: Read and write** on the tap repository only.
2. Add it as `TAP_TOKEN` in the main repository: Settings → Secrets and variables → Actions → New repository secret.

The secret is not set yet, so the step logs a warning and skips; update the tap by hand (above) after each release until it is. Once set, every tag `vX.Y.Z` updates the formula, so users get the new version with `brew update && brew upgrade`.

## Checking a formula by hand

```sh
TAP_DIR=/path/to/tap scripts/brew/update-tap.sh --check v0.1.0   # verify only, no writes
ruby -c /path/to/tap/Formula/usb-android-tethering.rb
```

`brew audit --strict --new --online <formula>` is the official check, but it can take several minutes and needs network access.

## Why a tap and not homebrew-core

homebrew-core expects a stable, widely used project with its own release cadence and a maintainer who keeps the formula current. A tap has no such bar and updates with each release. `install.sh` builds the same tagged source when Rust is already installed. If the project grows, submitting to homebrew-core is a later step; the formula here is written to pass `brew audit --strict --new`.

## How the formula works

- `url` points at the immutable source tarball for the tag, with its SHA-256. No prebuilt binary, so the build always matches the user's architecture and Rust toolchain.
- `depends_on "rust" => :build` builds from source; libusb is vendored, so there are no runtime dependencies.
- There is no `service` block: `brew services` cannot run the daemon as root, and the tool owns its launchd job anyway. `sudo usb-android-tethering install` writes both the daemon plist and the macOS network service, and the formula's caveats say so.
- `test` runs `--version` and `--help`, which need neither root nor a phone.
