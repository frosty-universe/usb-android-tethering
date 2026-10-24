//! Registers the virtual NIC as a real macOS network service ("USB Android Tethering"),
//! exactly like one added in System Settings → Network. macOS itself then runs DHCP on it,
//! installs routes and DNS, and ranks it by service order — so every app and VPN client that
//! uses the standard SystemConfiguration APIs sees an ordinary wired Ethernet network.
//!
//! The service only exists while the tether is running: it is removed on exit, by the crash
//! guard if the process dies, and at the next start if both were killed.

use anyhow::{Context, Result, bail};
use std::io::Write;
use std::net::Ipv4Addr;
use std::process::{Command, Stdio};

/// Fixed ID so a leftover service is always recognisable as ours.
pub const SERVICE_ID: &str = "0A5B7E7E-7E7E-4A4A-8D8D-0000000A7070";
pub const SERVICE_NAME: &str = "USB Android Tethering";

fn scutil(args: &[&str], script: &str) -> Result<String> {
    let mut child = Command::new("/usr/sbin/scutil")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run scutil")?;
    child
        .stdin
        .take()
        .context("scutil stdin")?
        .write_all(format!("{script}quit\n").as_bytes())?;
    let out = child.wait_with_output()?;
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    if text.contains("failed") || text.contains("rror") {
        bail!("scutil: {}", text.trim());
    }
    Ok(text)
}

fn prefs(script: &str) -> Result<String> {
    scutil(&["--prefs"], script)
}

fn sets() -> Result<Vec<String>> {
    Ok(prefs("list /Sets\n")?
        .lines()
        .filter_map(|l| l.split_once("= ").map(|(_, p)| p.trim().to_string()))
        .filter(|p| p.matches('/').count() == 2)
        .collect())
}

fn current_set() -> Result<String> {
    prefs("get /\nd.show\n")?
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("CurrentSet : ")
                .map(|s| s.trim().to_string())
        })
        .context("no current network location")
}

fn service_order(set: &str) -> Result<Vec<String>> {
    let text = prefs(&format!("get {set}/Network/Global/IPv4\nd.show\n")).unwrap_or_default();
    let mut in_order = false;
    let mut out = Vec::new();
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with("ServiceOrder") {
            in_order = true;
        } else if in_order && t == "}" {
            break;
        } else if in_order && let Some((_, id)) = t.split_once(" : ") {
            out.push(id.trim().to_string());
        }
    }
    Ok(out)
}

/// Script fragment that rewrites a set's ServiceOrder, keeping the dict's other keys.
fn set_order_script(set: &str, order: &[String]) -> String {
    let path = format!("{set}/Network/Global/IPv4");
    format!(
        "d.init\nget {path}\nd.add ServiceOrder * {}\nset {path}\n",
        order.join(" ")
    )
}

pub fn exists() -> bool {
    prefs("list /NetworkServices\n")
        .map(|t| t.contains(SERVICE_ID))
        .unwrap_or(false)
}

