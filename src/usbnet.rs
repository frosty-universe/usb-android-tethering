//! USB network-function layer: finds tethering phones, opens them, and hides which of the
//! three tethering protocols a phone speaks behind one [`Framing`] used by the data path.
//!
//! | Protocol | Used by | Control interface | Handled by |
//! |---|---|---|---|
//! | RNDIS | most Android phones | `E0/01/03`, `02/02/FF`, `EF/04/01`, `EF/01/01` | this tool |
//! | CDC-NCM | Pixel 6+, newer phones | `02/0D/00` | macOS natively; this tool as fallback |
//! | CDC-ECM | some phones and gadgets | `02/06/00` | macOS natively; this tool as fallback |
//!
//! For NCM and ECM the tool only steps in when macOS's own driver has not claimed the phone,
//! and it never detaches a native driver.

use crate::{ncm, rndis};
use anyhow::{Context, Result};
use rusb::{Direction, GlobalContext, TransferType, UsbContext};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a new NCM/ECM phone is left alone so macOS's native driver can claim it first.
const NATIVE_GRACE: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Rndis,
    Ncm,
    Ecm,
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Protocol::Rndis => "RNDIS",
            Protocol::Ncm => "CDC-NCM",
            Protocol::Ecm => "CDC-ECM",
        }
    }

    fn of_control(class: u8, sub: u8, proto: u8) -> Option<Self> {
        if rndis::is_control_interface(class, sub, proto) {
            Some(Protocol::Rndis)
        } else {
            match (class, sub) {
                (0x02, 0x0D) => Some(Protocol::Ncm),
                (0x02, 0x06) => Some(Protocol::Ecm),
                _ => None,
            }
        }
    }
}

/// A tethering function found on the bus (not yet opened).
pub struct Found {
    device: rusb::Device<GlobalContext>,
    pub vid: u16,
    pub pid: u16,
    pub protocol: Protocol,
    config: u8,
    ctrl_if: u8,
    int_ep: Option<u8>,
    data_if: u8,
    data_alt: u8,
    ep_in: u8,
    ep_out: u8,
    out_mps: usize,
}

impl Found {
    pub fn describe(&self) -> String {
        let name = self
            .device
            .open()
            .ok()
            .and_then(|h| {
                let d = self.device.device_descriptor().ok()?;
                h.read_product_string_ascii(&d).ok()
            })
            .unwrap_or_else(|| "unknown".into());
        format!(
            "{:04x}:{:04x} \"{}\" {} (bus {} addr {}, ctrl if {}, data if {})",
            self.vid,
            self.pid,
            name,
            self.protocol.name(),
            self.device.bus_number(),
            self.device.address(),
            self.ctrl_if,
            self.data_if
        )
    }
}

/// CDC class-specific descriptors (CS_INTERFACE, type 0x24) attached to a control interface.
fn cs_descriptors(extra: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = extra;
    std::iter::from_fn(move || {
        let len = *rest.first()? as usize;
        if len < 3 || len > rest.len() {
            return None;
        }
        let (d, tail) = rest.split_at(len);
        rest = tail;
        Some(d)
    })
    .filter(|d| d[1] == 0x24)
}

/// The data interface named by the CDC Union functional descriptor (subtype 0x06).
fn union_data_interface(extra: &[u8]) -> Option<u8> {
    cs_descriptors(extra)
        .find(|d| d[2] == 0x06 && d.len() >= 5)
        .map(|d| d[4])
}

type DataIface = (u8, u8, u8, u8, usize); // interface, alt, ep in, ep out, out mps

