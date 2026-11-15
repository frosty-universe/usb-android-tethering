//! Virtual Ethernet NIC built from a macOS `feth` (fake ethernet) pair.
//!
//!   macOS stack ⇄ fethH (host side: IP address, ARP, routes) ⇄ fethP (peer) ⇄ BPF ⇄ this process
//!
//! Unlike utun, the host side is an ordinary Ethernet interface, so VPN clients that ignore
//! tunnel interfaces (e.g. Mullvad skips every `utun*`) accept it as the underlying network.
//! Frames written to the peer through BPF arrive on the host side as received traffic, and
//! frames the host side transmits are read from the peer through BPF.

use anyhow::{Context, Result, bail, ensure};
use std::ffi::CString;
use std::io;
use std::ops::Range;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Command;

/// Fixed unit numbers so a pair left behind by a crash is recognised and removed.
pub const HOST_UNIT: u32 = 7070;
pub const PEER_UNIT: u32 = 7071;
const BIOCSSEESENT: libc::c_ulong = 0x8004_4277; // _IOW('B', 119, u_int)
const BPF_BUF: libc::c_uint = 256 * 1024;

pub fn host_name() -> String {
    format!("feth{HOST_UNIT}")
}

pub fn peer_name() -> String {
    format!("feth{PEER_UNIT}")
}

