//! Tiny DHCPv4 client (RFC 2131) run over the RNDIS link.

use crate::net::{self, BROADCAST, ETHERTYPE_IPV4};
use crate::tether::Link;
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;
const MAGIC: [u8; 4] = [99, 130, 83, 99];
const REPLY_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone, Debug)]
pub struct Lease {
    pub ip: Ipv4Addr,
    pub mask: Ipv4Addr,
    pub router: Ipv4Addr,
    pub dns: Vec<Ipv4Addr>,
    pub mtu: Option<u16>,
}

struct Reply {
    msg_type: u8,
    yiaddr: Ipv4Addr,
    opts: HashMap<u8, Vec<u8>>,
}

fn build(
    msg_type: u8,
    xid: u32,
    mac: [u8; 6],
    ciaddr: Ipv4Addr,
    extra: &[(u8, Vec<u8>)],
) -> Vec<u8> {
    let mut m = vec![0u8; 236];
    m[0] = 1; // BOOTREQUEST
    m[1] = 1; // Ethernet
    m[2] = 6;
    m[4..8].copy_from_slice(&xid.to_be_bytes());
    if ciaddr.is_unspecified() {
        m[10] = 0x80; // ask the server to broadcast replies: we have no address yet
    }
    m[12..16].copy_from_slice(&ciaddr.octets());
    m[28..34].copy_from_slice(&mac);
    m.extend_from_slice(&MAGIC);
    m.extend_from_slice(&[53, 1, msg_type]);
    let mut cid = vec![1u8];
    cid.extend_from_slice(&mac);
    m.extend_from_slice(&[61, cid.len() as u8]);
    m.extend_from_slice(&cid);
    // subnet, router, DNS, MTU, lease, server id, T1, T2
    m.extend_from_slice(&[55, 8, 1, 3, 6, 26, 51, 54, 58, 59]);
    for (code, val) in extra {
        m.extend_from_slice(&[*code, val.len() as u8]);
        m.extend_from_slice(val);
    }
    m.push(255);
    if m.len() < 300 {
        m.resize(300, 0);
    }
    m
}

fn parse(p: &[u8], xid: u32) -> Option<Reply> {
    if p.len() < 240 || p[0] != 2 || p[4..8] != xid.to_be_bytes() || p[236..240] != MAGIC {
        return None;
    }
    let mut opts = HashMap::new();
    let mut i = 240;
    while i < p.len() {
        match p[i] {
            0 => i += 1,
            255 => break,
            code => {
                let len = *p.get(i + 1)? as usize;
                let val = p.get(i + 2..i + 2 + len)?;
                opts.insert(code, val.to_vec());
                i += 2 + len;
            }
        }
    }
    Some(Reply {
        msg_type: *opts.get(&53)?.first()?,
        yiaddr: Ipv4Addr::new(p[16], p[17], p[18], p[19]),
        opts,
    })
}

