//! RNDIS (Microsoft Remote NDIS), the USB tethering protocol most Android phones use:
//! the control handshake and the data framing. Device discovery and USB plumbing live in
//! `usbnet.rs`; this module only speaks the protocol.
//!
//! Compatibility follows Linux's `rndis_host` (the reference host implementation):
//! the same interface classes are matched, the same default packet filter is used, and
//! phones that reject parts of the handshake degrade gracefully instead of failing.

use anyhow::{Context, Result, bail};
use rusb::{DeviceHandle, GlobalContext};
use std::time::Duration;

const MSG_PACKET: u32 = 0x0000_0001;
const MSG_INIT: u32 = 0x0000_0002;
const MSG_QUERY: u32 = 0x0000_0004;
const MSG_SET: u32 = 0x0000_0005;
const CMPLT: u32 = 0x8000_0000;

const OID_802_3_PERMANENT_ADDRESS: u32 = 0x0101_0101;
const OID_GEN_CURRENT_PACKET_FILTER: u32 = 0x0001_010E;
/// Packet filters to try, most capable first. The first is Linux's RNDIS_DEFAULT_FILTER
/// (DIRECTED | BROADCAST | ALL_MULTICAST | PROMISCUOUS); some phones reject parts of it.
const PACKET_FILTERS: [u32; 3] = [0x0000_002D, 0x0000_000D, 0x0000_0009];

const PACKET_HDR_LEN: usize = 44;
/// Largest transfer we accept from the phone (announced in INITIALIZE); also the RX buffer size.
pub const HOST_MAX_TRANSFER: u32 = 0x4000;
const CTRL_TIMEOUT: Duration = Duration::from_secs(1);

/// Interface class/subclass/protocol triples that identify an RNDIS control interface.
/// Mirrors the class matches in Linux `drivers/net/usb/rndis_host.c`.
pub fn is_control_interface(class: u8, sub: u8, proto: u8) -> bool {
    matches!(
        (class, sub, proto),
        (0xE0, 0x01, 0x03) // Wireless controller / RNDIS: Android's default
            | (0x02, 0x02, 0xFF) // CDC ACM with vendor protocol: Windows-style RNDIS
            | (0xEF, 0x04, 0x01) // Miscellaneous / RNDIS over Ethernet
            | (0xEF, 0x01, 0x01) // Miscellaneous / "ActiveSync" RNDIS variant
    )
}

/// Transmit limits the phone announced in INITIALIZE_CMPLT.
#[derive(Clone, Copy, Debug)]
pub struct TxLimits {
    /// Frames the phone accepts per bulk transfer (RNDIS multi-packet aggregation).
    pub max_pkts: usize,
    /// Largest bulk-OUT transfer the phone accepts.
    pub max_xfer: usize,
    /// Each RNDIS message starts on this boundary.
    pub align: usize,
    /// Bulk-OUT max packet size (to avoid transfers that would need a zero-length packet).
    pub mps: usize,
}

impl TxLimits {
    /// Reads the phone's limits from INITIALIZE_CMPLT: MaxPacketsPerTransfer @32,
    /// MaxTransferSize @36, PacketAlignmentFactor @40 (2^n).
    fn apply_init_cmplt(&mut self, r: &[u8]) {
        if r.len() >= 44 {
            self.max_pkts = (le32(r, 32) as usize).clamp(1, 32);
            self.max_xfer = (le32(r, 36) as usize).clamp(1600, 64 * 1024);
            self.align = 1 << le32(r, 40).min(7);
        }
    }

    /// Size of a transmit buffer that can always hold at least one full frame.
    pub fn buf_len(&self) -> usize {
        self.max_xfer.max(2048)
    }

    /// Test convenience: [`Self::pack_with`] over a slice of frames.
    #[cfg(test)]
    pub fn pack(&self, frames: &[&[u8]], out: &mut [u8]) -> (usize, usize) {
        self.pack_with(frames.len(), |i| frames[i], out)
    }