/// Picks the CDC-Data interface: the Union descriptor's choice if it has bulk endpoints,
/// otherwise the one right after the control interface, otherwise any.
fn data_interface(
    cfg: &rusb::ConfigDescriptor,
    ctrl_if: u8,
    union: Option<u8>,
) -> Option<DataIface> {
    let mut best: Option<(u8, DataIface)> = None; // (rank, candidate): lower rank wins
    for iface in cfg.interfaces() {
        for alt in iface.descriptors() {
            if alt.class_code() != 0x0A {
                continue;
            }
            let bulk = |dir| {
                alt.endpoint_descriptors()
                    .find(|e| e.transfer_type() == TransferType::Bulk && e.direction() == dir)
            };
            let (Some(i), Some(o)) = (bulk(Direction::In), bulk(Direction::Out)) else {
                continue;
            };
            let n = alt.interface_number();
            let rank = if Some(n) == union {
                0
            } else if ctrl_if.checked_add(1) == Some(n) {
                1
            } else {
                2
            };
            let cand = (
                n,
                alt.setting_number(),
                i.address(),
                o.address(),
                o.max_packet_size() as usize,
            );
            if best.is_none_or(|(r, _)| rank < r) {
                best = Some((rank, cand));
            }
        }
    }
    best.map(|(_, c)| c)
}

/// When each NCM/ECM device was first seen (for [`NATIVE_GRACE`]).
/// Keyed by (bus, address, vid, pid).
type SeenMap = HashMap<(u8, u8, u16, u16), Instant>;
static FIRST_SEEN: Mutex<Option<SeenMap>> = Mutex::new(None);

/// True once an NCM/ECM device has been present long enough and macOS's native driver has
/// not claimed it; false while macOS may still be attaching or when it owns the device.
fn ncm_ecm_is_ours(device: &rusb::Device<GlobalContext>, vid: u16, pid: u16, ctrl_if: u8) -> bool {
    let key = (device.bus_number(), device.address(), vid, pid);
    let mut seen = FIRST_SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let map = seen.get_or_insert_with(HashMap::new);
    map.retain(|_, t| t.elapsed() < Duration::from_secs(3600)); // forget long-gone devices
    let first = *map.entry(key).or_insert_with(Instant::now);
    drop(seen);
    if first.elapsed() < NATIVE_GRACE {
        return false;
    }
    // If the native driver is attached (or we can't tell), leave the phone to macOS.
    device
        .open()
        .and_then(|h| h.kernel_driver_active(ctrl_if))
        .is_ok_and(|active| !active)
}

/// True while a new NCM/ECM phone is inside its grace period (callers should rescan soon).
pub fn native_check_pending() -> bool {
    FIRST_SEEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|m| m.values().any(|t| t.elapsed() < NATIVE_GRACE))
}

/// Every tethering function this tool should drive: all RNDIS phones, plus NCM/ECM phones
/// that macOS's native driver did not claim.
pub fn find_all() -> Result<Vec<Found>> {
    let mut out = Vec::new();
    for device in GlobalContext::default().devices()?.iter() {
        let Ok(desc) = device.device_descriptor() else {
            continue;
        };
        let (vid, pid) = (desc.vendor_id(), desc.product_id());
        for ci in 0..desc.num_configurations() {
            let Ok(cfg) = device.config_descriptor(ci) else {
                continue;
            };
            let mut ctrl = None;
            for iface in cfg.interfaces() {
                for alt in iface.descriptors() {
                    let Some(protocol) = Protocol::of_control(
                        alt.class_code(),
                        alt.sub_class_code(),
                        alt.protocol_code(),
                    ) else {
                        continue;
                    };
                    if ctrl.is_none() {
                        let int_ep = alt
                            .endpoint_descriptors()
                            .find(|e| {
                                e.transfer_type() == TransferType::Interrupt
                                    && e.direction() == Direction::In
                            })
                            .map(|e| e.address());
                        ctrl = Some((
                            protocol,
                            alt.interface_number(),
                            int_ep,
                            union_data_interface(alt.extra()),
                        ));
                    }
                }
            }
            let Some((protocol, ctrl_if, int_ep, union)) = ctrl else {
                continue;
            };
            let Some((data_if, data_alt, ep_in, ep_out, out_mps)) =
                data_interface(&cfg, ctrl_if, union)
            else {
                continue;
            };
            if protocol != Protocol::Rndis && !ncm_ecm_is_ours(&device, vid, pid, ctrl_if) {
                break; // macOS drives it natively (or is about to): not ours
            }
            out.push(Found {
                vid,
                pid,
                protocol,
                config: cfg.number(),
                ctrl_if,
                int_ep,
                data_if,
                data_alt,
                ep_in,
                ep_out,
                out_mps: out_mps.max(1),
                device: device.clone(),
            });
            break;
        }
    }
    // RNDIS first: it is the protocol macOS can never handle itself.
    out.sort_by_key(|f| f.protocol != Protocol::Rndis);
    Ok(out)
}

