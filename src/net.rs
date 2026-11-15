//! Minimal Ethernet / ARP / IPv4 / UDP helpers for the link between the Mac and the phone.

use std::net::Ipv4Addr;

pub const ETH_HDR: usize = 14;
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const BROADCAST: [u8; 6] = [0xff; 6];

pub fn eth_frame(dst: [u8; 6], src: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity((ETH_HDR + payload.len()).max(60));
    f.extend_from_slice(&dst);
    f.extend_from_slice(&src);
    f.extend_from_slice(&ethertype.to_be_bytes());
    f.extend_from_slice(payload);
    if f.len() < 60 {
        f.resize(60, 0);
    }
    f
}

pub struct Eth<'a> {
    pub dst: [u8; 6],
    pub ethertype: u16,
    pub payload: &'a [u8],
}

pub fn parse_eth(f: &[u8]) -> Option<Eth<'_>> {
    if f.len() < ETH_HDR {
        return None;
    }
    Some(Eth {
        dst: f[0..6].try_into().unwrap(),
        ethertype: u16::from_be_bytes([f[12], f[13]]),
        payload: &f[ETH_HDR..],
    })
}

pub fn is_unicast(mac: &[u8; 6]) -> bool {
    mac[0] & 1 == 0
}

pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for c in data.chunks(2) {
        let w = if c.len() == 2 {
            u16::from_be_bytes([c[0], c[1]])
        } else {
            (c[0] as u16) << 8
        };
        sum += w as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Builds an IPv4/UDP packet (UDP checksum 0 = none, permitted for IPv4).
pub fn udp_ipv4(src: Ipv4Addr, dst: Ipv4Addr, sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
    let udp_len = 8 + payload.len();
    let total = 20 + udp_len;
    let mut p = Vec::with_capacity(total);
    p.extend_from_slice(&[0x45, 0x10]);
    p.extend_from_slice(&(total as u16).to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, 0, 64, 17, 0, 0]);
    p.extend_from_slice(&src.octets());
    p.extend_from_slice(&dst.octets());
    let c = checksum(&p[..20]);
    p[10..12].copy_from_slice(&c.to_be_bytes());
    p.extend_from_slice(&sport.to_be_bytes());
    p.extend_from_slice(&dport.to_be_bytes());
    p.extend_from_slice(&(udp_len as u16).to_be_bytes());
    p.extend_from_slice(&[0, 0]);
    p.extend_from_slice(payload);
    p
}

/// If `ip` is an IPv4 UDP packet to `dport`, returns its UDP payload.
pub fn udp_payload_to(ip: &[u8], dport: u16) -> Option<&[u8]> {
    if ip.len() < 20 || ip[0] >> 4 != 4 || ip[9] != 17 {
        return None;
    }
    // Ignore fragments (DHCP replies are never fragmented in practice).
    if u16::from_be_bytes([ip[6], ip[7]]) & 0x3fff != 0 {
        return None;
    }
    let ihl = ((ip[0] & 0x0f) as usize) * 4;
    if ihl < 20 {
        return None;
    }
    let total = (u16::from_be_bytes([ip[2], ip[3]]) as usize).min(ip.len());
    let udp = ip.get(ihl..total)?;
    if udp.len() < 8 || u16::from_be_bytes([udp[2], udp[3]]) != dport {
        return None;
    }
    let ulen = (u16::from_be_bytes([udp[4], udp[5]]) as usize).clamp(8, udp.len());
    Some(&udp[8..ulen])
}

/// Trims Ethernet padding from an IPv4 packet using its total-length field.
pub fn trim_ipv4(ip: &[u8]) -> Option<&[u8]> {
    if ip.len() < 20 || ip[0] >> 4 != 4 {
        return None;
    }
    let total = u16::from_be_bytes([ip[2], ip[3]]) as usize;
    if total < 20 || total > ip.len() {
        return None;
    }
    Some(&ip[..total])
}

pub fn mask_prefix(mask: Ipv4Addr) -> u32 {
    u32::from(mask).leading_ones()
}