/// Creates the service for `ifname`. `first` puts it at the top of the service order so
/// macOS (and VPN clients) prefer it over Wi-Fi.
pub fn install(ifname: &str, first: bool) -> Result<()> {
    if exists() {
        remove()?;
    }
    let set = current_set()?;
    let mut order = service_order(&set)?;
    order.retain(|s| s != SERVICE_ID);
    if first {
        order.insert(0, SERVICE_ID.into());
    } else {
        order.push(SERVICE_ID.into());
    }
    let id = SERVICE_ID;
    let name = SERVICE_NAME;
    let script = format!(
        "d.init\n\
         d.add UserDefinedName \"{name}\"\n\
         set /NetworkServices/{id}\n\
         d.init\n\
         d.add DeviceName {ifname}\n\
         d.add Hardware Ethernet\n\
         d.add Type Ethernet\n\
         d.add UserDefinedName \"{name}\"\n\
         set /NetworkServices/{id}/Interface\n\
         d.init\n\
         d.add ConfigMethod DHCP\n\
         set /NetworkServices/{id}/IPv4\n\
         d.init\n\
         d.add FTPPassive # 1\n\
         set /NetworkServices/{id}/Proxies\n\
         set {set}/Network/Service/{id} /NetworkServices/{id}\n\
         {order}\
         commit\n\
         apply\n",
        order = set_order_script(&set, &order),
    );
    // A commit fails if System Settings (or another tool) changed the prefs concurrently.
    let mut last = None;
    for _ in 0..3 {
        match prefs(&script) {
            Ok(_) if exists() => return Ok(()),
            Ok(_) => last = Some(anyhow::anyhow!("service not present after commit")),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    Err(last.unwrap()).context("create network service")
}

/// Removes the service from every network location. Returns true if it existed.
pub fn remove() -> Result<bool> {
    if !exists() {
        return Ok(false);
    }
    // Retried: the crash guard and `uninstall` may race, and a commit fails on stale prefs.
    for _ in 0..3 {
        let id = SERVICE_ID;
        let mut script = String::new();
        for set in sets()? {
            let order = service_order(&set)?;
            if order.iter().any(|s| s == SERVICE_ID) {
                let rest: Vec<String> = order.into_iter().filter(|s| s != SERVICE_ID).collect();
                script += &set_order_script(&set, &rest);
            }
            script += &format!("remove {set}/Network/Service/{id}\n");
        }
        script += &format!("remove /NetworkServices/{id}\ncommit\napply\n");
        // "remove" of a missing link prints "No such key", which is harmless here.
        let _ = prefs(&script);
        if !exists() {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    bail!("network service could not be removed")
}

fn store_get(key: &str) -> String {
    scutil(&[], &format!("show {key}\n")).unwrap_or_default()
}

fn first_addr(text: &str, key: &str) -> Option<Ipv4Addr> {
    let mut lines = text.lines().map(str::trim);
    while let Some(l) = lines.next() {
        if let Some(v) = l.strip_prefix(&format!("{key} : ")) {
            if v.starts_with('<') {
                // array: next line is "0 : a.b.c.d"
                return lines.next()?.split_once(" : ")?.1.parse().ok();
            }
            return v.parse().ok();
        }
    }
    None
}

pub struct Info {
    pub ip: Ipv4Addr,
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
}

/// Address macOS configured on the service, once DHCP has completed (`None` while there is
/// no address or only a self-assigned link-local one).
pub fn info() -> Option<Info> {
    let v4 = store_get(&format!("State:/Network/Service/{SERVICE_ID}/IPv4"));
    let ip = first_addr(&v4, "Addresses")?;
    // 169.254/16 means DHCP failed and macOS self-assigned an address: not usable.
    if ip.is_link_local() {
        return None;
    }
    let dns_text = store_get(&format!("State:/Network/Service/{SERVICE_ID}/DNS"));
    let dns = dns_text
        .lines()
        .skip_while(|l| !l.contains("ServerAddresses"))
        .skip(1)
        .map_while(|l| l.trim().split_once(" : ").and_then(|(_, a)| a.parse().ok()))
        .collect();
    Some(Info {
        ip,
        router: first_addr(&v4, "Router"),
        dns,
    })
}

/// Current primary interface according to configd (what VPN clients follow).
pub fn primary_interface() -> Option<String> {
    store_get("State:/Network/Global/IPv4")
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("PrimaryInterface : ")
                .map(|s| s.trim().to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scutil_arrays() {
        let t = "<dictionary> {\n  Addresses : <array> {\n    0 : 192.168.211.47\n  }\n  Router : 192.168.211.44\n}\n";
        assert_eq!(
            first_addr(t, "Addresses"),
            Some(Ipv4Addr::new(192, 168, 211, 47))
        );
        assert_eq!(
            first_addr(t, "Router"),
            Some(Ipv4Addr::new(192, 168, 211, 44))
        );
        assert_eq!(first_addr(t, "Missing"), None);
    }

    #[test]
    fn link_local_is_not_an_address() {
        // What macOS publishes when DHCP fails; `info()` must treat it as "not ready".
        let t = "<dictionary> {\n  Addresses : <array> {\n    0 : 169.254.217.186\n  }\n}\n";
        assert!(first_addr(t, "Addresses").unwrap().is_link_local());
    }

    #[test]
    fn order_script_keeps_other_keys() {
        let s = set_order_script("/Sets/X", &["A".into(), "B".into()]);
        assert_eq!(
            s,
            "d.init\nget /Sets/X/Network/Global/IPv4\nd.add ServiceOrder * A B\nset /Sets/X/Network/Global/IPv4\n"
        );
    }
}
