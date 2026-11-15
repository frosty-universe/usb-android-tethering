//! The tethering engine: finds the phone, brings the link up, and keeps it up.
//!
//! ```text
//! phone ⇄ USB (RNDIS, async bulk) ⇄ this process ⇄ BPF ⇄ feth peer ⇄ feth host (Ethernet) ⇄ macOS
//! ```
//!
//! * Phone → Mac: an [`usbio::RxRing`] keeps several bulk-IN transfers queued; completions are
//!   unpacked on the USB event thread and written straight into the virtual NIC.
//! * Mac → phone: the NIC thread reads frames from BPF in batches, packs as many as the phone
//!   accepts into each transfer (RNDIS multi-packet), and queues them on a [`usbio::TxPool`].
//! * Everything else is event-driven: no polling while idle.
//!
//! The host side is registered as the network service "USB Android Tethering", so macOS runs
//! DHCP, routing and DNS for it natively.

use crate::net::{self, ETHERTYPE_ARP, ETHERTYPE_IPV4};
use crate::{adb, dhcp, feth, launchd, netservice, state, usbio, usbnet};
use anyhow::Result;
use log::{debug, error, info, trace, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

pub static STOP: AtomicBool = AtomicBool::new(false);

/// Bulk-IN transfers kept queued (the phone never waits for a free buffer).
const RX_QUEUE: usize = 8;
/// Bulk-OUT transfers that may be in flight at once.
const TX_QUEUE: usize = 8;

/// Asks the engine to stop and wakes every wait (safe to call from a signal-handler thread).
pub fn request_stop() {
    STOP.store(true, Ordering::Relaxed);
    usbio::wake();
}

#[derive(Clone, Default)]
pub struct Options {
    pub once: bool,
    pub probe: bool,
    pub no_primary: bool,
    pub no_adb: bool,
}

#[derive(Default)]
pub struct Counters {
    pub rx_packets: AtomicU64,
    pub rx_bytes: AtomicU64,
    pub tx_packets: AtomicU64,
    pub tx_bytes: AtomicU64,
}

/// State shared by the USB event thread, the NIC thread and the control loop.
pub struct Link {
    pub dev: usbnet::Device,
    pub mac: [u8; 6],
    nic: OnceLock<Arc<feth::Feth>>,
    dhcp_tx: Sender<Vec<u8>>,
    alive: AtomicBool,
    pub counters: Counters,
}

impl Link {
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed) && !STOP.load(Ordering::Relaxed)
    }

    fn kill(&self) {
        self.alive.store(false, Ordering::Relaxed);
        if let Some(nic) = self.nic.get() {
            nic.interrupt();
        }
        usbio::wake();
    }

    /// One bulk-IN transfer from the phone (runs on the USB event thread: keep it short).
    fn on_phone_transfer(&self, buf: &[u8]) {
        let (mut packets, mut bytes) = (0u64, 0u64);
        self.dev.framing.parse(buf, |f| {
            if self.on_phone_frame(f) {
                packets += 1;
                bytes += f.len() as u64;
            }
        });
        if packets > 0 {
            self.counters
                .rx_packets
                .fetch_add(packets, Ordering::Relaxed);
            self.counters.rx_bytes.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    /// One frame from the phone; returns true if it was delivered to macOS.
    fn on_phone_frame(&self, f: &[u8]) -> bool {
        let Some(eth) = net::parse_eth(f) else {
            return false;
        };
        // Point-to-point link, so every unicast frame is ours. Some phones (e.g. ZTE) put a wrong
        // destination MAC on them; rewrite it like Linux's DST_MAC_FIXUP quirk instead of dropping.
        let fixed;
        let f = if net::is_unicast(&eth.dst) && eth.dst != self.mac {
            let mut v = f.to_vec();
            v[..6].copy_from_slice(&self.mac);
            fixed = v;
            fixed.as_slice()
        } else {
            f
        };
        if !matches!(eth.ethertype, ETHERTYPE_IPV4 | ETHERTYPE_ARP) {
            // IPv6 and others are dropped: IPv4-only keeps v6 from leaking around a VPN.
            trace!("dropping ethertype {:#06x} from phone", eth.ethertype);
            return false;
        }
        let Some(nic) = self.nic.get() else {
            // Before the NIC exists, only the readiness-check DHCP client listens.
            if let Some(d) = net::trim_ipv4(eth.payload).and_then(|p| net::udp_payload_to(p, 68)) {
                let _ = self.dhcp_tx.send(d.to_vec());
            }
            return false;
        };
        match nic.inject(f) {
            Ok(()) => true,
            Err(e) => {
                trace!("inject into {}: {e}", nic.host);
                false
            }
        }
    }
}

/// Ends the session if the owning thread exits for any reason, including a panic.
struct KillOnDrop(Arc<Link>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        self.0.kill();
    }
}

