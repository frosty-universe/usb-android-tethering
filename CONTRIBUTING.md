# Contributing

Thanks for your interest in improving `usb-android-tethering`! Bug reports, device test reports, documentation fixes and code are all welcome.

By participating you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Ways to help

- **Test on your phone.** Reports from brands other than Samsung are especially valuable. Open a [device report](https://github.com/frosty-universe/usb-android-tethering/issues/new/choose) with the output of `usb-android-tethering devices --json` and `status --json`.
- **Report bugs** using the bug template. Include `-vv` logs where possible.
- **Improve the docs.** Unclear wording counts as a bug.
- **Send code.** For anything larger than a small fix, please open an issue first so we can agree on the approach.

## Development setup

Requirements: macOS, Rust 1.88 or later (`rustup` recommended), and an Android phone with USB tethering for end-to-end testing. libusb is vendored, so no Homebrew packages are needed.

```sh
git clone https://github.com/frosty-universe/usb-android-tethering.git
cd usb-android-tethering
cargo build
cargo test
```

### Checks that CI runs (run them before pushing)

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

CI runs these on `macos-latest` (Apple Silicon), on `macos-15-intel`, and on the minimum supported Rust version (1.88, from `rust-version` in `Cargo.toml`). `cargo fmt --check` runs only on the stable toolchain. After the build it also runs a smoke test with no root and no phone: `--version`, `--help`, `devices --json` and `status --json`. CI sets `RUSTFLAGS=-D warnings`, so a warning fails the build locally only if you set the same.

`cargo test` needs macOS but no phone and no root: it uses `/usr/bin/plutil` and `/bin/sleep`, and writes only to the temporary directory.

### Testing with a phone

| Level | Command | Root | Changes the system? |
|---|---|:---:|---|
| Detection | `cargo run -- devices` | | No |
| USB and RNDIS handshake, DHCP | `cargo run -- probe -v` | | No |
| Full session, foreground | `sudo ./target/debug/usb-android-tethering -vv` | ✓ | Yes, reversed on Ctrl+C |
| Background service | `sudo ./target/release/usb-android-tethering install` | ✓ | Yes, reversed by `uninstall` |

After testing, confirm nothing is left behind:

```sh
ifconfig -l | tr ' ' '\n' | grep feth || echo "no leftover interfaces"
networksetup -listnetworkserviceorder | grep -c "USB Android Tethering"
```

A service count of `1` is expected only while the background service is installed.

To check performance, run [`scripts/bench.sh`](scripts/bench.sh) before and after your change on the same machine and phone, and include both results in the PR. [docs/PERFORMANCE.md](docs/PERFORMANCE.md) describes the methodology.

## Coding standards

- **Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) first,** especially its *Invariants* section. Changes to teardown order, transfer ownership or the network-service lifecycle need extra care and a test on real hardware.
- **Follow the surrounding style.** Keep modules small and focused, and add a doc comment to every module and public item. Comments explain *why*; the code shows *what*.
- **Errors.** Use `anyhow::Result` with context (`.context("…")`) at the boundaries. Don't `unwrap()` on anything that depends on runtime input.
- **Logging.** `info` is for events a user cares about, `debug` for diagnostics, `trace` for per-packet detail. The data path must not log at `info` or higher per packet.
- **`unsafe`.** Only for FFI (libc, libusb). Every block needs a clear reason it's sound.
- **Performance.** The data path must not allocate per packet or wake up on a timer while idle.
- **No new dependencies** without discussion. Each one adds build time, audit surface and supply-chain risk.
- **Tests.** Add unit tests for protocol parsing and packing, and for any pure logic. Hardware behaviour is verified manually and described in the PR.

## Commits and pull requests

- Keep each PR to one logical change, with a descriptive title.
- Write commit messages in the imperative mood ("Add NCM hint", not "Added NCM hint"), with a short summary line and a body explaining *why* when it isn't obvious.
- Fill in the PR template, including how you tested (hardware, macOS version, phone model).
- Update [CHANGELOG.md](CHANGELOG.md) under **Unreleased** for any user-visible change.
- CI must pass. Maintainers may ask for changes; reviews focus on correctness, safety, then clarity.

## Releasing (maintainers)

1. Update the version in `Cargo.toml` and move the **Unreleased** entries in `CHANGELOG.md` under the new version and date.
2. Run the full check list above, plus an end-to-end test on hardware (install, connect, unplug and replug, `kill -9` recovery, uninstall).
3. Push a tag `vX.Y.Z`. The *Release* workflow checks it matches `Cargo.toml`, runs the tests, and publishes GitHub release notes. It does not upload a binary. When `TAP_TOKEN` is set, it updates the Homebrew formula to that tag.
4. Verify the source archive with `scripts/check-install.sh vX.Y.Z`, then install with Homebrew or `UAT_VERSION=vX.Y.Z` and `install.sh`. Both build that tag.

## License

By contributing, you agree that your contributions are dual-licensed under the MIT and Apache-2.0 licenses, as described in the [README](README.md#license), without any additional terms or conditions.