pub fn fmt_mac(m: &[u8; 6]) -> String {
    m.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_of_known_header() {
        // Example header from RFC 1071 discussions / Wikipedia IPv4 checksum.
        let h = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        assert_eq!(checksum(&h), 0xb861);
    }

    #[test]
    fn udp_roundtrip() {
        let p = udp_ipv4(
            Ipv4Addr::new(1, 2, 3, 4),
            Ipv4Addr::BROADCAST,
            68,
            67,
            b"hello",
        );
        assert_eq!(checksum(&p[..20]), 0);
        assert_eq!(udp_payload_to(&p, 67), Some(&b"hello"[..]));
        assert_eq!(udp_payload_to(&p, 68), None);
        let mut padded = p.clone();
        padded.extend_from_slice(&[0; 10]);
        assert_eq!(trim_ipv4(&padded), Some(&p[..]));
    }

    #[test]
    fn prefix() {
        assert_eq!(mask_prefix(Ipv4Addr::new(255, 255, 255, 0)), 24);
    }

    #[test]
    fn checksum_odd_length_and_empty() {
        assert_eq!(checksum(&[]), 0xffff);
        // One trailing byte is padded with a zero low byte.
        assert_eq!(checksum(&[0x00, 0x01, 0x00]), checksum(&[0x00, 0x01]));
    }

    #[test]
    fn rejects_short_ethernet_frames() {
        assert!(parse_eth(&[]).is_none());
        assert!(parse_eth(&[0u8; 13]).is_none());
        let f = eth_frame([1; 6], [2; 6], ETHERTYPE_ARP, &[]);
        assert_eq!(f.len(), 60, "padded to the Ethernet minimum");
        assert_eq!(parse_eth(&f).unwrap().ethertype, ETHERTYPE_ARP);
    }

    #[test]
    fn udp_rejects_hostile_headers() {
        let p = udp_ipv4(
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(10, 0, 0, 2),
            1234,
            68,
            b"x",
        );
        assert!(udp_payload_to(&p, 68).is_some());

        // IHL below the 20-byte minimum must not expose header bytes as payload.
        let mut short_ihl = p.clone();
        short_ihl[0] = 0x44;
        assert!(udp_payload_to(&short_ihl, 68).is_none());
        // IHL past the packet.
        let mut long_ihl = p.clone();
        long_ihl[0] = 0x4f;
        assert!(udp_payload_to(&long_ihl, 68).is_none());
        // IP total length lying beyond the buffer is clamped, not trusted.
        let mut long_total = p.clone();
        long_total[2..4].copy_from_slice(&0xfff0u16.to_be_bytes());
        assert!(udp_payload_to(&long_total, 68).is_some());
        // UDP length below its own header, and beyond the packet, clamp safely.
        let mut tiny_udp = p.clone();
        tiny_udp[24..26].copy_from_slice(&0u16.to_be_bytes());
        assert_eq!(udp_payload_to(&tiny_udp, 68), Some(&[][..]));
        let mut huge_udp = p.clone();
        huge_udp[24..26].copy_from_slice(&0xffffu16.to_be_bytes());
        assert_eq!(udp_payload_to(&huge_udp, 68), Some(&b"x"[..]));
        // Every truncation yields either None or a payload inside the input.
        for cut in 0..p.len() {
            if let Some(d) = udp_payload_to(&p[..cut], 68) {
                assert!(d.len() < cut);
            }
        }
    }

    #[test]
    fn fragments_and_non_udp_are_refused() {
        let p = udp_ipv4(
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(10, 0, 0, 2),
            68,
            67,
            b"y",
        );
        for flags in [0x2000u16, 0x0001] {
            let mut f = p.clone();
            f[6..8].copy_from_slice(&flags.to_be_bytes());
            assert!(udp_payload_to(&f, 67).is_none(), "fragment {flags:#06x}");
        }
        let mut tcp = p.clone();
        tcp[9] = 6;
        assert!(udp_payload_to(&tcp, 67).is_none());
        // IPv6 and non-IPv4 versions are refused.
        let mut v6 = p;
        v6[0] = 0x60;
        assert!(udp_payload_to(&v6, 67).is_none());
    }

    #[test]
    fn trim_ipv4_rejects_bad_lengths() {
        let p = udp_ipv4(
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(10, 0, 0, 2),
            68,
            67,
            b"z",
        );
        assert_eq!(trim_ipv4(&p), Some(p.as_slice()));
        let mut short = p.clone();
        short[2..4].copy_from_slice(&19u16.to_be_bytes());
        assert!(trim_ipv4(&short).is_none(), "total length below the header");
        let mut long = p.clone();
        long[2..4].copy_from_slice(&(p.len() as u16 + 1).to_be_bytes());
        assert!(trim_ipv4(&long).is_none(), "total length beyond the frame");
    }
}
