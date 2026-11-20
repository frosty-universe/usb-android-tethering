//! CDC-NCM (USB Communications Class, Network Control Model 1.0): the tethering protocol of
//! Pixel 6 and later and of a growing number of Android phones (Linux is retiring RNDIS).
//!
//! macOS has a native NCM driver. This userspace implementation is used only when that driver
//! did not take the phone, so the tool never competes with, or detaches, a working native
//! driver. Datagrams travel in NCM Transfer Blocks (NTB16): a header (NTH16), the datagrams,
//! and a datagram pointer table (NDP16).

use anyhow::{Context, Result};
use rusb::{DeviceHandle, GlobalContext};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

const NTH16_SIG: u32 = 0x484D_434E; // "NCMH"
const NDP16_SIG: u32 = 0x304D_434E; // "NCM0": no CRC
const NDP16_SIG_CRC: u32 = 0x314D_434E; // "NCM1": each datagram carries a 4-byte CRC
const NTH16_LEN: usize = 12;
const GET_NTB_PARAMETERS: u8 = 0x80;
const SET_NTB_INPUT_SIZE: u8 = 0x86;
const SET_ETHERNET_PACKET_FILTER: u8 = 0x43;
/// Largest NTB we ask the phone to send us (also the receive buffer size).
const HOST_MAX_NTB_IN: u32 = 0x4000;
/// Upper bound on datagrams we pack per NTB, whatever the phone allows.
const MAX_DATAGRAMS: usize = 32;
const TIMEOUT: Duration = Duration::from_secs(1);

static SEQUENCE: AtomicU16 = AtomicU16::new(0);

/// NTB parameters negotiated with the phone (from GET_NTB_PARAMETERS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    /// Size of NTBs the phone will send us (our receive buffers).
    pub in_max: usize,
    /// Largest NTB the phone accepts from us.
    pub out_max: usize,
    /// Datagram placement: offset % out_divisor == out_remainder.
    pub out_divisor: usize,
    pub out_remainder: usize,
    /// NDP16 alignment in NTBs we send.
    pub out_ndp_align: usize,
    /// Datagrams per NTB the phone accepts.
    pub out_max_datagrams: usize,
    /// Bulk-OUT max packet size (to avoid transfers that would need a zero-length packet).
    pub mps: usize,
}