/// Mac → phone: batch frames from BPF, pack them per the phone's limits, queue asynchronously.
fn nic_loop(link: &Arc<Link>, nic: &feth::Feth) -> Result<()> {
    let framing = link.dev.framing;
    let mut pool = usbio::TxPool::new(
        link.dev.raw_handle(),
        link.dev.ep_out,
        TX_QUEUE,
        framing.tx_buf_len(),
        {
            let link = link.clone();
            move |why| {
                warn!("sending to the phone failed ({why})");
                link.kill();
            }
        },
    )?;
    // The kernel BPF filter already restricts this to IPv4/ARP; no per-batch allocation.
    let mut buf = vec![0u8; nic.read_buf_len()];
    let mut ranges = Vec::with_capacity(256);
    while link.is_alive() {
        if !nic.read_batch(&mut buf, &mut ranges)? {
            continue; // interrupted: re-check liveness
        }
        ranges.retain(|r| r.len() >= net::ETH_HDR);
        if ranges.is_empty() {
            continue;
        }
        let bytes: usize = ranges.iter().map(|r| r.len()).sum();
        link.counters
            .tx_packets
            .fetch_add(ranges.len() as u64, Ordering::Relaxed);
        link.counters
            .tx_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        let mut i = 0;
        while i < ranges.len() {
            let mut used = 0;
            pool.send(|out| {
                let rest = &ranges[i..];
                let (n, u) = framing.pack_with(rest.len(), |k| &buf[rest[k].clone()], out);
                used = u;
                n
            })?;
            i += used.max(1);
        }
    }
    Ok(())
}