/// How Ethernet frames are wrapped in USB transfers for the phone's protocol.
#[derive(Clone, Copy, Debug)]
pub enum Framing {
    Rndis(rndis::TxLimits),
    Ncm(ncm::Params),
    /// One raw Ethernet frame per transfer.
    Ecm {
        mps: usize,
        mtu: usize,
    },
}

const ETHERNET_MTU: usize = 1500;

impl Framing {
    pub fn rx_buf_len(&self) -> usize {
        match self {
            Framing::Rndis(_) => rndis::HOST_MAX_TRANSFER as usize,
            Framing::Ncm(p) => p.rx_buf_len(),
            Framing::Ecm { mtu, .. } => crate::net::ETH_HDR + mtu + 4,
        }
    }

    pub fn tx_buf_len(&self) -> usize {
        match self {
            Framing::Rndis(l) => l.buf_len(),
            Framing::Ncm(p) => p.tx_buf_len(),
            Framing::Ecm { mtu, .. } => crate::net::ETH_HDR + mtu + 4,
        }
    }

    /// Calls `on_frame` for each Ethernet frame in one received transfer.
    pub fn parse(&self, buf: &[u8], on_frame: impl FnMut(&[u8])) {
        match self {
            Framing::Rndis(_) => rndis::parse_packets(buf, on_frame),
            Framing::Ncm(_) => ncm::parse_ntb(buf, on_frame),
            Framing::Ecm { mtu, .. } => {
                let mut on_frame = on_frame;
                let buf = &buf[..buf.len().min(crate::net::ETH_HDR + *mtu + 4)];
                if buf.len() >= crate::net::ETH_HDR {
                    let frame = match crate::net::parse_eth(buf) {
                        Some(eth) if eth.ethertype == crate::net::ETHERTYPE_IPV4 => {
                            crate::net::trim_ipv4(eth.payload)
                                .map_or(&buf[..crate::net::ETH_HDR], |payload| {
                                    &buf[..crate::net::ETH_HDR + payload.len()]
                                })
                        }
                        _ => buf,
                    };
                    on_frame(frame);
                }
            }
        }
    }

    /// Packs frames `0..count` (from the front) into one transfer; returns (bytes, frames used).
    pub fn pack_with<'a>(
        &self,
        count: usize,
        frame: impl Fn(usize) -> &'a [u8],
        out: &mut [u8],
    ) -> (usize, usize) {
        match self {
            Framing::Rndis(l) => l.pack_with(count, frame, out),
            Framing::Ncm(p) => p.pack_with(count, frame, out),
            Framing::Ecm { mps, mtu } => {
                if count == 0 {
                    return (0, 0);
                }
                let f = frame(0);
                if f.len() > crate::net::ETH_HDR + *mtu || f.len() + 1 > out.len() {
                    return (0, 1);
                }
                out[..f.len()].copy_from_slice(f);
                // A frame ending on a max-packet boundary would need a zero-length packet;
                // append one pad byte instead, as Linux usbnet does.
                let mut n = f.len();
                if n.is_multiple_of(*mps) {
                    out[n] = 0;
                    n += 1;
                }
                (n, 1)
            }
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Framing::Rndis(l) => format!(
                "RNDIS, phone accepts {} frame(s) / {} B per transfer, alignment {}",
                l.max_pkts, l.max_xfer, l.align
            ),
            Framing::Ncm(p) => format!(
                "CDC-NCM, NTB in {} B / out {} B, up to {} datagrams per NTB",
                p.in_max, p.out_max, p.out_max_datagrams
            ),
            Framing::Ecm { .. } => "CDC-ECM, one frame per transfer".into(),
        }
    }
}