fn le16(b: &[u8], off: usize) -> usize {
    u16::from_le_bytes([b[off], b[off + 1]]) as usize
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn put16(b: &mut [u8], off: usize, v: usize) {
    b[off..off + 2].copy_from_slice(&(v as u16).to_le_bytes());
}

impl Params {
    /// Decodes the 28-byte NTB parameter structure (CDC NCM 1.0, table 6-3).
    fn from_ntb_parameters(p: &[u8], mps: usize) -> Option<Self> {
        if p.len() < 28 {
            return None;
        }
        let pow2 = |v: usize| if v.is_power_of_two() { v } else { 4 };
        Some(Params {
            in_max: (le32(p, 4) as usize).clamp(2048, 65535),
            out_max: (le32(p, 16) as usize).clamp(2048, 65535),
            out_divisor: le16(p, 20).max(1),
            out_remainder: le16(p, 22),
            out_ndp_align: pow2(le16(p, 24).max(4)),
            out_max_datagrams: match le16(p, 26) {
                0 => MAX_DATAGRAMS,
                n => n.min(MAX_DATAGRAMS),
            },
            mps: mps.max(1),
        })
    }

    /// Receive buffer size.
    pub fn rx_buf_len(&self) -> usize {
        self.in_max
    }

    /// Transmit buffer size (always enough for one full frame).
    pub fn tx_buf_len(&self) -> usize {
        self.out_max.max(2048)
    }

    /// Packs frames `frame(0..count)` into one NTB16. Returns (bytes, frames consumed); a frame
    /// too large to fit alone is dropped rather than wedging the queue.
    pub fn pack_with<'a>(
        &self,
        count: usize,
        frame: impl Fn(usize) -> &'a [u8],
        out: &mut [u8],
    ) -> (usize, usize) {
        // One spare byte keeps the transfer off a max-packet boundary (no zero-length packet).
        // `soft` is the smaller of the phone's NTB limit and the buffer.
        let soft = self.out_max.min(out.len()).saturating_sub(1);
        if soft == 0 {
            return (0, 0);
        }
        let mut entries = [(0usize, 0usize); MAX_DATAGRAMS];
        let (mut pos, mut n) = (NTH16_LEN, 0);
        for i in 0..count {
            let f = frame(i);
            let start = pos
                + (self.out_remainder + self.out_divisor - pos % self.out_divisor)
                    % self.out_divisor;
            let end = start + f.len();
            let ndp = end.next_multiple_of(self.out_ndp_align);
            let total = ndp + 8 + 4 * (n + 2); // NDP header + entries + terminator
            if n > 0 && (n == self.out_max_datagrams || total > soft) {
                break;
            }
            if total > soft {
                return (0, 1);
            }
            out[pos..start].fill(0);
            out[start..end].copy_from_slice(f);
            entries[n] = (start, f.len());
            pos = end;
            n += 1;
        }
        if n == 0 {
            return (0, 0);
        }
        let ndp = pos.next_multiple_of(self.out_ndp_align);
        out[pos..ndp].fill(0);
        let ndp_len = 8 + 4 * (n + 1);
        out[ndp..ndp + 4].copy_from_slice(&NDP16_SIG.to_le_bytes());
        put16(out, ndp + 4, ndp_len);
        put16(out, ndp + 6, 0); // no next NDP
        for (k, &(idx, len)) in entries[..n].iter().enumerate() {
            put16(out, ndp + 8 + 4 * k, idx);
            put16(out, ndp + 10 + 4 * k, len);
        }
        out[ndp + 8 + 4 * n..ndp + ndp_len].fill(0); // terminator entry
        let mut total = ndp + ndp_len;
        if total.is_multiple_of(self.mps) {
            out[total] = 0; // padding covered by wBlockLength
            total += 1;
        }
        out[0..4].copy_from_slice(&NTH16_SIG.to_le_bytes());
        put16(out, 4, NTH16_LEN);
        put16(out, 6, SEQUENCE.fetch_add(1, Ordering::Relaxed) as usize);
        put16(out, 8, total);
        put16(out, 10, ndp);
        (total, n)
    }
}

/// Calls `on_frame` for every datagram in a received NTB16. Malformed data is skipped;
/// nothing here can panic on hostile input.
pub fn parse_ntb(buf: &[u8], mut on_frame: impl FnMut(&[u8])) {
    if buf.len() < NTH16_LEN || le32(buf, 0) != NTH16_SIG {
        return;
    }
    let block = match le16(buf, 8) {
        0 => buf.len(),
        n => n.min(buf.len()),
    };
    let mut ndp = le16(buf, 10);
    // Follow the NDP chain; the bound stops a malicious loop of NDPs pointing at each other.
    for _ in 0..8 {
        if ndp < NTH16_LEN || ndp + 8 > block {
            return;
        }
        let crc = match le32(buf, ndp) {
            NDP16_SIG => 0,
            NDP16_SIG_CRC => 4,
            _ => return,
        };
        let ndp_len = le16(buf, ndp + 4).min(block - ndp);
        let mut e = ndp + 8;
        while e + 4 <= ndp + ndp_len {
            let (idx, len) = (le16(buf, e), le16(buf, e + 2));
            if idx == 0 || len == 0 {
                break;
            }
            if len > crc && idx + len <= block {
                on_frame(&buf[idx..idx + len - crc]);
            }
            e += 4;
        }
        let next = le16(buf, ndp + 6);
        // Only forward links are legal; a backwards or self link would replay datagrams.
        if next <= ndp {
            return;
        }
        ndp = next;
    }
}