fn human_bytes(b: u64) -> String {
    match b {
        0..1_000_000 => format!("{:.1} KB", b as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", b as f64 / 1e6),
        _ => format!("{:.2} GB", b as f64 / 1e9),
    }
}

fn join_addrs(v: &[std::net::Ipv4Addr]) -> String {
    v.iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn session(found: &usbnet::Found, opts: &Options) -> Result<()> {
    let dev = usbnet::Device::open(found)?;
    let mac = dev.mac;
    info!(
        "link up: {} (host MAC {})",
        dev.framing.describe(),
        net::fmt_mac(&mac)
    );
    let (dhcp_tx, dhcp_rx) = mpsc::channel();
    let link = Arc::new(Link {
        dev,
        mac,
        nic: OnceLock::new(),
        dhcp_tx,
        alive: AtomicBool::new(true),
        counters: Counters::default(),
    });

    // Phone → Mac: queued receive transfers, unpacked on the USB event thread.
    let rx = usbio::RxRing::start(
        link.dev.raw_handle(),
        link.dev.ep_in,
        RX_QUEUE,
        link.dev.framing.rx_buf_len(),
        {
            let link = link.clone();
            move |buf| link.on_phone_transfer(buf)
        },
        {
            let link = link.clone();
            move |why| {
                warn!("phone disconnected ({why})");
                link.kill();
            }
        },
    )?;

    let mut nic_thread = None;
    let mut service_created = false;
    let result = (|| -> Result<()> {
        // `probe` must never write the status file: the background service may be online, and
        // probe would otherwise replace its status with its own.
        if !opts.probe {
            state::write(state::Phase::Connecting, Some(found), None, None, None);
        }
        // Readiness check: the phone's DHCP server answers, so its tethering stack is up.
        let lease = dhcp::acquire(&link, &dhcp_rx)?;
        let summary = format!(
            "{}/{} via {}, DNS {}",
            lease.ip,
            net::mask_prefix(lease.mask),
            lease.router,
            join_addrs(&lease.dns)
        );
        if opts.probe {
            info!("phone network OK: {summary}");
            return Ok(());
        }
        debug!("phone offers {summary}");

        let mtu = lease.mtu.unwrap_or(1500).clamp(576, 1500);
        let nic = Arc::new(feth::Feth::create(mac, mtu)?);
        debug!("created Ethernet interface {} (MTU {mtu})", nic.host);
        let _ = link.nic.set(nic.clone());
        nic_thread = Some(thread::Builder::new().name("nic-tx".into()).spawn({
            let (link, nic) = (link.clone(), nic.clone());
            move || {
                let _end_session = KillOnDrop(link.clone()); // also on panic
                if let Err(e) = nic_loop(&link, &nic)
                    && link.is_alive()
                {
                    error!("transmit path failed: {e:#}");
                }
            }
        })?);

        if launchd::is_installed() {
            // Background mode keeps the service permanently, like a USB Ethernet adapter's,
            // so VPN clients already know it before the phone connects.
            if !netservice::exists() {
                netservice::install(&nic.host, !opts.no_primary)?;
            }
        } else {
            netservice::install(&nic.host, !opts.no_primary)?;
            service_created = true;
        }

        // Both directions are wired and the service exists: only now bring the link up, so
        // macOS's very first DHCP exchange can't race the data path.
        nic.bring_up()?;

        // macOS runs DHCP now. Without a real address after 10 s, bounce the link to restart it
        // rather than wait out macOS's long retry backoff.
        let started = Instant::now();
        let mut last_kick = started;
        let info = loop {
            if let Some(i) = netservice::info() {
                break i;
            }
            anyhow::ensure!(link.is_alive(), "phone disconnected during setup");
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(40),
                "macOS did not get an address from the phone"
            );
            if last_kick.elapsed() >= Duration::from_secs(10) {
                warn!("no address from the phone yet; restarting DHCP");
                nic.bounce()?;
                last_kick = Instant::now();
            }
            thread::sleep(Duration::from_millis(500));
        };
        let mut primary = netservice::primary_interface();
        state::write(
            state::Phase::Online,
            Some(found),
            Some(&nic.host),
            Some(&info),
            Some(&link.counters),
        );
        info!(
            "✔ online: \"{}\" ({}) {} via {}, DNS {}{}",
            netservice::SERVICE_NAME,
            nic.host,
            info.ip,
            info.router.map_or_else(|| "?".into(), |r| r.to_string()),
            join_addrs(&info.dns),
            if primary.as_deref() == Some(&nic.host) {
                " — primary network"
            } else {
                ""
            }
        );

        // Event-driven supervision: wakes on unplug, stop, or every few seconds for stats.
        let mut info = Some(info);
        let mut seen = usbio::change_count();
        let (mut last_net, mut last_log) = (Instant::now(), Instant::now());
        while link.is_alive() {
            usbio::wait_for_change(&mut seen, Duration::from_secs(5));
            if !link.is_alive() {
                break;
            }
            if last_net.elapsed() >= Duration::from_secs(30) {
                last_net = Instant::now();
                let now_info = netservice::info();
                // Self-heal: the address was lost or degraded to link-local (e.g. a lease
                // renewal failed while the phone was busy). Restart DHCP now.
                if now_info.is_none() {
                    warn!("lost the address from the phone; restarting DHCP");
                    if let Err(e) = nic.bounce() {
                        warn!("could not restart DHCP: {e:#}");
                    }
                }
                // Self-heal: someone deleted the service in System Settings while connected.
                if !netservice::exists() {
                    warn!(
                        "network service \"{}\" disappeared; re-creating it",
                        netservice::SERVICE_NAME
                    );
                    if let Err(e) = netservice::install(&nic.host, !opts.no_primary) {
                        warn!("could not re-create the network service: {e:#}");
                    } else if !launchd::is_installed() {
                        service_created = true;
                    }
                }
                info = now_info;
                let now_primary = netservice::primary_interface();
                if now_primary != primary {
                    info!(
                        "primary network is now {}",
                        now_primary.as_deref().unwrap_or("none")
                    );
                    primary = now_primary;
                }
            }
            // No address means the link is not usable: report connecting, not online, so
            // `status` never shows an online tether with a blank address.
            state::write(
                if info.is_some() {
                    state::Phase::Online
                } else {
                    state::Phase::Connecting
                },
                Some(found),
                Some(&nic.host),
                info.as_ref(),
                Some(&link.counters),
            );
            if last_log.elapsed() >= Duration::from_secs(60) {
                last_log = Instant::now();
                let c = &link.counters;
                debug!(
                    "traffic: ↓ {} ({} pkts)  ↑ {} ({} pkts)",
                    human_bytes(c.rx_bytes.load(Ordering::Relaxed)),
                    c.rx_packets.load(Ordering::Relaxed),
                    human_bytes(c.tx_bytes.load(Ordering::Relaxed)),
                    c.tx_packets.load(Ordering::Relaxed)
                );
            }
        }
        Ok(())
    })();

    // Teardown order matters: stop both data paths (their transfers must finish before the USB
    // handle closes), then remove the service; the NIC and handle go with `link`.
    link.kill();
    if let Some(t) = nic_thread {
        let _ = t.join();
    }
    drop(rx);
    if (service_created || (!launchd::is_installed() && netservice::exists()))
        && let Err(e) = netservice::remove()
    {
        error!(
            "could not remove network service: {e:#} — run `sudo usb-android-tethering cleanup`"
        );
    }
    drop(link);
    result
}

/// Removes anything a previous, killed instance left behind.
pub fn cleanup_leftovers() -> bool {
    // An installed background service owns a permanent network service; keep it.
    let service = if launchd::is_installed() {
        Ok(false)
    } else {
        netservice::remove()
    };
    let removed_service = service.unwrap_or_else(|e| {
        warn!("could not remove leftover network service: {e:#}");
        false
    });
    feth::remove_stale() | removed_service
}

/// Tries adb on phones whose tethering is off (at most every 20 s per phone).
fn try_adb(hints: &[usbnet::Hint], tried: &mut HashMap<String, (Instant, u32)>) {
    for h in hints {
        let usbnet::Hint::TetheringOff {
            adb_serial: Some(serial),
            ..
        } = h
        else {
            continue;
        };
        // Phones tether over RNDIS or NCM depending on model and Android version; alternate
        // between the two gadget functions until one brings tethering up.
        let attempts = match tried.get(serial) {
            Some((t, _)) if t.elapsed() < Duration::from_secs(20) => continue,
            Some((_, n)) => *n,
            None => 0,
        };
        let function = if attempts % 2 == 0 { "rndis" } else { "ncm" };
        tried.insert(serial.clone(), (Instant::now(), attempts + 1));
        adb::enable_tethering(serial, function);
    }
}

/// Pause before reconnecting: 2 s normally, doubling per consecutive failure, capped at 60 s.
fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs((2u64 << failures.saturating_sub(1).min(5)).min(60))
}