    /// Packs frames `frame(0..count)` into `out` as consecutive RNDIS_PACKET_MSGs, within the
    /// phone's limits. The closure avoids collecting slices first. Returns (bytes, frames).
    pub fn pack_with<'a>(
        &self,
        count: usize,
        frame: impl Fn(usize) -> &'a [u8],
        out: &mut [u8],
    ) -> (usize, usize) {
        // One spare byte keeps the transfer off a max-packet boundary (no zero-length packet).
        // `soft` is the phone's limit when aggregating; `hard` is the buffer, which bounds a lone frame.
        let hard = out.len() - 1;
        let soft = self.max_xfer.min(out.len()) - 1;
        let (mut len, mut used, mut last) = (0, 0, 0);
        for i in 0..count {
            let f = frame(i);
            let msg = (PACKET_HDR_LEN + f.len()).next_multiple_of(self.align);
            if used > 0 && (used == self.max_pkts || len + msg > soft) {
                break;
            }
            if len + msg > hard {
                // A frame bigger than the phone allows: drop it rather than wedge the queue.
                return (0, 1);
            }
            let hdr = [
                MSG_PACKET,
                msg as u32,
                36,
                f.len() as u32,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ];
            for (i, v) in hdr.iter().enumerate() {
                out[len + 4 * i..len + 4 * i + 4].copy_from_slice(&v.to_le_bytes());
            }
            out[len + PACKET_HDR_LEN..len + PACKET_HDR_LEN + f.len()].copy_from_slice(f);
            out[len + PACKET_HDR_LEN + f.len()..len + msg].fill(0);
            last = len;
            len += msg;
            used += 1;
        }
        // A transfer that ends exactly on a max-packet boundary would need a zero-length packet;
        // grow the last message by one byte instead (allowed: MessageLength covers padding).
        if len > 0 && len.is_multiple_of(self.mps) {
            let grown = le32(out, last + 4) + 1;
            out[last + 4..last + 8].copy_from_slice(&grown.to_le_bytes());
            out[len] = 0;
            len += 1;
        }
        (len, used)
    }
}

/// Calls `on_frame` for every Ethernet frame in a bulk-IN transfer (one or more
/// RNDIS_PACKET_MSGs); malformed trailing data is ignored.
pub fn parse_packets(buf: &[u8], mut on_frame: impl FnMut(&[u8])) {
    let n = buf.len();
    let mut off = 0;
    while off + PACKET_HDR_LEN <= n {
        let (t, len) = (le32(buf, off), le32(buf, off + 4) as usize);
        if t != MSG_PACKET || len < PACKET_HDR_LEN || off + len > n {
            break;
        }
        let start = off + 8 + le32(buf, off + 8) as usize;
        let dlen = le32(buf, off + 12) as usize;
        if start.checked_add(dlen).is_some_and(|end| end <= off + len) {
            on_frame(&buf[start..start + dlen]);
        }
        off += len;
    }
}

/// Control channel of one RNDIS function.
struct Control<'a> {
    h: &'a DeviceHandle<GlobalContext>,
    ctrl_if: u16,
    int_ep: Option<u8>,
    next_id: u32,
}

