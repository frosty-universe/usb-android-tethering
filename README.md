<div align="center">

# 📱➜💻 usb-android-tethering

### Android USB tethering on macOS, the way it should have always worked.

**Plug in your phone, turn on USB tethering, and your Mac is online.**
No kernel extension. No security downgrade. It works with your VPN.

A free, open-source **HoRNDIS alternative** for **macOS 27 Golden Gate, 26 Tahoe, 15 Sequoia and 14 Sonoma** on every **Apple silicon** Mac and on the Intel Macs those releases still support.
It speaks RNDIS, CDC-NCM and CDC-ECM, so Samsung Galaxy, Google Pixel, Xiaomi, OnePlus, Motorola and other Android phones work.

**[🌐 Website](https://frosty-universe.github.io/usb-android-tethering/)** · **[📥 Download](https://github.com/frosty-universe/usb-android-tethering/releases/latest)** · **[📖 Guide](#step-by-step)** · **[🍎 Compatibility](#mac-compatibility)** · **[❓ FAQ](#faq)**

[![CI](https://github.com/frosty-universe/usb-android-tethering/actions/workflows/ci.yml/badge.svg)](https://github.com/frosty-universe/usb-android-tethering/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/frosty-universe/usb-android-tethering?sort=semver)](https://github.com/frosty-universe/usb-android-tethering/releases/latest)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
![macOS: Apple Silicon + Intel](https://img.shields.io/badge/macOS-Apple%20Silicon%20%2B%20Intel-black?logo=apple)
![Written in Rust](https://img.shields.io/badge/written%20in-Rust-orange?logo=rust)
[![GitHub stars](https://img.shields.io/github/stars/frosty-universe/usb-android-tethering?style=social)](https://github.com/frosty-universe/usb-android-tethering/stargazers)

<a href="https://star-history.com/#frosty-universe/usb-android-tethering&Date"><img alt="Star history" src="https://api.star-history.com/svg?repos=frosty-universe/usb-android-tethering&type=Date" height="160"></a>

</div>

## ⚡ Install in one command

```sh
curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh
```

This downloads the latest universal binary (Apple Silicon and Intel), **verifies its SHA-256 checksum**, and installs a background service. You'll be asked for your password once. Then [enable USB tethering on your phone](#step-2) and you're online.

<sub>Prefer to read the script first? It's short: <a href="install.sh"><code>install.sh</code></a>. Or <a href="#build-from-source">build from source</a>.</sub>

### With Homebrew

```sh
brew install frosty-universe/usb-android-tethering/usb-android-tethering
sudo usb-android-tethering install
```

The formula builds from the tagged source with your Rust toolchain, so `brew upgrade` picks up every new release. It is a tap formula, not in homebrew-core, so the fully qualified name is needed the first time.

---

## 🤔 Why this exists

Most Android phones tether over **RNDIS**, a Microsoft protocol that **macOS has never supported**. The classic fix, the HoRNDIS kernel extension, no longer loads on current macOS, and on Apple Silicon any kext means lowering your Mac's boot security.

Tools that work around this with a *tunnel* interface (`utun`) create a new problem: **VPN apps ignore tunnel interfaces** when choosing the network to run over. You end up with tethering *or* your VPN, not both.

**usb-android-tethering** gives macOS what it expects: a genuine **Ethernet adapter**, backed by a real network service.

## ✨ Highlights

- 🔌 **Works like a physical Ethernet adapter.** macOS's own Network framework reports it as `wiredEthernet`, just like a USB Ethernet dongle ([verified](#physical-adapter)). It appears in *System Settings → Network* as **"USB Android Tethering"**.
- 🛡️ **VPN-compatible.** Your VPN runs over it like over any wired network. Only IPv4 is bridged, so IPv6 can't leak around the VPN.
- 🤖 **Universal Android support.**
  - **RNDIS**, used by most phones, with matching that follows Linux's reference driver.
  - **CDC-NCM and CDC-ECM** (Pixel 6+, newer phones) whenever macOS's own driver doesn't pick the phone up.
  - Detection is by USB interface class, not a model list, so any brand works.
- 🔁 **Set and forget.** It starts at boot and connects the moment you plug in. It reconnects after unplugging, sleep or a phone reboot, and recovers by itself from crashes.
- 🪶 **Light.**
  - Asynchronous USB I/O with multi-packet aggregation.
  - A kernel packet filter.
  - No polling while idle.
  - About 7 MB of memory.
- 🔒 **Safe by design.**
  - No kernel code; your Mac's security settings stay as they are.
  - Everything is removed on uninstall.
  - The installer verifies checksums.
  - [Security model](SECURITY.md).
- 🧰 **Scriptable.** `status` and `devices` support `--json`, logs are leveled and rotated, and exit codes are stable.

```console
$ usb-android-tethering status
Background service: installed, running (pid 96614)
Tether:             online for 3s (pid 96614)
Interface:          feth7070 (primary network)
Address:            192.168.211.47 via 192.168.211.44  DNS 192.168.211.44
Primary network:    feth7070
Phone:              04e8:6863 "SAMSUNG_Android" RNDIS (bus 0 addr 2, ctrl if 0, data if 1)
```

## 🆚 How it compares

| | Kernel extension (HoRNDIS) | Tunnel-based tools (`utun`) | **usb-android-tethering** |
|---|:---:|:---:|:---:|
| Runs on current macOS and Apple Silicon | ❌ | ✅ | ✅ |
| No boot-security downgrade | ❌ | ✅ | ✅ |
| Seen by macOS as wired Ethernet | ✅ | ❌ | ✅ |
| Listed in System Settings → Network | ✅ | ❌ | ✅ |
| VPN clients can run over it | ✅ | ⚠️ often ignored | ✅ |
| RNDIS **and** CDC-NCM/ECM phones | RNDIS only | varies | ✅ |
| Auto-connect, crash recovery | ❌ | varies | ✅ |

---

<a id="step-by-step"></a>
## 📖 Step-by-step guide: Android USB tethering on a Mac

### Step 1 — Install

```sh
curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh
```

You need a Mac running macOS 11 or later (Apple silicon or Intel, see [Mac compatibility](#mac-compatibility)) and your password once. The service then starts automatically at every boot.

<a id="step-2"></a>
### Step 2 — Turn on USB tethering on the phone

1. Connect the phone to the Mac with a USB **data** cable. Charge-only cables don't work.
2. Unlock the phone and open USB tethering:

| Phone | Where to find it |
|---|---|
| Samsung | Settings → Connections → Mobile Hotspot and Tethering → **USB tethering** |
| Google Pixel | Settings → Network & internet → Hotspot & tethering → **USB tethering** |
| Any other Android | Settings → search for **"USB tethering"** |

> 💡 **Fully automatic option:** turn on *USB debugging* (Developer options) and install `adb` (`brew install android-platform-tools`). The tool then switches USB tethering on by itself whenever you plug the phone in. Tap **Allow** once on the phone's prompt.

### Step 3 — You're online

Within a few seconds macOS switches to the phone. Check it:

```sh
usb-android-tethering status
```

You'll see "USB Android Tethering" as connected in *System Settings → Network*. Unplug the phone and your Mac falls back to Wi-Fi automatically.

### Step 4 — Use your VPN as usual

Connect your VPN after the phone is online, and it runs over the tether exactly as it would over Ethernet. If a VPN was already connected before you plugged in, reconnect it once.

### Step 5 — Update or uninstall

```sh
# Update to the latest release (same command as installing)
curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh

# Uninstall completely (service, network entry, binary, logs)
curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh -s -- --uninstall
```

---

<a id="physical-adapter"></a>
## 🔌 Just like a physical adapter

The phone shows up to macOS the way a USB Ethernet dongle does, at every layer apps and VPN clients look at:

| Layer | What macOS sees |
|---|---|
| Network framework (`NWPathMonitor`, used by apps and VPN clients) | `feth7070: wiredEthernet`, first on the path. Not "expensive", not "constrained". |
| System Settings → Network | A service named **"USB Android Tethering"**, Hardware *Ethernet*, configured by DHCP |
| System configuration | Ranked by service order like any adapter: primary while connected, falls back to Wi-Fi |
| Routing and DNS | Installed by macOS itself (its own DHCP client, scoped routes, resolver) |
| Kernel | A real Ethernet interface (a `feth` pair) with its own MAC and ARP, not a layer-3 tunnel |

Under the hood the tool is a userspace USB driver bridging the phone to that interface. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

<a id="mac-compatibility"></a>
## 🍎 Mac compatibility

The tool needs a Mac that runs macOS 11 or later, and it follows macOS itself: if macOS runs on your Mac, so does this.

**macOS versions**

| macOS | Latest | Macs | Notes |
|---|---|---|---|
| **27 Golden Gate** | 27.2.1 | Apple silicon only | Tested on 27.2. First Apple-silicon-only release. |
| **26 Tahoe** | 26.7.3 | Apple silicon + 4 Intel models | Last release for Intel Macs |
| **15 Sequoia** | 15.8.3 | Apple silicon + Intel | Supported |
| **14 Sonoma** | 14.8.9 | Apple silicon + Intel | Supported; no longer receiving regular updates from Apple |

**Apple silicon** — every Apple silicon Mac: M1, M2, M3, M4, M5, M6 (and their Pro/Max/Ultra variants), plus the A18 Pro MacBook Neo. Nothing in the code depends on the CPU family, so there is no chip to check.

**Intel Macs** work on macOS 26 Tahoe and earlier: MacBook Pro 16″ (2019), MacBook Pro 13″ (2020, four Thunderbolt ports), iMac (2020), Mac Pro (2019). They cannot run macOS 27.

Version numbers from [Apple's security releases page](https://support.apple.com/en-us/100100); model lists from [macOS 27 compatibility](https://support.apple.com/en-us/127255).

The universal binary runs natively on both architectures. There is no Apple-silicon-only build and no Rosetta requirement.

## 📱 Device support

Detection is by USB interface class, not a model list, so a phone doesn't have to be on a list to work.

| Phone | Handled by | Status |
|---|---|---|
| Android with **RNDIS**: Samsung, Xiaomi, OnePlus, OPPO/realme, vivo, Huawei/Honor, Motorola, Sony, LG, Nokia/HMD, ASUS, ZTE, Lenovo, Fairphone and others | **This tool** | Verified end to end on Samsung (Galaxy J7, Android) |
| Android with **CDC-NCM** (Pixel 6 and later, newer phones) | macOS's built-in driver if it claims the phone, otherwise **this tool** | Native path works on macOS; the userspace fallback is unit-tested only |
| Devices with **CDC-ECM** | macOS's built-in driver if it claims it, otherwise **this tool** | Unit-tested only |
| iPhone / iPad | macOS Personal Hotspot | Built into macOS; `devices` identifies it |

The **NCM/ECM userspace fallback has not been tested on hardware**, because no phone that macOS declines to claim was available. If yours is one, [open a device report](https://github.com/frosty-universe/usb-android-tethering/issues/new/choose) with the output of `usb-android-tethering devices --json` and the connection log.

`usb-android-tethering devices` lists what's attached and what to do about each device, including phones whose tethering is switched off and phones macOS already handles.

## 🧭 Commands

| Command | Root | Description |
|---|:---:|---|
| `status [--json]` | | Service state, address, traffic, primary network |
| `devices [--json]` | | Attached phones, whether each can tether, and advice |
| `probe` | | Test the phone link (USB + DHCP) without changing anything |
| `run` *(default)* | ✓ | Run in the foreground (Ctrl+C restores everything) |
| `install` / `uninstall [--purge]` | ✓ | Install or remove the background service |
| `cleanup` | ✓ | Remove leftovers after a crash (normally automatic) |

Options: `-v`/`-vv` (debug/trace), `-q` (quiet), `--log-file PATH`, `--once`, `--no-primary` (keep Wi-Fi preferred), `--no-adb`.
Exit codes: `0` success · `1` failure · `2` usage error · `130` forced stop.

## 🧾 What it changes on your Mac

| Item | Removed by |
|---|---|
| `/usr/local/bin/usb-android-tethering` | `uninstall` |
| `/Library/LaunchDaemons/com.usb-android-tethering.plist` | `uninstall` |
| Network service "USB Android Tethering" | `uninstall` |
| Interfaces `feth7070`/`feth7071` (only while connected) | Automatically |
| `/var/log/usb-android-tethering.log` | `uninstall --purge` |

Nothing else is touched: no kernel extensions, no changes to other network services, DNS or firewall.

## 🩺 Troubleshooting

| Symptom | Fix |
|---|---|
| `devices` lists nothing | Use a data cable and try another port. Unlock the phone. |
| "USB tethering is off" | Turn it on (Step 2), or use the automatic option |
| `Access denied` claiming the interface | Another tethering driver (HoRNDIS, DroidTether, …) holds the phone. Remove it. |
| Online, but no internet | Check the phone has mobile data. `status` should show `feth7070` as primary. |
| VPN still uses Wi-Fi | Reconnect the VPN once the phone is online |
| Anything else | `sudo usb-android-tethering -vv` shows everything, or read `/var/log/usb-android-tethering.log` |

<a id="faq"></a>
## ❓ FAQ

<details>
<summary><b>Is it safe? It asks for my password.</b></summary>

Creating a network interface and a system network service needs administrator rights, like any network driver. The tool contains no kernel code, opens no network ports, and runs its optional `adb` helper as your user, never as root. The installer refuses to run anything whose checksum doesn't match the release. The full model is in [SECURITY.md](SECURITY.md).
</details>

<details>
<summary><b>Does HoRNDIS work on Apple Silicon (M1/M2/M3/M4) or macOS Sequoia/Tahoe?</b></summary>

No. HoRNDIS is a kernel extension. It no longer loads on current macOS, and on Apple Silicon any third-party kernel extension requires lowering the Mac's boot security. usb-android-tethering is a userspace replacement: same result, no kext.
</details>

<details>
<summary><b>Why doesn't my Mac recognise my Android phone's USB tethering?</b></summary>

Most Android phones tether over RNDIS, which macOS has never supported, so nothing happens when you enable USB tethering. This tool adds that support. Phones using CDC-NCM (Pixel 6 and later) sometimes work natively; when macOS doesn't pick them up, this tool handles them too.
</details>

<details>
<summary><b>Why not a kernel extension?</b></summary>

On Apple Silicon, third-party kexts require "Reduced Security" boot mode and a reboot. DriverKit needs entitlements Apple grants case by case. A userspace driver avoids both, can't panic the kernel, and is far faster than any cellular connection needs. [More detail](docs/ARCHITECTURE.md#why-userspace).
</details>

<details>
<summary><b>Does it drain the battery or keep the Mac awake?</b></summary>

No. While idle it waits on events and uses 0 % CPU, and it never prevents sleep.
</details>

<details>
<summary><b>Can I keep Wi-Fi as my main network?</b></summary>

Yes. Install with `--no-primary` and the phone becomes a fallback that's used only when Wi-Fi is unavailable.
</details>

<details>
<summary><b>Why IPv4 only?</b></summary>

So IPv6 can't bypass a VPN that only tunnels IPv4. Almost every service is dual-stack. IPv6-only destinations need a VPN that carries IPv6.
</details>

<a id="build-from-source"></a>
## 🛠️ Build from source

```sh
git clone https://github.com/frosty-universe/usb-android-tethering.git
cd usb-android-tethering
cargo build --release          # Rust 1.88+; libusb is vendored, no other dependencies
sudo ./target/release/usb-android-tethering install
```

## 📚 Documentation

| | |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Data path, protocols, threading, invariants, design decisions |
| [docs/PERFORMANCE.md](docs/PERFORMANCE.md) | Methodology, measurements, reproducible benchmark |
| [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) | Phones, protocols, macOS versions, VPN clients |
| [docs/HOMEBREW.md](docs/HOMEBREW.md) | Installing and upgrading with Homebrew |
| [docs/FAQ.md](docs/FAQ.md) | Common problems and fixes |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Development setup, hardware testing, standards |
| [SECURITY.md](SECURITY.md) | Security model and private vulnerability reporting |
| [CHANGELOG.md](CHANGELOG.md) | Release history |

## 🤝 Contributing

Contributions are very welcome, and **device reports** from phones other than Samsung are especially valuable. See [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md).

## 🙏 Acknowledgements

- [HoRNDIS](https://github.com/jwise/HoRNDIS), the original macOS RNDIS driver and the inspiration for a generation of tethering tools.
- Linux's [`rndis_host`](https://github.com/torvalds/linux/blob/master/drivers/net/usb/rndis_host.c) and CDC-NCM drivers, the reference for protocol behaviour.
- [libusb](https://libusb.info) and [rusb](https://github.com/a1ien/rusb).

Android is a trademark of Google LLC; macOS is a trademark of Apple Inc. This project is not affiliated with Google, Apple or any phone manufacturer.

<a id="license"></a>
## 📄 License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

---

<div align="center">

**If this saved you from hunting for a USB Ethernet dongle, please ⭐ star the repo.** It helps other Mac users find it.

</div>