/// An opened, initialised tethering function.
pub struct Device {
    h: rusb::DeviceHandle<GlobalContext>,
    ctrl_if: u8,
    data_if: u8,
    pub ep_in: u8,
    pub ep_out: u8,
    pub framing: Framing,
    /// Host-side MAC address to use (what the phone sends to).
    pub mac: [u8; 6],
    /// Buffer for the synchronous send path (setup traffic before the async pool exists).
    tx: Mutex<Vec<u8>>,
}

/// A stable, locally administered MAC derived from the phone's identity, so the phone's DHCP
/// server hands out the same address every time (FNV-1a over vid, pid and serial number).
fn stable_mac(vid: u16, pid: u16, serial: &str) -> [u8; 6] {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in vid
        .to_le_bytes()
        .iter()
        .chain(&pid.to_le_bytes())
        .chain(serial.as_bytes())
    {
        h = (h ^ *b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let b = h.to_be_bytes();
    [(b[0] & 0xfe) | 0x02, b[1], b[2], b[3], b[4], b[5]]
}

/// Claims the control interface, then the data interface. If the second claim fails, the first
/// is released so the next attempt (or another tool) isn't blocked by a half-open claim.
fn claim_both(
    h: &rusb::DeviceHandle<GlobalContext>,
    ctrl: u8,
    data: u8,
    may_detach: bool,
) -> Result<()> {
    claim(h, ctrl, may_detach)?;
    if let Err(e) = claim(h, data, may_detach) {
        let _ = h.release_interface(ctrl);
        return Err(e);
    }
    Ok(())
}

fn claim(h: &rusb::DeviceHandle<GlobalContext>, iface: u8, may_detach: bool) -> Result<()> {
    match h.claim_interface(iface) {
        Ok(()) => Ok(()),
        Err(first) if may_detach => {
            // Only for RNDIS, which no macOS driver implements: a class driver that matched
            // the interface by accident can be detached safely.
            let _ = h.detach_kernel_driver(iface);
            h.claim_interface(iface)
                .with_context(|| format!("claim USB interface {iface} (first error: {first})"))
        }
        Err(e) => {
            Err(e).with_context(|| format!("claim USB interface {iface} (a macOS driver owns it)"))
        }
    }
}

impl Device {
    pub fn open(f: &Found) -> Result<Self> {
        let h = f.device.open().context("open USB device")?;
        if h.active_configuration().unwrap_or(0) != f.config {
            h.set_active_configuration(f.config)
                .context("select the tethering USB configuration")?;
        }
        let rndis = f.protocol == Protocol::Rndis;
        claim_both(&h, f.ctrl_if, f.data_if, rndis)?;
        let serial = f
            .device
            .device_descriptor()
            .ok()
            .and_then(|d| h.read_serial_number_string_ascii(&d).ok())
            .unwrap_or_default();
        let fallback_mac = stable_mac(f.vid, f.pid, &serial);

        // Past this point the interfaces are claimed: every error path releases them, so a
        // failed attempt doesn't leave the phone half-owned until the USB handle is dropped.
        match Self::configure(&h, f, fallback_mac) {
            Ok((framing, mac)) => Ok(Device {
                h,
                ctrl_if: f.ctrl_if,
                data_if: f.data_if,
                ep_in: f.ep_in,
                ep_out: f.ep_out,
                framing,
                mac,
                tx: Mutex::new(vec![0; framing.tx_buf_len()]),
            }),
            Err(e) => {
                let _ = h.release_interface(f.data_if);
                let _ = h.release_interface(f.ctrl_if);
                Err(e)
            }
        }
    }

    /// Runs the protocol setup once both interfaces are claimed. No cleanup here: `open`
    /// releases the interfaces if this returns an error.
    fn configure(
        h: &rusb::DeviceHandle<GlobalContext>,
        f: &Found,
        fallback_mac: [u8; 6],
    ) -> Result<(Framing, [u8; 6])> {
        let (framing, mac) = match f.protocol {
            Protocol::Rndis => {
                if f.data_alt != 0 {
                    h.set_alternate_setting(f.data_if, f.data_alt)?;
                }
                let (limits, mac) = rndis::initialize(h, f.ctrl_if, f.int_ep, f.out_mps)?;
                (Framing::Rndis(limits), mac.unwrap_or(fallback_mac))
            }
            Protocol::Ncm | Protocol::Ecm => {
                // CDC reset sequence: data interface to alt 0 (no endpoints), configure, then
                // select the alt setting with the bulk endpoints to start the data path.
                let _ = h.set_alternate_setting(f.data_if, 0);
                let framing = if f.protocol == Protocol::Ncm {
                    Framing::Ncm(ncm::setup(h, f.ctrl_if, f.out_mps)?)
                } else {
                    // SET_ETHERNET_PACKET_FILTER: DIRECTED | BROADCAST | ALL_MULTICAST.
                    let _ = h.write_control(
                        0x21,
                        0x43,
                        0x000E,
                        f.ctrl_if as u16,
                        &[],
                        Duration::from_secs(1),
                    );
                    Framing::Ecm {
                        mps: f.out_mps,
                        mtu: ETHERNET_MTU,
                    }
                };
                h.set_alternate_setting(f.data_if, f.data_alt)
                    .context("activate the data interface")?;
                (framing, fallback_mac)
            }
        };
        Ok((framing, mac))
    }

    /// Raw libusb handle for the asynchronous transfer engine.
    pub fn raw_handle(&self) -> *mut rusb::ffi::libusb_device_handle {
        self.h.as_raw()
    }

    /// Sends one frame synchronously (setup traffic; the data path uses `usbio::TxPool`).
    pub fn send_frame(&self, frame: &[u8]) -> Result<()> {
        let mut buf = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        let (n, _) = self.framing.pack_with(1, |_| frame, &mut buf);
        if n > 0 {
            self.h
                .write_bulk(self.ep_out, &buf[..n], Duration::from_secs(2))?;
        }
        Ok(())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.h.release_interface(self.data_if);
        let _ = self.h.release_interface(self.ctrl_if);
    }
}

// Phones we can't (or needn't) drive, with advice

/// A phone that is attached but not tethering through this tool right now.
pub enum Hint {
    /// Android phone in MTP/charging/ADB mode: USB tethering is switched off.
    /// `adb_serial` is set when USB debugging is on, so tethering can be switched on remotely.
    TetheringOff {
        vid: u16,
        pid: u16,
        brand: &'static str,
        adb_serial: Option<String>,
    },
    /// Phone tethering over CDC-NCM/ECM that macOS's native driver has claimed.
    Native {
        vid: u16,
        pid: u16,
        protocol: Protocol,
    },
    /// iPhone/iPad: Personal Hotspot over USB is built into macOS.
    Apple { pid: u16 },
}

impl Hint {
    /// One-line explanation with what the user should do.
    pub fn describe(&self) -> String {
        match self {
            Hint::TetheringOff {
                vid,
                pid,
                brand,
                adb_serial,
            } => format!(
                "{brand} phone {vid:04x}:{pid:04x} is connected but USB tethering is off — turn it on in \
                 Settings → Hotspot & tethering → USB tethering{}",
                if adb_serial.is_some() {
                    " (USB debugging is on: it is switched on automatically when adb is installed)"
                } else {
                    ""
                }
            ),
            Hint::Native { vid, pid, protocol } => format!(
                "phone {vid:04x}:{pid:04x} tethers over {}, which macOS is handling natively — nothing to do",
                protocol.name()
            ),
            Hint::Apple { pid } => {
                format!(
                    "iPhone/iPad 05ac:{pid:04x} — use Personal Hotspot, which macOS supports over USB natively"
                )
            }
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let (vid, pid, state) = match self {
            Hint::TetheringOff { vid, pid, .. } => (*vid, *pid, "tethering_off"),
            Hint::Native { vid, pid, .. } => (*vid, *pid, "native"),
            Hint::Apple { pid } => (0x05ac, *pid, "apple_native"),
        };
        serde_json::json!({
            "vid": format!("{vid:04x}"),
            "pid": format!("{pid:04x}"),
            "state": state,
            "usb_debugging": matches!(self, Hint::TetheringOff { adb_serial: Some(_), .. }),
            "advice": self.describe(),
        })
    }
}

/// USB vendor IDs of Android phone makers (for friendlier messages only; detection itself
/// works by interface class, so unknown brands are still recognised as "Android").
fn android_brand(vid: u16) -> Option<&'static str> {
    Some(match vid {
        0x18d1 => "Google",
        0x04e8 => "Samsung",
        0x2717 => "Xiaomi",
        0x2a70 => "OnePlus",
        0x22d9 => "OPPO/realme",
        0x2d95 => "vivo",
        0x12d1 => "Huawei",
        0x339b => "Honor",
        0x22b8 => "Motorola",
        0x0fce => "Sony",
        0x1004 => "LG",
        0x0bb4 => "HTC",
        0x2b0e => "Nothing",
        0x05c6 => "Qualcomm-based",
        0x0e8d => "MediaTek-based",
        0x1782 => "Unisoc-based",
        0x17ef => "Lenovo",
        0x2916 => "Yota",
        0x1bbb => "TCL/Alcatel",
        0x19d2 => "ZTE",
        0x2a45 => "Meizu",
        0x0489 => "Nokia/FIH",
        0x2e04 => "HMD",
        0x2ae5 => "Fairphone",
        0x1949 => "Amazon",
        0x0b05 => "ASUS",
        0x109b => "Hisense",
        0x1ebf => "Coolpad",
        0x29a9 => "Smartisan",
        0x0421 => "Nokia",
        0x091e => "Garmin",
        _ => return None,
    })
}

/// Phones that are plugged in but not tethering through this tool, with the reason.
pub fn hints() -> Vec<Hint> {
    let mut out = Vec::new();
    let Ok(list) = GlobalContext::default().devices() else {
        return out;
    };
    let ours: Vec<(u8, u8)> = find_all()
        .unwrap_or_default()
        .iter()
        .map(|f| (f.device.bus_number(), f.device.address()))
        .collect();
    for device in list.iter() {
        if ours.contains(&(device.bus_number(), device.address())) {
            continue;
        }
        let Ok(desc) = device.device_descriptor() else {
            continue;
        };
        let (vid, pid) = (desc.vendor_id(), desc.product_id());
        let Ok(cfg) = device.active_config_descriptor() else {
            continue;
        };
        let alts: Vec<_> = cfg
            .interfaces()
            .flat_map(|i| {
                i.descriptors()
                    .map(|a| (a.class_code(), a.sub_class_code(), a.protocol_code()))
                    .collect::<Vec<_>>()
            })
            .collect();
        if vid == 0x05ac {
            // iPhone/iPad expose PTP (06/01) and/or the Apple tethering interface (ff/fd).
            if alts
                .iter()
                .any(|&(c, s, _)| (c, s) == (0x06, 0x01) || (c, s) == (0xff, 0xfd))
            {
                out.push(Hint::Apple { pid });
            }
            continue;
        }
        if let Some(protocol) = alts
            .iter()
            .find_map(|&(c, s, p)| Protocol::of_control(c, s, p))
        {
            if protocol != Protocol::Rndis {
                out.push(Hint::Native { vid, pid, protocol });
            }
            continue;
        }
        // ADB (ff/42/01) or MTP/PTP (06/01/01) interfaces identify an Android phone.
        let has_adb = alts.contains(&(0xff, 0x42, 0x01));
        let is_phone_iface = has_adb || alts.iter().any(|&(c, s, _)| (c, s) == (0x06, 0x01));
        if let Some(brand) = android_brand(vid).or(is_phone_iface.then_some("Android")) {
            let adb_serial = has_adb
                .then(|| {
                    let h = device.open().ok()?;
                    h.read_serial_number_string_ascii(&desc).ok()
                })
                .flatten()
                .filter(|s| !s.is_empty());
            out.push(Hint::TetheringOff {
                vid,
                pid,
                brand,
                adb_serial,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_union_data_interface() {
        // Header (0x00), Union (0x06: control 0, data 1), Ethernet networking (0x0F).
        let extra = [
            5, 0x24, 0x00, 0x10, 0x01, 5, 0x24, 0x06, 0, 1, 13, 0x24, 0x0F, 4, 0, 0, 0, 0, 0xEA, 5,
            0, 0, 0,
        ];
        assert_eq!(union_data_interface(&extra), Some(1));
        assert_eq!(union_data_interface(&[]), None);
        assert_eq!(
            union_data_interface(&[2, 0x24]),
            None,
            "truncated descriptors are ignored"
        );
    }

    #[test]
    fn classifies_control_interfaces() {
        assert_eq!(Protocol::of_control(0xE0, 1, 3), Some(Protocol::Rndis));
        assert_eq!(Protocol::of_control(0xEF, 1, 1), Some(Protocol::Rndis));
        assert_eq!(Protocol::of_control(0x02, 0x0D, 0), Some(Protocol::Ncm));
        assert_eq!(Protocol::of_control(0x02, 0x06, 0), Some(Protocol::Ecm));
        assert_eq!(
            Protocol::of_control(0x02, 0x02, 0x01),
            None,
            "a plain modem (ACM) is not tethering"
        );
    }

    #[test]
    fn stable_mac_is_local_unicast_and_deterministic() {
        let a = stable_mac(0x18d1, 0x4eeb, "ABC123");
        assert_eq!(a, stable_mac(0x18d1, 0x4eeb, "ABC123"));
        assert_ne!(a, stable_mac(0x18d1, 0x4eeb, "ABC124"));
        assert_eq!(a[0] & 0x03, 0x02, "locally administered, unicast");
    }

    #[test]
    fn ecm_pads_to_avoid_zero_length_packets() {
        let f = Framing::Ecm {
            mps: 512,
            mtu: ETHERNET_MTU,
        };
        let frame = vec![1u8; 512];
        let mut out = vec![0u8; f.tx_buf_len()];
        assert_eq!(f.pack_with(1, |_| &frame, &mut out), (513, 1));
        let mut got = 0;
        f.parse(&out[..60], |_| got += 1);
        assert_eq!(got, 1);
    }

    #[test]
    fn ecm_drops_an_oversize_frame_without_panicking() {
        let f = Framing::Ecm {
            mps: 512,
            mtu: ETHERNET_MTU,
        };
        let big = vec![0u8; 200];
        let mut small = vec![0u8; 100];
        assert_eq!(f.pack_with(1, |_| &big, &mut small), (0, 1));
        let mut short = vec![0u8; 13];
        assert_eq!(f.pack_with(1, |_| &big, &mut short), (0, 1));
    }
}
