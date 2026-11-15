# Architecture

This document describes how `usb-android-tethering` works internally, the invariants the code relies on, and the reasoning behind its main design decisions. It's written for contributors and reviewers. For usage, see the [README](../README.md); for user questions see the [FAQ](FAQ.md), and for macOS and Mac coverage see [COMPATIBILITY](COMPATIBILITY.md).

- [Overview](#overview)
- [Data path](#data-path)
- [Threads and processes](#threads-and-processes)
- [Session lifecycle](#session-lifecycle)
- [Invariants](#invariants)
- [Supported protocols](#supported-protocols)
- [Protocol notes (RNDIS)](#protocol-notes-rndis)
- [Protocol notes (CDC-NCM and CDC-ECM)](#protocol-notes-cdc-ncm-and-cdc-ecm)
- [macOS integration](#macos-integration)
- [Design decisions](#design-decisions)
- [Known limitations](#known-limitations)
- [Source map](#source-map)

## Overview

```
 ┌──────────┐ USB bulk, RNDIS/NCM/ECM ┌───────────────────────────────┐  BPF   ┌──────────┐ feth ┌──────────┐
 │ Android  │ ◀─────────────────────▶ │ usb-android-tethering (user)  │ ◀────▶ │ feth7071 │ ◀──▶ │ feth7070 │ ◀─▶ macOS IP stack
 │  phone   │   8 RX / 8 TX async     │  usbnet host · packer/parser  │ filter │  (peer)  │ pair │  (host)  │     routes · DNS
 └──────────┘                         └───────────────────────────────┘        └──────────┘      └──────────┘
                                                                                         network service
                                                                                      "USB Android Tethering"
```

The program is a USB network *host* in userspace. It speaks the phone's tethering protocol (RNDIS, CDC-NCM or CDC-ECM, behind one `usbnet::Framing`) over libusb and exchanges raw Ethernet frames with macOS through a `feth` (fake Ethernet) pair. The host side of the pair, `feth7070`, is registered as an ordinary DHCP Ethernet network service, so **macOS itself** obtains the address, installs routes, configures DNS and decides the primary network.

## Data path

### Phone → Mac (receive)

1. `usbio::RxRing` keeps **8 bulk-IN transfers** permanently queued with libusb, so the phone never waits for a free buffer. The buffer size is the protocol's: 16 KiB for RNDIS (`HOST_MAX_TRANSFER`), the NTB input size for NCM (capped at 16 KiB), and 2 KiB for ECM.
2. On completion, the libusb event thread calls `Link::on_phone_transfer`, which splits the transfer into Ethernet frames with the protocol's parser (`rndis::parse_packets`, `ncm::parse_ntb`, or one frame per transfer for ECM) and handles each frame:
   - frames that aren't IPv4 or ARP are dropped. Unicast frames with a wrong destination MAC are rewritten to ours (a point-to-point link; Linux applies the same fix for ZTE devices);
   - before the NIC exists, the only consumer is the DHCP readiness client, which is handed UDP port 68 payloads and drops everything else;
   - otherwise the frame is written to the BPF device bound to `feth7071`, and macOS receives it on `feth7070`.
3. The transfer is resubmitted immediately. Traffic counters are updated once per completed transfer.

### Mac → phone (transmit)

1. A kernel BPF program (`BIOCSETF`) on `feth7071` passes only IPv4 and ARP. Nothing else is ever copied to userspace.
2. The `nic-tx` thread blocks in `poll()` on the BPF descriptor and a self-pipe, with no timeout. One `read()` returns a batch of frames.
3. `Framing::pack_with` packs as many frames as the phone accepts directly from the BPF buffer into a transfer buffer. One buffer and one frame-index vector are allocated for the lifetime of the thread; packing itself allocates nothing. The limits come from RNDIS `INITIALIZE_CMPLT` or from NCM `GET_NTB_PARAMETERS`. ECM carries one frame per transfer.
4. `usbio::TxPool` submits the transfer asynchronously. Up to **8** can be in flight, and the thread only blocks when all 8 are busy (backpressure).

## Threads and processes

| Thread / process | Blocks on | Role |
|---|---|---|
| main | condition variable (USB change / stop), 5 s tick while online | Device loop, session setup and teardown, supervision, status file |
| `usb-events` | `libusb_handle_events` | Async transfer completions (receive path runs here), hotplug callbacks |
| `nic-tx` | `poll(bpf, self-pipe)` | Transmit path |
| ctrlc handler | signal | Sets the stop flag, wakes waiters |
| `__guard` (separate process) | kqueue `EVFILT_PROC/NOTE_EXIT` | Cleans up if the main process dies uncleanly |

No thread wakes on a timer while the link is idle, apart from the 5 s supervision tick.

## Session lifecycle

```
            ┌──────────── hotplug event / 30 s backstop ───────────┐
            ▼                                                      │
 ┌──────────────────┐ tethering device ┌──────────────┐ DHCP ok ┌─────────────┐ macOS has IP ┌────────┐
 │     Waiting      │─────────────────▶│  Connecting  │────────▶│ NIC + svc   │─────────────▶│ Online │
 │ hints, adb, wait │                  │ protocol init│         │ feth, BPF,  │              │ 5 s    │
 └──────────────────┘                  │ RX ring,DHCP │         │ TX pool     │              │ ticks  │
            ▲                      └──────┬───────┘         └──────┬──────┘              └───┬────┘
            │   backoff 2 s → 60 s        │ error                  │ error                   │ unplug / error / stop
            └─────────────────────────────┴────────────────────────┴─────────────────────────┘
                                             teardown (ordered, see invariants)
```

**Setup order** (in `tether::session`): protocol init (RNDIS handshake, or NCM/ECM setup and data alt-setting) → receive ring → DHCP readiness check → create `feth` pair (link down) → attach NIC to receive path → start transmit thread → ensure network service → **bring link up** → wait for a routable address from macOS.

**Teardown order** (in `tether::session`):

1. `Link::kill`: clear the alive flag, interrupt `nic-tx` through the self-pipe, wake waiters.
2. Join `nic-tx`. Dropping the `TxPool` cancels and drains all transmit transfers.
3. Drop the `RxRing`, which cancels and drains all receive transfers.
4. Remove the network service (foreground mode only).
5. Drop the `Link`: release the USB interfaces, close the handle, destroy the `feth` pair.

## Invariants

1. **A transfer is never freed while libusb owns it.** `RxRing` and `TxPool` count in-flight transfers, and their `Drop` cancels everything and waits for every callback. If that doesn't finish within 3 s, the memory is intentionally leaked rather than freed.
2. **Transfers are drained before the device handle closes.** This is guaranteed by the teardown order above.
3. **Callbacks don't panic.** All parsing uses bounds-checked slicing and early returns. A panic across the libusb FFI boundary would abort the process; launchd and the crash guard would still recover.
4. **Fixed identifiers make leftovers recognisable.**
   - Interfaces are always `feth7070`/`feth7071`.
   - The service ID is always `0A5B7E7E-7E7E-4A4A-8D8D-0000000A7070`.
   - Start-up, `cleanup` and the guard remove exactly these, and nothing else.
5. **Who owns the network service.**
   - Foreground mode creates it per session and removes it on exit.
   - Background (installed) mode keeps it permanently, the way macOS keeps a USB Ethernet adapter's service, and only `uninstall` removes it.
   - `cleanup_leftovers` never removes an installed service.
6. **"Running" means PID and executable name.** A status file whose PID was reused by an unrelated process is ignored.
7. **Only IPv4 and ARP cross the bridge,** in both directions. Receive filters in userspace; transmit filters in the kernel.
8. **The link goes up last.** `feth7070` is created *down* and brought up only after both data directions are wired and the network service exists. macOS starts DHCP the instant the link comes up, and a reply arriving before the receive path is attached would be lost. That would leave macOS on a self-assigned `169.254.x.x` address with a long retry backoff.
9. **Link-local is not "online".** A `169.254/16` address means DHCP failed. Setup waits for a real address, and restarts DHCP by bouncing the link after 10 s (40 s budget). Supervision does the same if the address is later lost.
10. **Never take a phone from a native driver.** NCM/ECM phones are left alone for a 4 s grace period after they appear, and are used only if macOS's driver hasn't claimed them (`kernel_driver_active`). Their interfaces are never detached. Only RNDIS, which no macOS driver implements, may detach an accidentally matched class driver.

## Supported protocols

| Protocol | Control interface | Data framing | When this tool drives it |
|---|---|---|---|
| RNDIS | `E0/01/03`, `02/02/FF`, `EF/04/01`, `EF/01/01` (Linux `rndis_host` classes) | `REMOTE_NDIS_PACKET_MSG`, several per transfer | Always: macOS has no RNDIS driver |
| CDC-NCM | `02/0D/00` | NTB16: NTH16 + datagrams + NDP16 | Only if macOS's native driver didn't claim the phone |
| CDC-ECM | `02/06/00` | One Ethernet frame per transfer | Only if macOS's native driver didn't claim the phone |

In all cases the data interface is the one named by the CDC Union functional descriptor, falling back to the interface after the control interface. The host MAC is the one the phone reports (RNDIS), or a stable locally administered address derived from the phone's vendor, product and serial number, so the phone's DHCP server hands out the same address every time.

## Protocol notes (RNDIS)

| Item | Detail |
|---|---|
| Control channel | `SEND_ENCAPSULATED_COMMAND` (`0x21/0x00`) and `GET_ENCAPSULATED_RESPONSE` (`0xA1/0x01`) on the control interface. The interrupt endpoint is drained while waiting. |
| Handshake | `INITIALIZE` (host max transfer 16 KiB), `QUERY OID_802_3_PERMANENT_ADDRESS`, `SET OID_GEN_CURRENT_PACKET_FILTER` |
| `INITIALIZE_CMPLT` layout | status @12, MaxPacketsPerTransfer **@32**, MaxTransferSize **@36**, PacketAlignmentFactor **@40** (2ⁿ). Covered by a unit test. |
| Data messages | `REMOTE_NDIS_PACKET_MSG`: 44-byte header, DataOffset relative to byte 8. Several per transfer, each aligned to the device's factor. |
| Zero-length packets | A transfer whose length is a multiple of the bulk max packet size would need a ZLP. Instead, the last message's `MessageLength` grows by one padding byte. |
| Interface classes | Control `E0/01/03` (Android), `02/02/FF` (Windows-style) or `EF/04/01`, plus CDC-Data `0A` with bulk IN and OUT |
| Packet filter | Linux's default (`DIRECTED | BROADCAST | ALL_MULTICAST | PROMISCUOUS`), falling back to narrower filters if a phone rejects it |
| Example (Samsung) | Accepts 15 frames / 23,700 B per transfer, alignment 16 |

## Protocol notes (CDC-NCM and CDC-ECM)

| Item | Detail |
|---|---|
| Activation | Data interface to alt setting 0, configure, then the alt setting with the bulk endpoints (the CDC reset sequence) |
| NCM parameters | `GET_NTB_PARAMETERS` (mandatory); `SET_NTB_INPUT_SIZE` to 16 KiB and `SET_ETHERNET_PACKET_FILTER` (best effort) |
| NCM transmit | NTB16 with up to 32 datagrams (or the phone's `wNtbOutMaxDatagrams`), each placed at `offset % wNdpOutDivisor == wNdpOutPayloadRemainder`, and the NDP16 aligned to `wNdpOutAlignment` |
| NCM receive | Follows the NDP chain (bounded, to defeat loops); `NCM1` datagrams have their 4-byte CRC stripped |
| ECM | Raw frames. A frame ending on a max-packet boundary gets one pad byte instead of a zero-length packet, as Linux usbnet does. |

## macOS integration

| Mechanism | Used for | Notes |
|---|---|---|
| `feth` pair (`ifconfig fethN create`, `peer`, `lladdr`) | Virtual Ethernet NIC | The host side's MAC address is the one the phone expects |
| BPF (`/dev/bpfN`) | Frame I/O on the peer | `BIOCSBLEN` 256 KiB, `BIOCIMMEDIATE`, `BIOCSHDRCMPLT`, `BIOCSSEESENT=0`, `BIOCPROMISC`, `BIOCSETF` |
| SystemConfiguration preferences (`scutil --prefs`) | Network service, service order | Commits are retried on conflicts; every network location is cleaned on removal |
| Dynamic store (`scutil`) | Reading the address, DNS and primary interface | Polled during setup, every 30 s while online, and on demand for `status` |
| launchd | Background service | `KeepAlive`; reinstall waits for a full unload before bootstrapping |
| kqueue | Crash guard | `EVFILT_PROC` + `NOTE_EXIT`, falling back to polling |
| libusb hotplug | Device arrival and departure | Falls back to 2 s polling if unavailable |

## Design decisions

### Why userspace

Third-party kernel extensions on Apple Silicon require "Reduced Security" boot mode, an auxiliary kernel collection rebuild and a reboot. HoRNDIS no longer loads on current macOS. DriverKit (NetworkingDriverKit + USBDriverKit) is the supported alternative, but Apple grants the USB transport and networking family entitlements case by case, and it requires a paid developer account and C++. A userspace driver can't panic the kernel, installs without changing security settings, and comparable implementations reach about 300 Mbit/s over USB 2.0, far above cellular throughput.

### Why `feth` rather than `utun`

A `utun` interface is a layer-3 tunnel. VPN clients commonly exclude tunnel interfaces when choosing the underlying network; Mullvad, for example, skips every `utun*` name. A `feth` pair is a real Ethernet interface: macOS handles ARP itself, and the interface can back a normal network service.

### Why a real network service rather than a dynamic-store override

Publishing a service only in the dynamic store with `OverridePrimary` (the vpnc-script approach) makes macOS route through it. But clients that read the *configured* service order never see it. A service in the preferences is indistinguishable from one created in System Settings.

### Why macOS does DHCP

When macOS's IPConfiguration agent owns the lease, renewals, network signatures, DNS and routes all behave exactly as on any wired network. The built-in DHCP client in `dhcp.rs` is used only as a fast readiness check before the interface is created.

### Why asynchronous libusb

With synchronous transfers there is at most one transfer in flight per direction, so the bus sits idle between completions. Queued receive transfers and pipelined sends, together with RNDIS aggregation, removed that gap and cut CPU per megabyte by 20 to 40 percent under the same conditions (see [PERFORMANCE.md](PERFORMANCE.md)).

### Why `scutil` subprocesses instead of SystemConfiguration FFI

Configuration changes happen a handful of times per session, so the cost of a subprocess doesn't matter. Driving `scutil` avoids a large unsafe CoreFoundation FFI surface, and the exact commands are easy to audit and reproduce by hand.

## Known limitations

- **One phone at a time.** `find_all` returns a list, but `run` takes the first entry (RNDIS before NCM/ECM) and runs a single session. A second tethering phone is ignored until the first is unplugged.
- **IPv4 only.** IPv6, IPX, mDNS over IPv6 and every other EtherType are dropped in both directions. There is no IPv6 address, no IPv6 DNS and no IPv6 route on the tether, by design (see [SECURITY.md](../SECURITY.md)).
- **The NCM/ECM userspace path is unit-tested only.** No phone that macOS declines to claim has been available for hardware testing. RNDIS is verified end to end.
- **Root is required** to create interfaces, open BPF and edit network preferences. There is no privileged helper split; the whole process runs as root.
- **Fixed names and a single instance.** The pair is always `feth7070`/`feth7071`, the service ID is fixed, and a second `run` refuses to start while one is active.
- **MTU is clamped to 576–1500** from the lease's MTU option; jumbo frames are not supported.
- **Receive costs one `write()` per frame.** BPF accepts a single frame per write, so very high packet rates are syscall-bound. Transmit is batched.
- **The optional adb switch needs USB debugging and Android 9 or later**, and a phone that accepts `svc usb setFunctions`. Without it, tethering is switched on by hand.
- **No traffic accounting beyond counters**, no shaping, no firewall integration. Routing, DNS and priority are macOS's.

## Source map

| File | Responsibility |
|---|---|
| `main.rs` | CLI definition (clap), command handlers, exit codes |
| `tether.rs` | Device loop, session lifecycle, data-path glue, supervision, backoff |
| `usbio.rs` | libusb event thread, hotplug, `RxRing`, `TxPool` |
| `usbnet.rs` | Device discovery (RNDIS/NCM/ECM, CDC Union), native-driver check, `Framing`, device open, phone hints |
| `rndis.rs` | RNDIS control handshake, `TxLimits` packer, packet parser |
| `ncm.rs` | CDC-NCM setup, NTB16 packer and parser |
| `feth.rs` | `feth` pair lifecycle, BPF setup and kernel filter, batched reads, self-pipe |
| `netservice.rs` | Network service creation and removal, service order, dynamic-store queries |
| `dhcp.rs` | Minimal DHCPv4 client (readiness check) |
| `net.rs` | Ethernet, IPv4 and UDP helpers |
| `launchd.rs` | Daemon install, uninstall and safe reload |
| `guard.rs` | Crash guard process |
| `state.rs` | Status file (atomic, write-on-change) |
| `logging.rs` | Leveled logger, colour, file output, rotation |
| `adb.rs` | Optional automatic tethering switch |
