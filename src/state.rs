//! Runtime status file shared between the running tether and `usb-android-tethering status`.

use crate::netservice::Info;
use crate::tether::Counters;
use crate::usbnet::{Found, Protocol};
use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

pub const PATH: &str = "/var/run/usb-android-tethering.json";

#[derive(Clone, Copy)]
pub enum Phase {
    Waiting,
    Connecting,
    Online,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Waiting => "waiting",
            Phase::Connecting => "connecting",
            Phase::Online => "online",
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Phase start time and the bytes last written successfully, so a repeated status costs one
/// `stat()` instead of a rewrite.
static LAST: Mutex<(&'static str, u64, String)> = Mutex::new(("", 0, String::new()));

/// Last write error, so a persistent failure is reported once instead of every poll.
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

pub fn write(
    phase: Phase,
    dev: Option<&Found>,
    iface: Option<&str>,
    net: Option<&Info>,
    c: Option<&Counters>,
) {
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let mut body = next_body(&mut last, phase.name(), now(), dev, iface, net, c);
    // Rewrite identical content if the file was removed behind us; `status` would otherwise
    // report no tether until the link changes.
    if body.is_none() && !std::path::Path::new(PATH).exists() {
        last.2.clear();
        body = next_body(&mut last, phase.name(), now(), dev, iface, net, c);
    }
    let Some(body) = body else {
        return;
    };
    // Best effort (only root writes /var/run). A silent failure would make `status` claim
    // nothing is running, so it is logged once.
    let result = store(std::path::Path::new(PATH), &body);
    let mut last_error = LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner());
    match result {
        Ok(()) => {
            last.2 = body;
            *last_error = None;
        }
        Err(e) => {
            let msg = e.to_string();
            if last_error.as_deref() != Some(&msg) {
                log::warn!("cannot write {PATH}: {msg}");
                *last_error = Some(msg);
            }
        }
    }
}

/// Replace `path` atomically: the temp file is created fresh (`create_new`) so a planted symlink
/// is refused, and the rename keeps readers from seeing a half-written document.
fn store(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| {
            // Explicit, because `status` runs without root and the writer's umask may hide it.
            f.set_permissions(std::fs::Permissions::from_mode(0o644))?;
            f.write_all(body.as_bytes())
        })
        .and_then(|()| std::fs::rename(&tmp, path))
}

/// The body to write, or `None` when it matches the last successful write. A phase change
/// restarts the clock stored in `last`; the body is only considered written once it is.
fn next_body(
    last: &mut (&'static str, u64, String),
    phase: &'static str,
    at: u64,
    dev: Option<&Found>,
    iface: Option<&str>,
    net: Option<&Info>,
    c: Option<&Counters>,
) -> Option<String> {
    if last.0 != phase {
        *last = (phase, at, String::new());
    }
    let body = status_json(phase, last.1, dev, iface, net, c).to_string();
    (body != last.2).then_some(body)
}

/// The status document. Split from [`write`] so it can be tested without touching disk.
fn status_json(
    phase: &str,
    since: u64,
    dev: Option<&Found>,
    iface: Option<&str>,
    net: Option<&Info>,
    c: Option<&Counters>,
) -> Value {
    json!({
        "pid": std::process::id(),
        "phase": phase,
        "since": since,
        "device": dev.map(|d| device_json(d.vid, d.pid, d.protocol)),
        "interface": iface,
        "network": net.map(|n| json!({
            "ip": n.ip.to_string(),
            "gateway": n.router.map(|r| r.to_string()),
            "dns": n.dns.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
        })),
        "traffic": c.map(|c| json!({
            "rx_bytes": c.rx_bytes.load(Ordering::Relaxed),
            "rx_packets": c.rx_packets.load(Ordering::Relaxed),
            "tx_bytes": c.tx_bytes.load(Ordering::Relaxed),
            "tx_packets": c.tx_packets.load(Ordering::Relaxed),
        })),
    })
}

/// Only the IDs and protocol: no product string, serial, or bus/port path, so the status file
/// stays a machine status rather than a device fingerprint.
fn device_json(vid: u16, pid: u16, protocol: Protocol) -> Value {
    json!({
        "vid": format!("{vid:04x}"),
        "pid": format!("{pid:04x}"),
        "protocol": protocol.name(),
    })
}

/// Remove the status file, but never one that belongs to another live instance.
pub fn clear() {
    if read().is_some_and(|v| v["pid"] == std::process::id()) {
        let _ = std::fs::remove_file(PATH);
    }
}

/// The status of a *running* instance, if any.
pub fn read() -> Option<Value> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(PATH).ok()?).ok()?;
    running_doc(&v).then_some(v)
}

/// True if this status document belongs to a live tether. The PID is chosen so that a huge
/// value cannot wrap into an unrelated live PID.
fn running_doc(v: &Value) -> bool {
    let Some(pid) = v["pid"]
        .as_u64()
        .and_then(|p| libc::pid_t::try_from(p).ok())
    else {
        return false;
    };
    is_tether_process(pid)
}

