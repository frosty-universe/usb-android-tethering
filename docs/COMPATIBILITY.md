# Compatibility

`usb-android-tethering` is a userspace program. It uses only facilities that every supported macOS release provides: `feth` interfaces, BPF devices, `scutil` preferences, launchd and libusb. It contains no kernel extension and no architecture-specific code beyond libusb's own.

The short version: **if macOS runs on your Mac, this tool runs on your Mac.** It follows macOS, not a fixed hardware list.

## macOS versions

The binary's deployment target is macOS 11.0 (visible as `minos 11.0` in its `LC_BUILD_VERSION` load command), so it loads on macOS 11 and later. The releases below are the ones the project documents and tests against.

| macOS | Macs | Status |
|---|---|---|
| **27 Golden Gate** | Apple silicon only | Tested on 27.2. First Apple-silicon-only release. |
| **26 Tahoe** | Apple silicon and four Intel models | Last release for Intel Macs. |
| **15 Sequoia** | Apple silicon and Intel | Supported. |
| **14 Sonoma** | Apple silicon and Intel | Supported. Apple no longer ships regular updates for it. |
| **11 Big Sur to 13 Ventura** | Apple silicon and Intel | Expected to work (the binary's deployment target is 11.0), but not part of the documented test matrix. |

Nothing in the code branches on the macOS version. The features it depends on (`feth`, BPF, SystemConfiguration preferences, `launchctl bootstrap`) have existed for far longer than macOS 11.

## Apple silicon

Every Apple silicon Mac runs every macOS release from its introduction onward, and this tool runs on all of them. Nothing in the code depends on the CPU family: it uses `feth`, BPF, `scutil` and libusb, which are the same on an M-series Mac as on the A-series MacBook Neo. There is no chip to look up.

Intel Macs work on macOS 26 Tahoe and earlier. They cannot run macOS 27.

## Intel Macs

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