/// NCM setup on a claimed function, before the data alt setting is selected. Reads the NTB
/// parameters (the only mandatory step), then caps NTB size and sets a filter, best effort.
pub fn setup(h: &DeviceHandle<GlobalContext>, ctrl_if: u8, out_mps: usize) -> Result<Params> {
    let idx = ctrl_if as u16;
    let mut p = [0u8; 28];
    let n = h
        .read_control(0xA1, GET_NTB_PARAMETERS, 0, idx, &mut p, TIMEOUT)
        .context("NCM GET_NTB_PARAMETERS")?;
    let mut params =
        Params::from_ntb_parameters(&p[..n], out_mps).context("malformed NCM NTB parameters")?;
    let want = params.in_max.min(HOST_MAX_NTB_IN as usize) as u32;
    if h.write_control(
        0x21,
        SET_NTB_INPUT_SIZE,
        0,
        idx,
        &want.to_le_bytes(),
        TIMEOUT,
    )
    .is_ok()
    {
        params.in_max = want as usize;
    }
    // DIRECTED | BROADCAST | ALL_MULTICAST; optional in the spec, ignored by many phones.
    let _ = h.write_control(0x21, SET_ETHERNET_PACKET_FILTER, 0x000E, idx, &[], TIMEOUT);
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Params {
        Params {
            in_max: 16384,
            out_max: 16384,
            out_divisor: 4,
            out_remainder: 2,
            out_ndp_align: 4,
            out_max_datagrams: 3,
            mps: 512,
        }
    }

    #[test]
    fn decodes_ntb_parameters() {
        // Values as Android's f_ncm reports them.
        let mut p = [0u8; 28];
        put16(&mut p, 0, 28);
        put16(&mut p, 2, 1);
        p[4..8].copy_from_slice(&16384u32.to_le_bytes());
        p[16..20].copy_from_slice(&16384u32.to_le_bytes());
        put16(&mut p, 20, 4);
        put16(&mut p, 22, 0);
        put16(&mut p, 24, 4);
        put16(&mut p, 26, 0);
        let got = Params::from_ntb_parameters(&p, 512).unwrap();
        assert_eq!(
            (
                got.out_max,
                got.out_divisor,
                got.out_ndp_align,
                got.out_max_datagrams
            ),
            (16384, 4, 4, 32)
        );
    }

    #[test]
    fn pack_parse_roundtrip() {
        let frames: Vec<Vec<u8>> = (0..5u8).map(|i| vec![i; 60 + 97 * i as usize]).collect();
        let p = params();
        let mut out = vec![0u8; p.tx_buf_len()];
        let (n, used) = p.pack_with(frames.len(), |i| &frames[i], &mut out);
        assert_eq!(used, 3, "respects wNtbOutMaxDatagrams");
        assert_eq!(le16(&out, 8), n, "wBlockLength covers the whole transfer");
        let mut got = Vec::new();
        parse_ntb(&out[..n], |f| got.push(f.to_vec()));
        assert_eq!(got, frames[..3]);
        // Datagram placement honours divisor/remainder.
        let ndp = le16(&out, 10);
        for k in 0..3 {
            assert_eq!(le16(&out, ndp + 8 + 4 * k) % 4, 2);
        }
    }

    #[test]
    fn parse_follows_forward_ndp_chain() {
        // Two NDPs, one datagram each: both frames arrive, and the chain stops at a null link.
        let mut b = vec![0u8; 80];
        b[0..4].copy_from_slice(&NTH16_SIG.to_le_bytes());
        put16(&mut b, 8, 80);
        put16(&mut b, 10, 12);
        for (ndp, next, idx) in [(12, 32, 48), (32, 0, 64)] {
            b[ndp..ndp + 4].copy_from_slice(&NDP16_SIG.to_le_bytes());
            put16(&mut b, ndp + 4, 16);
            put16(&mut b, ndp + 6, next);
            put16(&mut b, ndp + 8, idx);
            put16(&mut b, ndp + 10, 8);
            b[idx..idx + 8].fill(ndp as u8);
        }
        let mut got = Vec::new();
        parse_ntb(&b, |f| got.push(f.to_vec()));
        assert_eq!(got, vec![vec![12u8; 8], vec![32u8; 8]]);
        // A backwards link closes the chain after the NDP it reuses was already consumed.
        put16(&mut b, 38, 12);
        got.clear();
        parse_ntb(&b, |f| got.push(f.to_vec()));
        assert_eq!(got, vec![vec![12u8; 8], vec![32u8; 8]]);
    }

    #[test]
    fn parse_strips_crc_and_skips_bad_datagrams() {
        let mut b = vec![0u8; 48];
        b[0..4].copy_from_slice(&NTH16_SIG.to_le_bytes());
        put16(&mut b, 8, 48);
        put16(&mut b, 10, 12);
        b[12..16].copy_from_slice(&NDP16_SIG_CRC.to_le_bytes());
        put16(&mut b, 16, 16);
        put16(&mut b, 20, 40);
        put16(&mut b, 22, 8); // 4 bytes payload + 4-byte CRC
        b[40..48].copy_from_slice(b"abcdCRC!");
        let mut got = Vec::new();
        parse_ntb(&b, |f| got.push(f.to_vec()));
        assert_eq!(got, vec![b"abcd".to_vec()]);
        // Length equal to the CRC alone leaves no payload.
        put16(&mut b, 22, 4);
        got.clear();
        parse_ntb(&b, |f| got.push(f.to_vec()));
        assert!(got.is_empty());
        // Datagram running past wBlockLength is skipped, not sliced.
        put16(&mut b, 22, 0xffff);
        got.clear();
        parse_ntb(&b, |f| got.push(f.to_vec()));
        assert!(got.is_empty());
    }

    #[test]
    fn pack_respects_out_max_smaller_than_buffer() {
        let p = Params {
            out_max: 1600,
            ..params()
        };
        let mut out = vec![0u8; p.tx_buf_len()];
        assert!(out.len() > p.out_max);
        let ok = vec![0u8; 1400];
        let (n, used) = p.pack_with(1, |_| &ok, &mut out);
        assert_eq!(used, 1);
        assert!(n <= p.out_max);
        let big = vec![0u8; 1600];
        assert_eq!(
            p.pack_with(1, |_| &big, &mut out),
            (0, 1),
            "over wNtbOutMaxSize"
        );
    }

    #[test]
    fn parse_rejects_garbage_and_loops() {
        let mut calls = 0;
        parse_ntb(&[0xff; 64], |_| calls += 1);
        parse_ntb(&[], |_| calls += 1);
        // An NDP that points at itself must not loop forever.
        let mut b = vec![0u8; 64];
        b[0..4].copy_from_slice(&NTH16_SIG.to_le_bytes());
        put16(&mut b, 4, 12);
        put16(&mut b, 8, 64);
        put16(&mut b, 10, 12);
        b[12..16].copy_from_slice(&NDP16_SIG.to_le_bytes());
        put16(&mut b, 16, 16);
        put16(&mut b, 18, 12); // next NDP = itself
        parse_ntb(&b, |_| calls += 1);
        assert_eq!(calls, 0);
    }

    #[test]
    fn parse_survives_hostile_ntbs() {
        let mut calls = 0;
        // wBlockLength far beyond the transfer, then NDP entries beyond the block.
        let mut b = vec![0u8; 40];
        b[0..4].copy_from_slice(&NTH16_SIG.to_le_bytes());
        put16(&mut b, 8, 0xffff);
        put16(&mut b, 10, 12);
        parse_ntb(&b, |_| calls += 1);
        b[12..16].copy_from_slice(&NDP16_SIG.to_le_bytes());
        put16(&mut b, 16, 0xffff); // NDP length beyond the block
        put16(&mut b, 20, 0xfff0); // datagram offset beyond the block
        put16(&mut b, 22, 0xfff0); // datagram length beyond the block
        parse_ntb(&b, |_| calls += 1);
        assert_eq!(calls, 0);

        // Every truncation of a valid block parses and yields only in-bounds frames.
        let p = params();
        let frame = vec![9u8; 100];
        let mut out = vec![0u8; p.tx_buf_len()];
        let (n, _) = p.pack_with(1, |_| &frame, &mut out);
        for cut in 0..=n {
            parse_ntb(&out[..cut], |f| assert!(f.len() <= cut));
        }
    }
}
