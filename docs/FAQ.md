# FAQ

Plain answers to the questions people actually ask. For macOS and Mac coverage see [COMPATIBILITY.md](COMPATIBILITY.md); for internals see [ARCHITECTURE.md](ARCHITECTURE.md); for the security model see [SECURITY.md](../SECURITY.md).

## Why doesn't my Mac see my Android phone's USB tethering?

Because most Android phones tether with **RNDIS**, a Microsoft protocol that macOS has never implemented. Turning on USB tethering on the phone does nothing visible on the Mac: no interface appears, no network service is added. This tool supplies the missing RNDIS host driver in userspace.

## Is this HoRNDIS?

It solves the same problem and is a replacement for it, but it is not a fork and shares no code. HoRNDIS is a kernel extension. It no longer loads on current macOS, and on Apple silicon any third-party kernel extension requires lowering the Mac's boot security. This tool runs entirely in userspace: no kext, no boot-security change, no reboot.

The trade-off is that a userspace driver copies every frame through the CPU. That costs far less than the phone's cellular link can deliver, but it is not a kernel fast path. See [PERFORMANCE.md](PERFORMANCE.md).

## Will my VPN work over it?

Yes, and that is the main reason it uses a `feth` pair instead of a tunnel. VPN clients commonly skip `utun*` interfaces when choosing the network to run over, so a tunnel-based tether leaves you choosing between the phone and the VPN. A `feth` pair is a real Ethernet interface, so the VPN treats it like a wired network.

Connect the VPN after the phone is online. If the VPN was already connected, reconnect it once so it picks up the new network.

## Why IPv4 only? Is IPv6 broken?

It is deliberately not carried. Only IPv4 and ARP frames cross the bridge, in both directions. If IPv6 were bridged, it would bypass a VPN that only tunnels IPv4, which is a real leak. Almost every service is reachable over IPv4.

The cost: an IPv6-only destination is unreachable unless your VPN carries IPv6. There is no IPv6 address, route or DNS server on the tether.

## What happens when I unplug the phone?

The session ends, the `feth` pair and (in foreground mode) the network service are removed, and macOS falls back to Wi-Fi or Ethernet by its normal service order. With the background service installed, the tether reconnects by itself when the phone comes back and tethering is on.

If you installed with `--no-primary`, the tether sits below your other networks and is only used when they are unavailable.

## What happens when the Mac sleeps?

The tool does not prevent sleep. It holds no power assertion, and while idle it blocks on events instead of polling, so it does not keep the Mac awake or drain the battery.

When the Mac wakes, USB devices re-enumerate. If the old session did not survive, the daemon goes back to waiting and reconnects when it sees the phone again. If it does not come back within a few seconds, unplug and replug the phone, or run `usb-android-tethering status` to see what it thinks is attached.

## How do I uninstall it completely?

```sh
curl -fsSL https://raw.githubusercontent.com/frosty-universe/usb-android-tethering/main/install.sh | sh -s -- --uninstall
```

That runs `sudo usb-android-tethering uninstall --purge`, which stops and removes the launchd job, deletes the plist and the binary, removes the network service from every network location, and deletes the log. The `feth` interfaces only exist while connected, so there is nothing else to remove.

If you built from source, run `sudo /Library/PrivilegedHelperTools/usb-android-tethering uninstall --purge`.

## Is it safe to run as root?

It has to run as root: creating network interfaces, opening BPF devices and editing network preferences all require it. The design keeps that privilege narrow.

- No kernel code.
- No network sockets, and nothing listens on any port.
- Only IPv4 and ARP frames are bridged.
- The optional `adb` helper is never run as root. It runs as the logged-in console user, and only if the binary is owned by root or that user and is not writable by anyone else.
- The installer builds the tagged source and checks that the binary reports that version before installing it.

The full model, including file permissions, is in [SECURITY.md](../SECURITY.md). Building the tagged source does not protect against a compromised tag; the source is small enough to read.

## My Pixel or other newer phone uses NCM. Does it need this tool?

Maybe not. macOS has a native CDC-NCM driver. If it claims the phone, the phone appears as a normal Ethernet adapter and you need nothing from this tool.

If macOS does not claim the phone, this tool can drive it. It waits 4 seconds after the phone appears for macOS to attach first, and it never detaches a native driver. If the phone is already handled, `usb-android-tethering devices` says so and tells you to do nothing.

The NCM and ECM paths in this tool are unit-tested but have not been tested on hardware, because no phone macOS declined to claim was available. RNDIS, which is what most phones use, is verified end to end.

## Can I use two phones at once?

No. One session runs at a time, and it takes the first tethering phone it finds, preferring RNDIS. A second phone is ignored until the first is unplugged.

## Does it change my DNS, firewall or other networks?

No. It adds one network service, "USB Android Tethering", and lets macOS configure it: macOS runs DHCP, installs routes, sets DNS for that service and ranks it by service order. Other network services, DNS settings for other networks and the firewall are not touched.

Uninstalling removes that one service from every network location.

## Why does it ask for my password?

Because `install`, `run`, `uninstall` and `cleanup` need root for the reasons above. `status`, `devices` and `probe` do not need root, so you can inspect the phone link without a password.

## Do I need USB debugging or adb?

No. Turn on USB tethering on the phone by hand and the tool connects.

If you do enable USB debugging and install `adb` (`brew install android-platform-tools`), the tool can switch tethering on by itself when you plug the phone in. It alternates between the `rndis` and `ncm` USB functions, because phones differ in which one brings tethering up. Tap Allow once on the phone's prompt. Use `--no-adb` to turn this off.

## "Access denied" when claiming the interface. What now?

Another program owns the phone's USB interface. In practice that means an old tethering driver, most often a leftover HoRNDIS or DroidTether install. Remove the other driver, unplug the phone and plug it back in. This tool will not detach a driver it did not install.

## Online, but no internet?

Check that the phone itself has mobile data. `usb-android-tethering status` should show `feth7070` as the primary network and a non-link-local address. An address in `169.254.x.x` means DHCP failed; the tool bounces the link to retry, and `status` will keep showing it as not ready until it succeeds.

If the phone is on a captive-portal Wi-Fi network and is sharing that, the Mac inherits the portal.

## Why are the interface names always `feth7070` and `feth7071`?

So anything left behind after a crash is recognisable and removable. Start-up, the `cleanup` command and the crash guard remove exactly those names and the one fixed network service ID, and nothing else.

## Does it work with an iPhone?

Not as a driver. macOS supports iPhone Personal Hotspot over USB natively, so you do not need this tool. `usb-android-tethering devices` identifies an attached iPhone and says so.

## Will it keep running after a reboot?

If installed as the background service, yes. The launchd job starts at boot and connects whenever the phone has USB tethering on. Without `install`, `run` only lasts until you stop it or the Mac reboots.

## Where are the logs?

`/var/log/usb-android-tethering.log` for the background service, rotated to `.log.1` at 5 MB. In the foreground, logs go to the terminal. Add `-v` for diagnostics, `-vv` for per-packet detail, or `--log-file PATH` to append to a file of your choice.