fn wait(link: &Link, rx: &Receiver<Vec<u8>>, xid: u32, want: &[u8]) -> Option<Reply> {
    let deadline = Instant::now() + REPLY_TIMEOUT;
    while link.is_alive() {
        let left = deadline.checked_duration_since(Instant::now())?;
        match rx.recv_timeout(left.min(Duration::from_millis(250))) {
            Ok(p) => {
                if let Some(r) = parse(&p, xid).filter(|r| want.contains(&r.msg_type)) {
                    return Some(r);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

fn addr(o: &HashMap<u8, Vec<u8>>, code: u8) -> Option<Ipv4Addr> {
    let v = o.get(&code)?;
    (v.len() >= 4).then(|| Ipv4Addr::new(v[0], v[1], v[2], v[3]))
}

fn lease_from(r: &Reply, fallback_server: Ipv4Addr) -> Result<Lease> {
    let server = addr(&r.opts, 54).unwrap_or(fallback_server);
    let router = addr(&r.opts, 3).unwrap_or(server);
    if r.yiaddr.is_unspecified() || router.is_unspecified() {
        bail!("DHCP ACK without address or router");
    }
    let dns = r
        .opts
        .get(&6)
        .map(|v| {
            v.as_chunks::<4>()
                .0
                .iter()
                .map(|c| Ipv4Addr::new(c[0], c[1], c[2], c[3]))
                .collect()
        })
        .filter(|v: &Vec<_>| !v.is_empty())
        .unwrap_or_else(|| vec![router]);
    let mtu = r
        .opts
        .get(&26)
        .filter(|v| v.len() == 2)
        .map(|v| u16::from_be_bytes([v[0], v[1]]));
    Ok(Lease {
        ip: r.yiaddr,
        mask: addr(&r.opts, 1).unwrap_or(Ipv4Addr::new(255, 255, 255, 0)),
        router,
        dns,
        mtu,
    })
}

fn broadcast(link: &Link, payload: &[u8]) -> Result<()> {
    let ip = net::udp_ipv4(Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, 68, 67, payload);
    link.dev
        .send_frame(&net::eth_frame(BROADCAST, link.mac, ETHERTYPE_IPV4, &ip))
}

fn drain(rx: &Receiver<Vec<u8>>) {
    while rx.try_recv().is_ok() {}
}

/// DISCOVER → OFFER → REQUEST → ACK.
pub fn acquire(link: &Link, rx: &Receiver<Vec<u8>>) -> Result<Lease> {
    for attempt in 1..=6 {
        if !link.is_alive() {
            break;
        }
        drain(rx);
        let xid = unsafe { libc::arc4random() };
        log::debug!("DHCP discover (attempt {attempt})");
        broadcast(
            link,
            &build(DISCOVER, xid, link.mac, Ipv4Addr::UNSPECIFIED, &[]),
        )?;
        let Some(offer) = wait(link, rx, xid, &[OFFER]) else {
            continue;
        };
        let server = addr(&offer.opts, 54);
        log::debug!(
            "DHCP offer {} from {}",
            offer.yiaddr,
            server.unwrap_or(Ipv4Addr::UNSPECIFIED)
        );
        let mut extra = vec![(50, offer.yiaddr.octets().to_vec())];
        if let Some(server) = server {
            extra.push((54, server.octets().to_vec()));
        }
        broadcast(
            link,
            &build(REQUEST, xid, link.mac, Ipv4Addr::UNSPECIFIED, &extra),
        )?;
        match wait(link, rx, xid, &[ACK, NAK]) {
            Some(r) if r.msg_type == ACK => {
                return lease_from(&r, server.unwrap_or(Ipv4Addr::UNSPECIFIED));
            }
            Some(_) => log::debug!("DHCP NAK, restarting"),
            None => {}
        }
    }
    bail!("no DHCP lease from the phone (is USB tethering switched on?)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ack() {
        let xid = 0x1234_5678;
        let mut p = build(ACK, xid, [2, 0, 0, 0, 0, 1], Ipv4Addr::UNSPECIFIED, &[]);
        p[0] = 2;
        p[16..20].copy_from_slice(&[192, 168, 42, 77]);
        // Replace options after the magic cookie.
        p.truncate(240);
        p.extend_from_slice(&[53, 1, ACK, 1, 4, 255, 255, 255, 0, 3, 4, 192, 168, 42, 129]);
        p.extend_from_slice(&[
            6, 8, 8, 8, 8, 8, 1, 1, 1, 1, 51, 4, 0, 0, 0x0e, 0x10, 54, 4, 192, 168, 42, 129, 255,
        ]);
        let r = parse(&p, xid).unwrap();
        let l = lease_from(&r, Ipv4Addr::UNSPECIFIED).unwrap();
        assert_eq!(l.ip, Ipv4Addr::new(192, 168, 42, 77));
        assert_eq!(l.router, Ipv4Addr::new(192, 168, 42, 129));
        assert_eq!(
            l.dns,
            vec![Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(1, 1, 1, 1)]
        );
        assert!(parse(&p, xid + 1).is_none());
    }
}
