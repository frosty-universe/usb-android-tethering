# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1] - 2026-11-20

### Changed

- Releases remain source-only. `install.sh` and Homebrew build the tagged source.
- Root daemon installation uses `/Library/PrivilegedHelperTools`, independent of Homebrew's prefix.
- Release builds use abort-on-panic for a smaller optimized daemon.

### Fixed

- Hardened BPF, RNDIS, DHCP, ECM framing, service-order validation and shutdown paths.
- Corrected full-MTU CDC-ECM transmit handling.
- CI validates every shell script independently.

## [0.2.0] - 2026-11-16

### Changed

- Dependencies refreshed to current releases compatible with the Rust 1.88 minimum: `libc` 0.2.191, `log` 0.4.35, `serde_json` 1.0.154, plus transitive updates.
- README: concise macOS compatibility and install guidance for Apple silicon and supported Intel Macs.
- Comments: decorative section banners removed and long notes cut to one or two lines. No code changes.

### Added

- **Homebrew tap, live.** `brew install frosty-universe/usb-android-tethering/usb-android-tethering` builds the tagged source from [the published tap](https://github.com/frosty-universe/homebrew-usb-android-tethering); `brew audit --strict --new` passes. The release workflow updates the tap on every version tag when a `TAP_TOKEN` secret is set, and docs/HOMEBREW.md documents the manual path. See docs/HOMEBREW.md.
- **Installer improvements.** `install.sh` now takes `--help` and `--uninstall`, rejects unknown options, builds a selected `UAT_VERSION=vX.Y.Z` from source, and prints actionable failures.
- `scripts/check-install.sh`, a no-root check that downloads a release source archive and verifies its tag metadata.
- `.env.example`. The real `.env`, `CLAUDE.md` and `AGENTS.md` are gitignored; `AGENTS.md` points at `CLAUDE.md`, which `scripts/merge-claude-md.sh` maintains from `scripts/claude-antislop.md` behind a hash guard that never overwrites local edits.
- docs/COMPATIBILITY.md and docs/FAQ.md.
- Compatibility pages cite Apple's published latest releases (27.2.1, 26.7.3, 15.8.3, 14.8.9) with source links, keep the four Intel models that reach macOS 26, and separate hardware verification from unit and smoke tests.

### Fixed

- A failed USB setup could leave an interface claimed until the handle was dropped; both interfaces are now released on every error path.
- `RxRing::start` leaked its transfers if a later submit failed; the ring is now dropped, which cancels and frees them.
- Hardened the engine and protocol paths against panics from device-controlled input, and added tests for state, logging, guard, NCM/RNDIS packing, scutil parsing and BPF record parsing (80 tests).

## [0.1.0] - 2026-09-27

First public release.

### Added

- **Userspace USB tethering driver** for Android on macOS (Apple Silicon and Intel). No kernel extension, no SIP changes.
- **Universal protocol support.**
  - RNDIS, with Linux `rndis_host` interface matching (including the `EF/01/01` variant), packet-filter fallbacks and destination-MAC fixup.
  - CDC-NCM (NTB16) and CDC-ECM, for phones macOS's native driver doesn't claim, never detaching a native driver.
  - The data interface is found through the CDC Union descriptor.
  - A stable host MAC per phone.
- **Native network integration.** The phone appears as a real Ethernet interface (`feth` pair) backed by the network service "USB Android Tethering". macOS runs DHCP, routing and DNS itself, and ranks it by service order.
- **VPN compatibility.** It uses a real Ethernet interface rather than a `utun` tunnel, and bridges IPv4 and ARP only so IPv6 can't bypass a VPN.
- **High-throughput, low-overhead data path.**
  - Asynchronous libusb I/O: 8 queued receive transfers and 8 pipelined transmit transfers.
  - RNDIS multi-packet aggregation within the phone's announced limits.
  - A kernel BPF filter.
  - Allocation-free transmit packing.
  - Event-driven threads with no idle polling.
- **Background service** (`install` / `uninstall`) using launchd. It starts at boot, reacts to USB hotplug, and reconnects automatically.
- **Durability.**
  - A crash guard process (kqueue).
  - Cleanup at start-up and a `cleanup` command.
  - Self-healing if the network service is deleted or DHCP fails (the link is brought up only after the data path is wired, so macOS's first DHCP exchange can't be lost).
  - Exponential backoff on repeated failures.
  - Safe reinstall over a running daemon.
- **Diagnostics.** `status` and `devices` (both with `--json`), and `probe` for testing the link without root. Hints for phones with tethering off, CDC-NCM phones and iPhones.
- **Optional automatic tethering switch** through `adb` (USB debugging, Android 9+). It alternates between the `rndis` and `ncm` gadget functions, and adb runs as the logged-in console user, never as root.
- **Logging.** Leveled (`-v`, `-vv`, `-q`), coloured (honours `NO_COLOR`), optional file output with 5 MB rotation.
- **One-command installer** (`install.sh`). It downloads the latest tagged source, builds it for the current Mac, and installs the service. `--uninstall` removes everything.
- **Automated releases.** A version tag tests and publishes release notes without attaching an executable.
- **Documentation:** README with a step-by-step guide, architecture, performance methodology, contributing guide, security policy.
- **Continuous integration** on Apple Silicon and Intel runners and on the minimum supported Rust version (1.88).

[Unreleased]: https://github.com/frosty-universe/usb-android-tethering/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/frosty-universe/usb-android-tethering/releases/tag/v0.2.1
[0.2.0]: https://github.com/frosty-universe/usb-android-tethering/releases/tag/v0.2.0
[0.1.0]: https://github.com/frosty-universe/usb-android-tethering/releases/tag/v0.1.0