/// Main loop: wait for a phone, run a session, repeat. Returns true on success.
pub fn run(opts: &Options) -> bool {
    if !opts.probe && cleanup_leftovers() {
        info!("removed leftovers from a previous run");
    }
    let hotplug = usbio::start();
    // With hotplug the rescan is only a backstop; without it, poll.
    let rescan = if hotplug {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(2)
    };
    debug!(
        "USB hotplug {}",
        if hotplug {
            "enabled"
        } else {
            "unavailable; polling"
        }
    );

    let mut seen = usbio::change_count();
    let mut last_hints: Vec<String> = Vec::new();
    let mut adb_tried = HashMap::new();
    let mut announced_wait = false;
    let mut ok = true;
    let mut failures = 0u32;
    if !opts.probe {
        state::write(state::Phase::Waiting, None, None, None, None);
    }
    while !STOP.load(Ordering::Relaxed) {
        let found = match usbnet::find_all() {
            Ok(mut v) if !v.is_empty() => v.swap_remove(0),
            Ok(_) => {
                let raw = usbnet::hints();
                if !opts.no_adb && !opts.probe {
                    try_adb(&raw, &mut adb_tried);
                }
                let hints: Vec<String> = raw.iter().map(usbnet::Hint::describe).collect();
                if hints != last_hints {
                    hints.iter().for_each(|h| info!("{h}"));
                    announced_wait = !hints.is_empty();
                    last_hints = hints;
                }
                if opts.probe {
                    error!("no phone with USB tethering on was found");
                    return false;
                }
                if !announced_wait {
                    info!("waiting for an Android phone with USB tethering on…");
                    announced_wait = true;
                }
                // A new NCM/ECM phone is waiting to see whether macOS claims it: look again soon.
                let wait = if usbnet::native_check_pending() {
                    Duration::from_secs(1)
                } else {
                    rescan
                };
                usbio::wait_for_change(&mut seen, wait);
                continue;
            }
            Err(e) => {
                error!("USB enumeration failed: {e:#}");
                usbio::wait_for_change(&mut seen, Duration::from_secs(2));
                continue;
            }
        };
        last_hints.clear();
        announced_wait = false;
        info!("found phone {}", found.describe());
        let r = session(&found, opts);
        if !opts.probe {
            state::write(state::Phase::Waiting, None, None, None, None);
        }
        match &r {
            Ok(()) if STOP.load(Ordering::Relaxed) || opts.probe => {}
            Ok(()) => info!("phone disconnected; back on your other networks"),
            Err(e) => {
                let msg = format!("{e:#}");
                let hint = if msg.contains("Access denied") {
                    " — another app (e.g. an old tethering driver) is using the phone"
                } else {
                    ""
                };
                error!("{msg}{hint}");
            }
        }
        ok = r.is_ok();
        if opts.once || opts.probe {
            break;
        }
        // The phone re-enumerates when tethering is toggled: give it a moment. Repeated failures
        // back off (2 s → 60 s) instead of hammering the phone; a USB plug event retries at once.
        failures = if ok { 0 } else { failures.saturating_add(1) };
        let delay = retry_delay(failures);
        if failures > 1 {
            info!(
                "retrying in {}s (or when a USB device is plugged in)",
                delay.as_secs()
            );
        }
        seen = usbio::change_count();
        usbio::wait_for_change(&mut seen, delay);
    }
    state::clear(); // pid-guarded: never removes another instance's status
    ok
}

#[cfg(test)]
mod tests {
    use super::retry_delay;

    #[test]
    fn retry_backoff_schedule() {
        let secs: Vec<u64> = (0..9).map(|f| retry_delay(f).as_secs()).collect();
        assert_eq!(secs, [2, 2, 4, 8, 16, 32, 60, 60, 60]);
    }
}