/// True if `pid` is alive *and* is this program — a stale file whose PID was reused by an
/// unrelated process after a crash or reboot must not count as "running". The basename check
/// (not the full path) is deliberate: a foreground run lives in `target/`, a service in
/// `/usr/local/bin`.
fn is_tether_process(pid: libc::pid_t) -> bool {
    if pid <= 0 {
        return false;
    }
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n > 0 {
        let exe = String::from_utf8_lossy(&buf[..n as usize]);
        return exe.rsplit('/').next() == Some(env!("CARGO_PKG_NAME"));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netservice::Info;
    use std::net::Ipv4Addr;
    fn counters(rx: u64) -> Counters {
        let c = Counters::default();
        c.rx_bytes.store(rx, Ordering::Relaxed);
        c.rx_packets.store(2, Ordering::Relaxed);
        c
    }

    #[test]
    fn online_document_shape() {
        let info = Info {
            ip: Ipv4Addr::new(192, 168, 211, 47),
            router: Some(Ipv4Addr::new(192, 168, 211, 44)),
            dns: vec![Ipv4Addr::new(1, 1, 1, 1)],
        };
        let c = counters(1500);
        let v = status_json(
            "online",
            1_700_000_000,
            None,
            Some("feth7070"),
            Some(&info),
            Some(&c),
        );
        assert_eq!(v["phase"], "online");
        assert_eq!(v["since"], 1_700_000_000);
        assert_eq!(v["interface"], "feth7070");
        assert_eq!(v["network"]["ip"], "192.168.211.47");
        assert_eq!(v["network"]["gateway"], "192.168.211.44");
        assert_eq!(v["network"]["dns"][0], "1.1.1.1");
        assert_eq!(v["traffic"]["rx_bytes"], 1500);
        assert_eq!(v["traffic"]["rx_packets"], 2);
        assert!(v["device"].is_null());
    }

    #[test]
    fn an_unchanged_status_is_not_written_again() {
        let mut last = ("", 0, String::new());
        let body = next_body(&mut last, "waiting", 100, None, None, None, None).unwrap();
        last.2 = body; // as a successful write does
        let later = next_body(&mut last, "waiting", 999, None, None, None, None);
        assert!(later.is_none(), "same content must cost no disk write");
        assert_eq!(
            last.1, 100,
            "the clock keeps the phase start, not the write time"
        );

        let c = counters(1);
        assert!(
            next_body(&mut last, "waiting", 999, None, None, None, Some(&c)).is_some(),
            "changing traffic is a real update"
        );
    }

    #[test]
    fn a_new_phase_restarts_the_clock() {
        let mut last = ("online", 100, "old".to_string());
        let body = next_body(&mut last, "connecting", 500, None, None, None, None).unwrap();
        assert_eq!(last.0, "connecting");
        assert_eq!(last.1, 500);
        assert!(body.contains("\"since\":500"), "{body}");
    }

    #[test]
    fn identical_input_gives_identical_bytes() {
        // Write-on-change compares bodies byte for byte, so key order must be stable.
        let info = Info {
            ip: Ipv4Addr::new(10, 0, 0, 2),
            router: None,
            dns: vec![],
        };
        let c = counters(7);
        let a = status_json("online", 5, None, Some("feth1"), Some(&info), Some(&c)).to_string();
        let b = status_json("online", 5, None, Some("feth1"), Some(&info), Some(&c)).to_string();
        assert_eq!(a, b);
    }

    #[test]
    fn missing_parts_are_null() {
        let v = status_json("waiting", 0, None, None, None, None);
        assert_eq!(v["phase"], "waiting");
        assert!(v["network"].is_null());
        assert!(v["traffic"].is_null());
    }

    #[test]
    fn device_document_has_ids_only() {
        let v = device_json(0x18d1, 0x4ee7, Protocol::Rndis);
        assert_eq!(v["vid"], "18d1");
        assert_eq!(v["pid"], "4ee7");
        assert_eq!(v["protocol"], "RNDIS");
        assert_eq!(v.as_object().unwrap().len(), 3, "no serial or product name");
        assert_eq!(
            device_json(1, 2, Protocol::Ncm)["protocol"],
            "CDC-NCM".to_string()
        );
    }

    #[test]
    fn phase_names_are_distinct() {
        assert_eq!(Phase::Waiting.name(), "waiting");
        assert_eq!(Phase::Connecting.name(), "connecting");
        assert_eq!(Phase::Online.name(), "online");
    }

    #[test]
    fn store_writes_readable_json_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("uat-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("status.json");
        store(&path, "{\"phase\":\"online\"}").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"phase\":\"online\"}"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644,
            "readable without root"
        );
        assert!(!dir.join("status.json.tmp").exists());
        // The temp file is removed first, so a symlink planted there cannot redirect the write.
        std::os::unix::fs::symlink(dir.join("victim"), dir.join("status.json.tmp")).unwrap();
        store(&path, "{}").unwrap();
        assert!(
            !dir.join("victim").exists(),
            "the planted link was not followed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_numeric_pid_of_a_live_tether_counts() {
        for doc in [
            json!({}),
            json!({"pid": "1234"}),
            json!({"pid": -1}),
            json!({"pid": u64::MAX}),
            json!({"pid": 0}),
        ] {
            assert!(!running_doc(&doc), "{doc}");
        }
    }

    #[test]
    fn a_reused_pid_is_not_a_running_tether() {
        // Our own PID is alive, but the executable is the test binary, not the tether.
        assert!(!is_tether_process(std::process::id() as libc::pid_t));
        assert!(!running_doc(&json!({"pid": std::process::id()})));
        assert!(!is_tether_process(-1));
        // A PID that cannot exist.
        assert!(!is_tether_process(1_000_000_000));
    }
}
