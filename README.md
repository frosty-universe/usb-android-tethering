<div align="center">

# 📱➜💻 usb-android-tethering

### Android USB tethering on macOS. Real Ethernet. VPN-friendly.

**Plug in your phone. Turn on USB tethering. Your Mac is online.**

A free, open-source **HoRNDIS alternative** for macOS 11+, supporting **RNDIS, CDC-NCM and CDC-ECM** on Apple silicon and Intel Macs.

[Website](https://frosty-universe.github.io/usb-android-tethering/) · [Install](#install) · [Guide](#connect) · [Compatibility](#compatibility) · [Wiki](https://github.com/frosty-universe/usb-android-tethering/wiki)

[![CI](https://github.com/frosty-universe/usb-android-tethering/actions/workflows/ci.yml/badge.svg)](https://github.com/frosty-universe/usb-android-tethering/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/frosty-universe/usb-android-tethering?sort=semver)](https://github.com/frosty-universe/usb-android-tethering/releases/latest)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust](https://img.shields.io/badge/written%20in-Rust-orange?logo=rust)](https://www.rust-lang.org/)

</div>

## Why this exists

macOS does not provide an RNDIS host driver. HoRNDIS uses a kernel extension that no longer fits current macOS and Apple silicon.

Tunnel-based alternatives expose `utun` interfaces that VPN clients may ignore. This project runs the driver in userspace and presents the phone as a normal wired Ethernet service:

- No kernel extension or boot-security change
- macOS-managed DHCP, routes and DNS
- VPN-compatible `feth` Ethernet interface
- IPv4 and ARP only, so IPv6 cannot bypass an IPv4-only VPN
- Automatic reconnect through `launchd`

## Install

### Homebrew (recommended)

```sh
brew install frosty-universe/usb-android-tethering/usb-android-tethering
sudo usb-android-tethering install
```

The tap builds the tagged source locally. `brew upgrade` updates it. `brew services` is not used because the daemon owns a root launchd job.

### Source installer

Requires Rust 1.88+:

```sh
curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh
```

The script builds the tagged source locally. Releases attach no prebuilt executable.

Uninstall either installation:

```sh
sudo usb-android-tethering uninstall --purge
```

## Connect

1. Connect the phone with a USB data cable.
2. Enable **USB tethering** in Android settings.
3. Check the link:

```sh
usb-android-tethering status
```

The service appears as **USB Android Tethering** in **System Settings → Network**. macOS falls back to Wi-Fi when the phone is unplugged.

Optional automatic tethering requires USB debugging and `adb`:

```sh
brew install android-platform-tools
```

## Commands

| Command | Root | Purpose |
|---|:---:|---|
| `status [--json]` | No | Service state, address, traffic and primary network |
| `devices [--json]` | No | Attached phones and protocol advice |
| `probe` | No | Test USB and DHCP without changing system configuration |
| `run` | Yes | Run in the foreground |
| `install` | Yes | Install the launchd daemon and network service |
| `uninstall --purge` | Yes | Remove the daemon, network service, binary and log |
| `cleanup` | Yes | Remove leftovers after a crash |

Options: `-v`/`-vv`, `-q`, `--log-file PATH`, `--once`, `--no-primary`, `--no-adb`.

## Compatibility

- macOS 11 and later
- Apple silicon and supported Intel Macs
- Android USB network functions detected by interface class, not a model list
- RNDIS: verified end to end with Samsung
- CDC-NCM and CDC-ECM: native macOS path works when macOS claims the device; the userspace fallback is unit-tested and needs hardware reports

See [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) for the tested macOS releases and device details.

## Against TetherKit

[TetherKit](https://github.com/XiaoMiku01/TetherKit) is a respected userspace RNDIS driver using `feth`, BPF and libusb. The projects differ mainly in system integration:

| | TetherKit | **usb-android-tethering** |
|---|---|---|
| Protocols | RNDIS | RNDIS, CDC-NCM, CDC-ECM |
| Network service | Temporary DHCP setup | Persistent SystemConfiguration service |
| VPN routing | Manual route changes | macOS ranks it as wired Ethernet |
| Background operation | Helper or app | `launchd`, reconnects on replug |
| Interface | SwiftUI app and CLI | CLI |
| macOS floor | 13.3 CLI / 14 app | 11 |
| Distribution | Source build, arm64 releases | Homebrew or source build, no executable release asset |

No same-phone, same-link throughput comparison has been published. This project makes no speed claim.

## Development

```sh
git clone https://github.com/frosty-universe/usb-android-tethering.git
cd usb-android-tethering
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

Hardware testing requires a tether-capable Android phone. See [CONTRIBUTING.md](CONTRIBUTING.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and the [project wiki](https://github.com/frosty-universe/usb-android-tethering/wiki).

## Security

The daemon requires root only for network interfaces, BPF and system network preferences. It opens no listening sockets, contains no kernel code, and runs optional `adb` as the logged-in user.

Read [SECURITY.md](SECURITY.md) before installing. Report vulnerabilities privately using the instructions there.

## License

Licensed under either [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option. Not affiliated with Apple, Google or any phone manufacturer.
