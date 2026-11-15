# Compatibility

`usb-android-tethering` is a userspace program. It uses only facilities that every supported macOS release provides: `feth` interfaces, BPF devices, `scutil` preferences, launchd and libusb. It contains no kernel extension and no architecture-specific code beyond libusb's own.

The short version: **if macOS runs on your Mac, this tool runs on your Mac.** It follows macOS, not a fixed hardware list.

## macOS versions

The binary's deployment target is macOS 11.0 (visible as `minos 11.0` in its `LC_BUILD_VERSION` load command), so it loads on macOS 11 and later. The releases below are the ones the project documents and tests against.

| macOS | Latest release at time of writing | Macs | Status |
|---|---|---|---|
| **27 Golden Gate** | 27.2.1 | Apple silicon only | Tested. First Apple-silicon-only release. |
| **26 Tahoe** | 26.7.3 | Apple silicon and four Intel models | Tested. Last release for Intel Macs. |
| **15 Sequoia** | 15.8.3 | Apple silicon and Intel | Supported. |
| **14 Sonoma** | 14.8.9 | Apple silicon and Intel | Supported. Apple no longer ships regular updates for it. |
| **11 Big Sur to 13 Ventura** | (final releases) | Apple silicon and Intel | Expected to work (the binary's deployment target is 11.0), but not part of the documented test matrix. |

Nothing in the code branches on the macOS version. The features it depends on (`feth`, BPF, SystemConfiguration preferences, `launchctl bootstrap`) have existed for far longer than macOS 11.

## Apple silicon

Every Apple silicon Mac runs every macOS release from its introduction onward, and this tool runs on all of them.

| Chip | Macs |
|---|---|
| **M1** | MacBook Air (2020), MacBook Pro 13-inch (2020), Mac mini (2020), iMac 24-inch (2021), MacBook Pro 14/16-inch (2021) |
| **M2 / M2 Pro / M2 Max / M2 Ultra** | MacBook Air 13/15-inch (2022–23), MacBook Pro 13-inch (2022), MacBook Pro 14/16-inch (2023), Mac mini (2023), Mac Studio (2023), Mac Pro (2023) |
| **M3 / M3 Pro / M3 Max / M3 Ultra** | MacBook Pro 14/16-inch (2023), iMac 24-inch (2023), MacBook Air 13/15-inch (2024), Mac Studio (2025) |
| **M4 / M4 Pro / M4 Max** | MacBook Pro 14/16-inch (2024), iMac 24-inch (2024), Mac mini (2024), MacBook Air 13/15-inch (2025), Mac Studio (2025) |
| **M5 / M5 Pro / M5 Max / M5 Ultra** | MacBook Pro 14-inch (2025), MacBook Pro 14/16-inch (2026), MacBook Air 13/15-inch (2026), Mac mini (2026), Mac Studio (2026) |
| **M6** | MacBook Pro 14-inch (2026), iMac 24-inch (2026), Mac mini (2026) |
| **A18 Pro** | MacBook Neo (2026) |

The A18 Pro MacBook Neo runs macOS on an iPhone-class SoC. Nothing in this tool depends on the CPU family, so it is supported the same way as the M-series Macs.

## Intel Macs

Intel Macs work on macOS 26 Tahoe and earlier. They cannot run macOS 27.

| Model | Notes |
|---|---|
| MacBook Pro 16-inch (2019) | Supported through macOS 26 |
| MacBook Pro 13-inch (2020, four Thunderbolt 3 ports) | Supported through macOS 26 |
| iMac 27-inch (2020) | Supported through macOS 26 |
| Mac Pro (2019) | Supported through macOS 26 |

The release archive contains a **universal binary** (arm64 and x86_64, combined with `lipo`). It runs natively on both architectures: there is no separate Intel build and no Rosetta requirement.

## What the binary needs

- macOS 11 or later (deployment target `minos 11.0`).
- Root for `run`, `install`, `uninstall` and `cleanup`. `status`, `devices` and `probe` need no root.
- A free `/dev/bpfN` device (the first 256 are tried).
- The `feth7070` and `feth7071` interface names to be free. They are fixed so leftovers are recognisable.
- For the optional automatic tethering switch: USB debugging on the phone, Android 9 or later, and a trusted `adb` binary.

## Phones and protocols

Detection is by USB interface class, not a model list. A phone does not have to be listed to work.

| Phone / protocol | Handled by | Status |
|---|---|---|
| Android with **RNDIS** (Samsung, Xiaomi, OnePlus, OPPO/realme, vivo, Huawei/Honor, Motorola, Sony, LG, Nokia/HMD, ASUS, ZTE, Lenovo, Fairphone and others) | **This tool**, always | Verified end to end on Samsung. macOS has no RNDIS driver, so there is no alternative. |
| Android with **CDC-NCM** (Pixel 6 and later, newer phones) | macOS's built-in driver if it claims the phone, otherwise **this tool** | The native path works on macOS. The userspace fallback is unit-tested only. |
| Devices with **CDC-ECM** | macOS's built-in driver if it claims it, otherwise **this tool** | Unit-tested only. |
| iPhone / iPad | macOS Personal Hotspot | Built into macOS. `devices` identifies it. |

The NCM/ECM userspace fallback has not been tested on hardware, because no phone that macOS declines to claim was available. If yours is one, open a device report with `usb-android-tethering devices --json` and the connection log.

## USB and network requirements

- **USB 2.0 High-Speed or better.** A USB 1.1 port works but is far slower than the phone's connection. Charge-only cables carry no data and will not work.
- **A data cable** that exposes the phone's USB network function. Some cheap cables expose only power.
- **IPv4.** The bridge carries IPv4 and ARP only. IPv6 is not available over the tether, by design.

## Not compatible

- **iOS devices as tethering targets.** macOS handles Personal Hotspot natively; this tool does not drive it.
- **Phones whose tethering interface is claimed by another driver** (an old HoRNDIS or DroidTether install, for example). Remove the other driver; this tool will not detach it.
- **Windows, Linux or BSD.** The program uses macOS-only interfaces (`feth`, BPF, `scutil`, launchd).
