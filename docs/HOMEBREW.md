# Homebrew

The formula lives in a separate tap repository, `frosty-universe/homebrew-usb-android-tethering`, so users install it with the fully qualified name:

```sh
brew install frosty-universe/usb-android-tethering/usb-android-tethering
sudo usb-android-tethering install
```

## One-time setup (maintainers)

Homebrew requires a tap repository whose name starts with `homebrew-`. Create it once:

```sh
brew tap-new frosty-universe/usb-android-tethering --no-git
```

`brew tap-new` creates the repository at `$(brew --repository)/Library/Taps/frosty-universe/homebrew-usb-android-tethering`. Push that directory to GitHub as a new public repository with the same name, then generate the first formula from the current tag:

```sh
TAP_DIR=/path/to/homebrew-usb-android-tethering scripts/brew/update-tap.sh v0.1.0
```

Commit and push the generated `Formula/usb-android-tethering.rb`.

## Automatic updates on every release

`.github/workflows/release.yml` runs `scripts/brew/update-tap.sh` after it publishes a release. It needs a repository secret named `TAP_TOKEN`:

1. Create a fine-grained personal access token with **Contents: Read and write** on the tap repository only.
2. Add it as `TAP_TOKEN` in the main repository: Settings → Secrets and variables → Actions → New repository secret.

Without the secret the step is skipped and the tap stays at its last version. With it, every tag `vX.Y.Z` updates the formula, so users get the new version with `brew update && brew upgrade`.

## Checking a formula by hand

```sh
TAP_DIR=/path/to/tap scripts/brew/update-tap.sh --check v0.1.0   # verify only, no writes
ruby -c /path/to/tap/Formula/usb-android-tethering.rb
```

`brew audit --strict --new --online <formula>` is the official check, but it can take several minutes and needs network access.

## Why a tap and not homebrew-core

homebrew-core expects a stable, widely used project with its own release cadence and a maintainer who keeps the formula current. A tap has no such bar, updates instantly with each release, and lets users choose `--build-from-source` or the published binary from `install.sh`. If the project grows, submitting to homebrew-core is a later step; the formula here is written to pass `brew audit --strict --new`.

## How the formula works

- `url` points at the immutable source tarball for the tag, with its SHA-256. No prebuilt binary, so the build always matches the user's architecture and Rust toolchain.
- `depends_on "rust" => :build` builds from source; libusb is vendored, so there are no runtime dependencies.
- `service` describes the daemon for `brew services`, with `require_root true` because the network interface needs root. The tool's own `sudo usb-android-tethering install` remains the recommended path, since it also creates the network service entry.
- `test` runs `--version` and `status`, which need neither root nor a phone.