impl Control<'_> {
    /// Sends an encapsulated command and waits for its completion message.
    fn command(&mut self, mut msg: Vec<u8>) -> Result<Vec<u8>> {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        msg[8..12].copy_from_slice(&self.next_id.to_le_bytes());
        let (msg_type, req_id) = (le32(&msg, 0), self.next_id);
        self.h
            .write_control(0x21, 0x00, 0, self.ctrl_if, &msg, CTRL_TIMEOUT)
            .context("RNDIS SEND_ENCAPSULATED_COMMAND")?;
        let mut buf = vec![0u8; 4096];
        for _ in 0..100 {
            // Some devices only answer once their RESPONSE_AVAILABLE notification is read.
            if let Some(ep) = self.int_ep {
                let mut n = [0u8; 16];
                let _ = self.h.read_interrupt(ep, &mut n, Duration::from_millis(20));
            }
            match self
                .h
                .read_control(0xA1, 0x01, 0, self.ctrl_if, &mut buf, CTRL_TIMEOUT)
            {
                Ok(n)
                    if n >= 16
                        && le32(&buf, 0) == (msg_type | CMPLT)
                        && le32(&buf, 8) == req_id =>
                {
                    let status = le32(&buf, 12);
                    if status != 0 {
                        bail!("RNDIS message {msg_type:#x} failed with status {status:#010x}");
                    }
                    buf.truncate(n);
                    return Ok(buf);
                }
                // Stale replies, status indications, "not ready yet": keep waiting.
                Ok(_) | Err(rusb::Error::Pipe) | Err(rusb::Error::Timeout) => {}
                Err(e) => return Err(e).context("RNDIS GET_ENCAPSULATED_RESPONSE"),
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        bail!("RNDIS device did not answer message {msg_type:#x}")
    }

    fn message(words: &[u32], payload: &[u8]) -> Vec<u8> {
        let mut m: Vec<u8> = words.iter().flat_map(|v| v.to_le_bytes()).collect();
        m.extend_from_slice(payload);
        m
    }

    fn query(&mut self, oid: u32) -> Result<Vec<u8>> {
        let r = self.command(Self::message(&[MSG_QUERY, 28, 0, oid, 0, 20, 0], &[]))?;
        let (len, off) = (le32(&r, 16) as usize, le32(&r, 20) as usize + 8);
        r.get(off..off + len)
            .map(<[u8]>::to_vec)
            .context("malformed RNDIS QUERY_CMPLT")
    }

    fn set(&mut self, oid: u32, value: &[u8]) -> Result<()> {
        let total = 28 + value.len() as u32;
        let msg = Self::message(&[MSG_SET, total, 0, oid, value.len() as u32, 20, 0], value);
        self.command(msg).map(|_| ())
    }
}

/// Runs the RNDIS handshake on a claimed control interface. Returns the phone's transmit
/// limits and the host MAC address the phone expects (if it reports one).
pub fn initialize(
    h: &DeviceHandle<GlobalContext>,
    ctrl_if: u8,
    int_ep: Option<u8>,
    out_mps: usize,
) -> Result<(TxLimits, Option<[u8; 6]>)> {
    let mut c = Control {
        h,
        ctrl_if: ctrl_if as u16,
        int_ep,
        next_id: 0,
    };
    let mut limits = TxLimits {
        max_pkts: 1,
        max_xfer: 2048,
        align: 1,
        mps: out_mps.max(1),
    };
    let r = c
        .command(Control::message(
            &[MSG_INIT, 24, 0, 1, 0, HOST_MAX_TRANSFER],
            &[],
        ))
        .context("RNDIS INITIALIZE")?;
    limits.apply_init_cmplt(&r);
    let mac = c
        .query(OID_802_3_PERMANENT_ADDRESS)
        .ok()
        .and_then(|b| b.get(..6).map(|s| s.try_into().unwrap()));
    let mut last = None;
    for filter in PACKET_FILTERS {
        match c.set(OID_GEN_CURRENT_PACKET_FILTER, &filter.to_le_bytes()) {
            Ok(()) => return Ok((limits, mac)),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap()).context("RNDIS set packet filter")
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_pkts: usize, align: usize, mps: usize) -> TxLimits {
        TxLimits {
            max_pkts,
            max_xfer: 16384,
            align,
            mps,
        }
    }

    #[test]
    fn pack_parse_roundtrip_multi_packet() {
        let frames: Vec<Vec<u8>> = (0..5u8).map(|i| vec![i; 60 + i as usize * 100]).collect();
        let refs: Vec<&[u8]> = frames.iter().map(Vec::as_slice).collect();
        let l = limits(3, 4, 512);
        let mut out = vec![0; l.buf_len()];
        let (n, used) = l.pack(&refs, &mut out);
        assert_eq!(used, 3, "respects MaxPacketsPerTransfer");
        let mut got = Vec::new();
        parse_packets(&out[..n], |f| got.push(f.to_vec()));
        assert_eq!(got, frames[..3]);
        // Every message starts aligned.
        let mut off = 0;
        while off < n.saturating_sub(PACKET_HDR_LEN) {
            assert_eq!(off % 4, 0);
            off += le32(&out, off + 4) as usize;
        }
    }

    #[test]
    fn pack_avoids_zero_length_packet() {
        // 44-byte header + 468-byte frame = 512 = one full max-size packet.
        let frame = vec![7u8; 468];
        let l = limits(1, 1, 512);
        let mut out = vec![0; l.buf_len()];
        let (n, used) = l.pack(&[&frame], &mut out);
        assert_eq!((n, used), (513, 1));
        let mut got = Vec::new();
        parse_packets(&out[..n], |f| got.push(f.to_vec()));
        assert_eq!(got, vec![frame]);
    }

    #[test]
    fn reads_init_cmplt_layout() {
        // Values as Android's f_rndis reports them: 10 packets, 16 KiB, alignment 2^2.
        let fields = [CMPLT | MSG_INIT, 52, 1, 0, 1, 0, 1, 0, 10, 16384, 2, 0, 0];
        let r: Vec<u8> = fields.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut l = limits(1, 1, 512);
        l.apply_init_cmplt(&r);
        assert_eq!((l.max_pkts, l.max_xfer, l.align), (10, 16384, 4));
    }

    #[test]
    fn parse_rejects_garbage() {
        let mut calls = 0;
        parse_packets(&[0xff; 100], |_| calls += 1);
        parse_packets(&[], |_| calls += 1);
        assert_eq!(calls, 0);
    }
}