fn ifconfig(args: &[&str]) -> Result<()> {
    let out = Command::new("/sbin/ifconfig")
        .args(args)
        .output()
        .context("run ifconfig")?;
    if !out.status.success() {
        bail!(
            "ifconfig {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub fn exists(name: &str) -> bool {
    CString::new(name).is_ok_and(|c| unsafe { libc::if_nametoindex(c.as_ptr()) != 0 })
}

/// Copies `name` into the `ifreq` name field; the kernel needs it NUL-terminated.
fn set_ifname(ifr: &mut libc::ifreq, name: &str) -> Result<()> {
    ensure!(
        name.len() < ifr.ifr_name.len(),
        "interface name {name:?} is too long"
    );
    // Zero first so a name shorter than a previous one stays NUL-terminated.
    ifr.ifr_name = [0; 16];
    for (dst, src) in ifr.ifr_name.iter_mut().zip(name.as_bytes()) {
        *dst = *src as libc::c_char;
    }
    Ok(())
}

/// Destroys a pair left behind by a previous crash. Returns true if anything was removed.
pub fn remove_stale() -> bool {
    let mut removed = false;
    for name in [host_name(), peer_name()] {
        if exists(&name) && ifconfig(&[&name, "destroy"]).is_ok() {
            removed = true;
        }
    }
    removed
}

pub struct Feth {
    pub host: String,
    peer: String,
    bpf: Option<OwnedFd>,
    buf_len: usize,
    /// Self-pipe: [`Feth::interrupt`] wakes a reader blocked in [`Feth::read_batch`], so the
    /// transmit thread can sleep indefinitely instead of waking up to check for shutdown.
    wake_r: OwnedFd,
    wake_w: OwnedFd,
}

#[repr(C)]
struct BpfInsn {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct BpfProgram {
    len: libc::c_uint,
    insns: *const BpfInsn,
}

/// Kernel-side filter: pass only IPv4 and ARP frames to us, so everything else macOS emits
/// (IPv6, mDNS over v6, …) is dropped before it is ever copied to userspace.
const ETHERTYPE_FILTER: [BpfInsn; 5] = [
    BpfInsn {
        code: 0x28,
        jt: 0,
        jf: 0,
        k: 12,
    }, // ldh [12]        (ethertype)
    BpfInsn {
        code: 0x15,
        jt: 1,
        jf: 0,
        k: 0x0800,
    }, // jeq IPv4 → accept
    BpfInsn {
        code: 0x15,
        jt: 0,
        jf: 1,
        k: 0x0806,
    }, // jeq ARP  → accept, else reject
    BpfInsn {
        code: 0x06,
        jt: 0,
        jf: 0,
        k: u32::MAX,
    }, // accept: ret whole frame
    BpfInsn {
        code: 0x06,
        jt: 0,
        jf: 0,
        k: 0,
    }, // reject: ret 0
];

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&r, &w] {
        unsafe {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
    }
    Ok((r, w))
}

impl Feth {
    /// Creates the pair with `mac` on the host side (the address the phone sends to).
    pub fn create(mac: [u8; 6], mtu: u16) -> Result<Self> {
        if remove_stale() {
            log::info!("removed leftover interfaces from a previous run");
        }
        let (host, peer) = (host_name(), peer_name());
        ifconfig(&[&host, "create"])?;
        // From here on Drop destroys whatever exists.
        let (wake_r, wake_w) = match pipe() {
            Ok(p) => p,
            Err(e) => {
                let _ = ifconfig(&[&host, "destroy"]);
                return Err(e).context("create wake pipe");
            }
        };
        let mut f = Feth {
            host,
            peer,
            bpf: None,
            buf_len: BPF_BUF as usize,
            wake_r,
            wake_w,
        };
        ifconfig(&[&f.peer, "create"])?;
        ifconfig(&[&f.host, "peer", &f.peer])?;
        let mac = crate::net::fmt_mac(&mac);
        let mtu = mtu.to_string();
        ifconfig(&[&f.host, "lladdr", &mac, "mtu", &mtu])?;
        ifconfig(&[&f.peer, "mtu", &mtu, "up"])?;
        // The host side stays down until the data path is wired (see `bring_up`): macOS starts
        // DHCP the moment it goes up, and replies arriving before then would be lost.
        f.open_bpf()?;
        Ok(f)
    }

    fn open_bpf(&mut self) -> Result<()> {
        let fd = (0..256)
            .find_map(|i| {
                let path = CString::new(format!("/dev/bpf{i}")).ok()?;
                let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
                (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
            })
            .context("no free /dev/bpf device")?;
        let raw = fd.as_raw_fd();
        let one: libc::c_uint = 1;
        let zero: libc::c_uint = 0;
        let mut blen = BPF_BUF;
        let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
        set_ifname(&mut ifr, &self.peer)?;
        unsafe {
            // Buffer size must be set before binding the interface.
            if libc::ioctl(raw, libc::BIOCSBLEN, &mut blen) < 0 {
                return Err(io::Error::last_os_error()).context("BIOCSBLEN");
            }
            if libc::ioctl(raw, libc::BIOCSETIF, &ifr) < 0 {
                return Err(io::Error::last_os_error()).context("BIOCSETIF");
            }
            for (req, val, what) in [
                (libc::BIOCIMMEDIATE, &one, "BIOCIMMEDIATE"),
                (libc::BIOCSHDRCMPLT, &one, "BIOCSHDRCMPLT"),
                (BIOCSSEESENT, &zero, "BIOCSSEESENT"), // don't read back frames we inject
            ] {
                if libc::ioctl(raw, req, val) < 0 {
                    return Err(io::Error::last_os_error()).context(what);
                }
            }
            if libc::ioctl(raw, libc::BIOCPROMISC as libc::c_ulong) < 0 {
                return Err(io::Error::last_os_error()).context("BIOCPROMISC");
            }
            let prog = BpfProgram {
                len: ETHERTYPE_FILTER.len() as libc::c_uint,
                insns: ETHERTYPE_FILTER.as_ptr(),
            };
            if libc::ioctl(raw, libc::BIOCSETF, &prog) < 0 {
                return Err(io::Error::last_os_error()).context("BIOCSETF");
            }
        }
        // A zero-length buffer would make every read return 0 and spin the transmit thread.
        ensure!(
            blen as usize > 18,
            "BPF returned an unusable buffer size ({blen})"
        );
        self.buf_len = blen as usize;
        self.bpf = Some(fd);
        Ok(())
    }

    fn fd(&self) -> i32 {
        self.bpf.as_ref().map_or(-1, |f| f.as_raw_fd())
    }

    pub fn read_buf_len(&self) -> usize {
        self.buf_len
    }

    /// Delivers a frame from the phone to macOS (it appears as received on the host side).
    pub fn inject(&self, frame: &[u8]) -> io::Result<()> {
        let n = unsafe { libc::write(self.fd(), frame.as_ptr().cast(), frame.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n as usize != frame.len() {
            return Err(io::Error::other("short write to BPF"));
        }
        Ok(())
    }

    /// Brings the host side up; macOS starts DHCP on it immediately.
    pub fn bring_up(&self) -> Result<()> {
        ifconfig(&[&self.host, "up"])
    }

    /// Takes the host side down and up again, which makes macOS restart DHCP at once
    /// (used when DHCP fails and macOS falls back to a link-local address).
    pub fn bounce(&self) -> Result<()> {
        ifconfig(&[&self.host, "down"])?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        ifconfig(&[&self.host, "up"])
    }

    /// Wakes a thread blocked in [`Self::read_batch`] (async-signal-safe, never blocks).
    pub fn interrupt(&self) {
        let _ = unsafe { libc::write(self.wake_w.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
    }

    /// Blocks until macOS transmits frames, or [`Self::interrupt`] fires (returns false).
    /// Records each frame's position in `buf`, which must be [`Self::read_buf_len`] long.
    pub fn read_batch(&self, buf: &mut [u8], frames: &mut Vec<Range<usize>>) -> io::Result<bool> {
        frames.clear();
        let mut pfd = [
            libc::pollfd {
                fd: self.fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.wake_r.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(pfd.as_mut_ptr(), 2, -1) } < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted {
                Ok(true)
            } else {
                Err(e)
            };
        }
        if pfd[1].revents != 0 {
            // Drain the self-pipe, or the next poll would return immediately and spin.
            let mut sink = [0u8; 16];
            unsafe {
                libc::read(
                    self.wake_r.as_raw_fd(),
                    sink.as_mut_ptr().cast(),
                    sink.len(),
                )
            };
            return Ok(false);
        }
        if pfd[0].revents & (libc::POLLERR | libc::POLLNVAL | libc::POLLHUP) != 0 {
            return Err(io::Error::other("BPF device error (interface removed?)"));
        }
        let n = unsafe { libc::read(self.fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            return match e.kind() {
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => Ok(true),
                _ => Err(e),
            };
        }
        parse_bpf(&buf[..n as usize], frames);
        Ok(true)
    }
}

/// Splits a BPF read into frames: `bpf_hdr { timeval32 tstamp; u32 caplen; u32 datalen; u16 hdrlen; }`
/// plus data, each record padded to 4 bytes (BPF_WORDALIGN).
fn parse_bpf(buf: &[u8], frames: &mut Vec<Range<usize>>) {
    let mut off = 0;
    while let (Some(caplen), Some(hdrlen)) = (u32_at(buf, off + 8), u16_at(buf, off + 16)) {
        let (caplen, hdrlen) = (caplen as usize, hdrlen as usize);
        let start = off + hdrlen;
        let Some(end) = start.checked_add(caplen) else {
            break;
        };
        // A header shorter than bpf_hdr, or a record running past the read: truncated tail.
        if hdrlen < 18 || end > buf.len() {
            break;
        }
        frames.push(start..end);
        off = end.next_multiple_of(4);
    }
}

/// The `bpf_hdr` fields are native-endian, and a device-supplied `caplen` must never index
/// out of the buffer, so both readers return `None` instead of panicking.
fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(
        buf.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u16_at(buf: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(
        buf.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

impl Drop for Feth {
    fn drop(&mut self) {
        drop(self.bpf.take());
        // Destroying the host side removes its addresses and routes too.
        let _ = ifconfig(&[&self.host, "destroy"]);
        let _ = ifconfig(&[&self.peer, "destroy"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(data: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 18];
        r[8..12].copy_from_slice(&(data.len() as u32).to_ne_bytes());
        r[12..16].copy_from_slice(&(data.len() as u32).to_ne_bytes());
        r[16..18].copy_from_slice(&18u16.to_ne_bytes());
        r.extend_from_slice(data);
        r.resize(r.len().next_multiple_of(4), 0);
        r
    }

    #[test]
    fn splits_bpf_batches() {
        let mut buf = record(&[1; 60]);
        buf.extend(record(&[2; 61]));
        buf.extend(&[0xff; 7]); // truncated trailer is ignored
        let mut frames = Vec::new();
        parse_bpf(&buf, &mut frames);
        assert_eq!(frames.len(), 2);
        assert!(buf[frames[0].clone()].iter().all(|&b| b == 1));
        assert_eq!(buf[frames[1].clone()].len(), 61);
    }

    fn record_with(caplen: u32, hdrlen: u16, data: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 18];
        r[8..12].copy_from_slice(&caplen.to_ne_bytes());
        r[12..16].copy_from_slice(&caplen.to_ne_bytes());
        r[16..18].copy_from_slice(&hdrlen.to_ne_bytes());
        r.extend_from_slice(data);
        r.resize(r.len().next_multiple_of(4), 0);
        r
    }

    #[test]
    fn parse_bpf_rejects_bad_headers_without_panicking() {
        let mut frames = Vec::new();
        // A caplen that would overflow the offset arithmetic.
        parse_bpf(&record_with(u32::MAX, 18, &[0; 8]), &mut frames);
        assert!(frames.is_empty());
        // A caplen past the end of the read.
        parse_bpf(&record_with(4096, 18, &[0; 8]), &mut frames);
        assert!(frames.is_empty());
        // A header shorter than bpf_hdr.
        parse_bpf(&record_with(0, 4, &[0; 8]), &mut frames);
        assert!(frames.is_empty());
        // A record that is exactly a header.
        parse_bpf(&record_with(0, 18, &[]), &mut frames);
        assert_eq!(frames, vec![18..18]);
        // Empty input, and every truncated prefix of a valid batch.
        frames.clear();
        parse_bpf(&[], &mut frames);
        assert!(frames.is_empty());
        let good = record_with(4, 18, &[7; 4]);
        for n in 0..18 + 4 {
            let mut f = Vec::new();
            parse_bpf(&good[..n], &mut f);
            assert!(f.is_empty(), "prefix of {n} bytes produced a frame");
        }
    }

    #[test]
    fn parse_bpf_skips_padding_between_records() {
        // hdrlen 18 + caplen 3 = 21, padded to 24: the next record starts there.
        let mut buf = record_with(3, 18, &[1, 2, 3]);
        buf.extend(record_with(1, 18, &[9]));
        let mut frames = Vec::new();
        parse_bpf(&buf, &mut frames);
        assert_eq!(frames, vec![18..21, 42..43]);
        assert_eq!(&buf[frames[1].clone()], &[9]);
    }

    #[test]
    fn ethertype_filter_accepts_only_ipv4_and_arp() {
        let prog = ETHERTYPE_FILTER;
        assert_eq!(prog[0].code, 0x28, "first insn must load the ethertype");
        assert_eq!(prog[0].k, 12);
        assert_eq!(prog[1].k, 0x0800);
        assert_eq!(prog[2].k, 0x0806);
        // Every jump must land inside the program, never past its end.
        for (at, insn) in prog.iter().enumerate().take(3).skip(1) {
            assert_eq!(insn.code, 0x15);
            for target in [insn.jt, insn.jf] {
                let land = at + 1 + target as usize;
                assert!(
                    land < prog.len(),
                    "jump from {at} lands outside the program"
                );
            }
        }
        // IPv4 skips the ARP test to accept; ARP falls through, everything else rejects.
        assert_eq!((prog[1].jt, prog[1].jf), (1, 0));
        assert_eq!((prog[2].jt, prog[2].jf), (0, 1));
        assert_eq!(prog[3].code, 0x06);
        assert_eq!(prog[3].k, u32::MAX);
        assert_eq!(prog[4].code, 0x06);
        assert_eq!(prog[4].k, 0);
    }

    #[test]
    fn interface_names_fit_the_kernel_field() {
        assert_eq!(host_name(), "feth7070");
        assert_eq!(peer_name(), "feth7071");
        let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
        set_ifname(&mut ifr, &peer_name()).unwrap();
        let name: Vec<u8> = ifr.ifr_name.iter().map(|&c| c as u8).collect();
        assert_eq!(&name[..8], b"feth7071");
        assert_eq!(name[8], 0, "name must stay NUL-terminated");
        let too_long = "x".repeat(ifr.ifr_name.len());
        assert!(set_ifname(&mut ifr, &too_long).is_err());
    }
}
