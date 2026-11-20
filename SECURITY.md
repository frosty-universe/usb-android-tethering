# Security Policy

## Supported versions

Security fixes are made on the latest release and on `main`.

| Version | Supported |
|---|---|
| 0.2.x | ✓ |

## Reporting a vulnerability

**Please don't open a public issue for security problems.**

Report privately through GitHub's [private vulnerability reporting](https://github.com/frosty-universe/usb-android-tethering/security/advisories/new) (the repository's *Security* tab → *Report a vulnerability*). Please include:

- the affected version or commit, and the macOS version;
- a description of the issue and its impact;
- steps to reproduce, or a proof of concept if you have one.

You can expect an acknowledgement within **5 days** and a first assessment within **14 days**. Fixes are released as soon as practical. We credit reporters in the release notes unless you'd rather stay anonymous.

## Security model

`usb-android-tethering` is a network driver, and its background service runs as **root**. The design keeps that privilege as narrow as possible.

### Trust boundaries

| Input | Trust | Handling |
|---|---|---|
| USB data from the phone (RNDIS messages, NCM transfer blocks, CDC descriptors, serial number) | **Untrusted** | All parsing is bounds-checked, never panics on malformed input, and bounds pointer chains such as NCM's NDP list (covered by unit tests). The serial number is only passed to `adb` as a separate argument, never through a shell. |
| Frames from macOS on the virtual NIC | Trusted (local kernel) | Filtered in the kernel to IPv4 and ARP |
| Command-line arguments | Local user | Parsed by clap. Commands that change the system require root. |
| Values read from macOS and pasted into `scutil` scripts (network location path, interface name) | Semi-trusted (system configuration) | Validated against a fixed character set and refused outright if they don't match, rather than escaped. The service name is quoted, with `"` and `\` escaped. `scutil` failures are detected from its own output because it exits 0 on some errors. |
| `adb` binary (optional) | **User-writable location** | Never executed as root. It runs as the logged-in console user, only if owned by root or that user and not group- or world-writable. |

### What runs as root, and why

| Action | Why root is needed |
|---|---|
| Creating the `feth` interfaces and opening `/dev/bpf*` | Kernel network configuration |
| Editing the network service (`scutil --prefs`) | System network preferences |
| Installing the launchd daemon and binary | System locations |

The daemon opens **no network sockets** and listens on nothing. It exchanges frames only with the phone (USB) and the local kernel (BPF).

### Installer

`install.sh` downloads the tagged source archive, builds it with Cargo, runs the result once with `--version` to prove it matches the tag and executes on this Mac, and only then calls `sudo install`. Any failed step aborts before anything is installed. No prebuilt binary is published. The source archive is not signed; building it locally does not protect against a compromised tag.

### Files

| Path | Owner / mode | Notes |
|---|---|---|
| `/Library/PrivilegedHelperTools/usb-android-tethering` | `root:wheel 0755` | Set explicitly before the binary is renamed into place. The directory is a root-owned system location, never a Homebrew prefix. |
| `/Library/LaunchDaemons/com.usb-android-tethering.plist` | root, `0644` (process umask) | Written by the installer running as root. |
| `/var/run/usb-android-tethering.json` | root, `0644` (explicit) | Status only (addresses, counters). No secrets. Replaced atomically. |
| `/var/log/usb-android-tethering.log` | root, `0644` (process umask) | Contains no traffic contents. Rotated to `.log.1` at 5 MB. |

The plist and log are ordinary files created by a root process with the default umask. The status file sets `0644` explicitly. A restrictive umask makes the implicit modes more restrictive, never less.

### Network exposure

- Only IPv4 and ARP cross the bridge, so IPv6 can't bypass an IPv4 VPN.
- The phone routes your traffic, like any hotspot. Treat the phone and the mobile carrier as you would any network operator, and use a VPN if you don't trust them.

## Hardening checklist for contributors

- No `unsafe` without a comment explaining why it's sound.
- Code that parses device data must bounds-check and must not panic.
- Never pass device-controlled strings to a shell.
- Never execute a binary from a user-writable location as root.
- Keep root-owned files non-writable by other users.
