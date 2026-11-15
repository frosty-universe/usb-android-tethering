# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Dependencies refreshed to the latest releases compatible with the Rust 1.88 minimum: `libc` 0.2.191, `log` 0.4.35, `serde_json` 1.0.154, plus transitive updates.
- README: explicit Mac compatibility section for macOS 27 Golden Gate, 26 Tahoe, 15 Sequoia and 14 Sonoma; every Apple silicon generation (M1 through M6, A18 Pro MacBook Neo) and the Intel models still supported; star-history chart.
- Comments: decorative section banners removed and long notes cut to one or two lines. No code changes.

### Added

- `.env.example`, `CLAUDE.md.example`, `AGENTS.md.example`. The real `.env`, `CLAUDE.md` and `AGENTS.md` are gitignored.

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
- **One-command installer** (`install.sh`). It downloads the latest universal (Apple Silicon + Intel) release, verifies its SHA-256 checksum, and installs the service. `--uninstall` removes everything.
- **Automated releases.** A version tag builds, tests and publishes a universal binary with SHA256SUMS.
- **Documentation:** README with a step-by-step guide, architecture, performance methodology, contributing guide, security policy.
- **Continuous integration** on Apple Silicon and Intel runners and on the minimum supported Rust version (1.88).

[Unreleased]: https://github.com/frosty-universe/usb-android-tethering/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/frosty-universe/usb-android-tethering/releases/tag/v0.1.0
