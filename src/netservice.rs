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
use std::time::Duration;

/// Fixed ID so a leftover service is always recognisable as ours.
pub const SERVICE_ID: &str = "0A5B7E7E-7E7E-4A4A-8D8D-0000000A7070";
pub const SERVICE_NAME: &str = "USB Android Tethering";
const RETRY_DELAY: Duration = Duration::from_millis(300);

/// scutil reports failures on stdout and still exits 0, so its own wording is what we check.
/// "No such key" is deliberately absent: it is the expected answer when removing a link that
/// is already gone.
const FAILURES: [&str; 4] = [
    "preference changes abandoned",
    "Invalid argument",
    ": unknown,",
    "too many arguments",
];

fn check(text: &str) -> Result<()> {
    if let Some(f) = FAILURES.iter().find(|f| text.contains(**f)) {
        bail!("scutil: {f}: {}", text.trim());
    }
    Ok(())
}

fn scutil(args: &[&str], script: &str) -> Result<String> {
    let mut child = Command::new("/usr/sbin/scutil")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start scutil (needed to change macOS network settings)")?;
    let mut input = String::with_capacity(script.len() + 8);
    input.push_str(script);
    if !input.ends_with('\n') {
        input.push('\n');
    }
    input.push_str("quit\n");
    child
        .stdin
        .take()
        .context("scutil stdin")?
        .write_all(input.as_bytes())
        .context("send commands to scutil")?;
    let out = child.wait_with_output().context("wait for scutil")?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    check(&format!("{stdout}{stderr}"))?;
    anyhow::ensure!(out.status.success(), "scutil exited with {}", out.status);
    Ok(stdout)
}

fn prefs(script: &str) -> Result<String> {
    scutil(&["--prefs"], script)
}

/// IDs listed under a `list <dir>` path, with `prefix` removed and any deeper path dropped.
fn list_ids(text: &str, prefix: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.split_once("= ").map(|(_, p)| p.trim()))
        .filter_map(|p| p.strip_prefix(prefix))
        .filter(|id| !id.is_empty() && !id.contains('/'))
        .map(str::to_string)
        .collect()
}

fn sets() -> Result<Vec<String>> {
    Ok(list_ids(&prefs("list /Sets\n")?, "/Sets/")
        .into_iter()
        .map(|s| format!("/Sets/{s}"))
        .collect())
}

fn service_ids() -> Result<Vec<String>> {
    Ok(list_ids(
        &prefs("list /NetworkServices\n")?,
        "/NetworkServices/",
    ))
}

fn current_set() -> Result<String> {
    prefs("get /\nd.show\n")?
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("CurrentSet : ")
                .map(|s| s.trim().to_string())
        })
        .context(
            "macOS has no current network location; open System Settings → Network and try again",
        )
}

/// Set paths and interface names are pasted into scutil commands, so anything unexpected is
/// refused rather than escaped: macOS only ever produces `/Sets/<uuid>` and `feth<digits>`.
fn checked_set(path: &str) -> Result<&str> {
    if path.starts_with("/Sets/")
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_'))
    {
        Ok(path)
    } else {
        bail!("unexpected network location path from macOS: {path:?}")
    }
}

fn checked_ifname(ifname: &str) -> Result<&str> {
    if !ifname.is_empty() && ifname.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(ifname)
    } else {
        bail!("unexpected interface name: {ifname:?}")
    }
}

/// scutil only treats `"` and `\` specially inside a quoted value.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn valid_order_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn parse_service_order(text: &str) -> Vec<String> {
    let mut in_order = false;
    let mut out = Vec::new();
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with("ServiceOrder") {
            in_order = true;
        } else if in_order {
            if t.starts_with('}') {
                break;
            }
            if let Some((_, id)) = t.split_once(" : ") {
                let id = id.trim();
                if valid_order_id(id) {
                    out.push(id.to_string());
                }
            }
        }
    }
    out
}

fn service_order(set: &str) -> Result<Vec<String>> {
    let set = checked_set(set)?;
    Ok(parse_service_order(&prefs(&format!(
        "get {set}/Network/Global/IPv4\nd.show\n"
    ))?))
}

/// Script fragment that rewrites a set's ServiceOrder, keeping the dict's other keys.
fn set_order_script(set: &str, order: &[String]) -> String {
    let path = format!("{set}/Network/Global/IPv4");
    format!(
        "d.init\nget {path}\nd.add ServiceOrder * {}\nset {path}\n",
        order.join(" ")
    )
}

fn install_script(ifname: &str, set: &str, order: &[String]) -> String {
    let (id, name) = (SERVICE_ID, quote(SERVICE_NAME));
    format!(
        "d.init\n\
         d.add UserDefinedName {name}\n\
         set /NetworkServices/{id}\n\
         d.init\n\
         d.add DeviceName {}\n\
         d.add Hardware Ethernet\n\
         d.add Type Ethernet\n\
         d.add UserDefinedName {name}\n\
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
        quote(ifname),
        order = set_order_script(set, order),
    )
}

pub fn exists() -> bool {
    service_ids().is_ok_and(|ids| ids.iter().any(|id| id == SERVICE_ID))
}

fn install_once(ifname: &str, first: bool) -> Result<()> {
    let current = current_set()?;
    let set = checked_set(&current)?;
    let mut order = service_order(set)?;
    order.retain(|s| s != SERVICE_ID);
    if first {
        order.insert(0, SERVICE_ID.into());
    } else {
        order.push(SERVICE_ID.into());
    }
    prefs(&install_script(ifname, set, &order))?;
    Ok(())
}

/// Creates the service for `ifname`. `first` puts it at the top of the service order so
/// macOS (and VPN clients) prefer it over Wi-Fi.
pub fn install(ifname: &str, first: bool) -> Result<()> {
    let ifname = checked_ifname(ifname)?;
    if exists() {
        remove().context("remove the previous network service first")?;
    }
    let mut last = None;
    // A commit fails if System Settings (or another tool) changed the prefs concurrently.
    for _ in 0..3 {
        match install_once(ifname, first) {
            Ok(()) if exists() => return Ok(()),
            Ok(()) => {
                last = Some(anyhow::anyhow!(
                    "the service was not listed after the commit"
                ))
            }
            Err(e) => last = Some(e),
        }
        std::thread::sleep(RETRY_DELAY);
    }
    Err(last.expect("loop runs at least once")).context(
        "could not add the network service; open System Settings → Network, then try again",
    )
}

fn remove_once() -> Result<()> {
    let mut script = String::new();
    for set in sets()? {
        let set = checked_set(&set)?;
        // Reordering is best effort: an unreadable order must not stop the removal itself.
        if let Ok(order) = service_order(set)
            && order.iter().any(|s| s == SERVICE_ID)
        {
            let rest: Vec<String> = order.into_iter().filter(|s| s != SERVICE_ID).collect();
            script += &set_order_script(set, &rest);
        }
        script += &format!("remove {set}/Network/Service/{SERVICE_ID}\n");
    }
    script += &format!("remove /NetworkServices/{SERVICE_ID}\ncommit\napply\n");
    prefs(&script)?;
    Ok(())
}

/// Removes the service from every network location. Returns true if it existed.
pub fn remove() -> Result<bool> {
    if !exists() {
        return Ok(false);
    }
    let mut last = None;
    // Retried: the crash guard and `uninstall` may race, and a commit fails on stale prefs.
    for _ in 0..3 {
        match remove_once() {
            Ok(()) if !exists() => return Ok(true),
            Ok(()) => {
                last = Some(anyhow::anyhow!(
                    "the service is still listed after the commit"
                ))
            }
            Err(e) => last = Some(e),
        }
        std::thread::sleep(RETRY_DELAY);
    }
    Err(last.expect("loop runs at least once")).context(
        "could not remove the network service; close System Settings → Network and retry, \
         or run `sudo usb-android-tethering cleanup`",
    )
}

fn store_get(key: &str) -> String {
    scutil(&[], &format!("show {key}\n")).unwrap_or_default()
}

/// First IPv4 address under `key` in a `d.show` dump: either a scalar (`Router : 1.2.3.4`)
/// or the first entry of an array (`Addresses : <array> { 0 : 1.2.3.4 }`).
fn first_addr(text: &str, key: &str) -> Option<Ipv4Addr> {
    let mut lines = text.lines().map(str::trim);
    while let Some(l) = lines.next() {
        let Some(v) = l.strip_prefix(key).and_then(|r| r.strip_prefix(" : ")) else {
            continue;
        };
        if v.starts_with('<') {
            // Array: the entries that follow are "0 : a.b.c.d". Stop at the closing brace so a
            // later key (e.g. Router) can never be mistaken for an address.
            return lines
                .take_while(|l| !l.starts_with('}'))
                .filter_map(|l| l.split_once(" : "))
                .filter(|(k, _)| k.chars().all(|c| c.is_ascii_digit()))
                .find_map(|(_, a)| a.trim().parse().ok());
        }
        if let Ok(a) = v.parse() {
            return Some(a);
        }
    }
    None
}

/// Server addresses from a `d.show` dump of a DNS dictionary.
fn parse_dns(text: &str) -> Vec<Ipv4Addr> {
    let mut lines = text.lines().map(str::trim);
    if !lines.any(|l| l.starts_with("ServerAddresses")) {
        return Vec::new();
    }
    lines
        .take_while(|l| !l.starts_with('}'))
        .filter_map(|l| l.split_once(" : "))
        .filter(|(k, _)| k.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|(_, a)| a.trim().parse().ok())
        .collect()
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
    let dns = parse_dns(&store_get(&format!(
        "State:/Network/Service/{SERVICE_ID}/DNS"
    )));
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

    fn ip(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
        Ipv4Addr::new(a, b, c, d)
    }

    #[test]
    fn parses_scutil_arrays() {
        let t = "<dictionary> {\n  Addresses : <array> {\n    0 : 192.168.211.47\n  }\n  Router : 192.168.211.44\n}\n";
        assert_eq!(first_addr(t, "Addresses"), Some(ip(192, 168, 211, 47)));
        assert_eq!(first_addr(t, "Router"), Some(ip(192, 168, 211, 44)));
        assert_eq!(first_addr(t, "Missing"), None);
    }

    #[test]
    fn first_addr_does_not_leak_a_later_key() {
        // An empty array must not hand back the Router that follows it.
        let t = "<dictionary> {\n  Addresses : <array> {\n  }\n  Router : 10.0.0.1\n}\n";
        assert_eq!(first_addr(t, "Addresses"), None);
        // A non-address entry inside the array is skipped, the next real one is used.
        let t =
            "<dictionary> {\n  Addresses : <array> {\n    0 : garbage\n    1 : 10.0.0.2\n  }\n}\n";
        assert_eq!(first_addr(t, "Addresses"), Some(ip(10, 0, 0, 2)));
    }

    #[test]
    fn link_local_is_not_an_address() {
        // What macOS publishes when DHCP fails; `info()` must treat it as "not ready".
        let t = "<dictionary> {\n  Addresses : <array> {\n    0 : 169.254.217.186\n  }\n}\n";
        assert!(first_addr(t, "Addresses").unwrap().is_link_local());
    }

    #[test]
    fn parses_dns_servers() {
        let t = "<dictionary> {\n  DomainName : lan\n  ServerAddresses : <array> {\n    0 : 1.1.1.1\n    1 : 8.8.8.8\n  }\n  SearchDomains : <array> {\n    0 : 9.9.9.9\n  }\n}\n";
        assert_eq!(parse_dns(t), vec![ip(1, 1, 1, 1), ip(8, 8, 8, 8)]);
        assert!(parse_dns("<dictionary> {\n}\n").is_empty());
        assert!(parse_dns("").is_empty());
    }

    #[test]
    fn parses_service_order() {
        let t = "<dictionary> {\n  ServiceOrder : <array> {\n    0 : A\n    1 : B\n  }\n  Other : 1\n}\n";
        assert_eq!(parse_service_order(t), vec!["A", "B"]);
        let empty = "<dictionary> {\n  ServiceOrder : <array> {\n  }\n}\n";
        assert!(parse_service_order(empty).is_empty());
        assert!(parse_service_order("").is_empty());
    }

    #[test]
    fn parses_listed_paths() {
        let t = "  path [0] = /NetworkServices/AAA\n  path [1] = /NetworkServices/BBB\n";
        assert_eq!(list_ids(t, "/NetworkServices/"), vec!["AAA", "BBB"]);
        // Deeper paths and other prefixes are ignored.
        let t = "path [0] = /Sets/XYZ\npath [1] = /Sets/XYZ/Network/Service/A\n";
        assert_eq!(list_ids(t, "/Sets/"), vec!["XYZ"]);
    }

    #[test]
    fn order_script_keeps_other_keys() {
        let s = set_order_script("/Sets/X", &["A".into(), "B".into()]);
        assert_eq!(
            s,
            "d.init\nget /Sets/X/Network/Global/IPv4\nd.add ServiceOrder * A B\nset /Sets/X/Network/Global/IPv4\n"
        );
    }

    #[test]
    fn install_script_quotes_values_and_orders_the_service() {
        let s = install_script("feth7070", "/Sets/X", &[SERVICE_ID.into(), "A".into()]);
        assert!(s.contains("d.add DeviceName \"feth7070\"\n"));
        assert!(s.contains(&format!("d.add UserDefinedName \"{SERVICE_NAME}\"\n")));
        assert!(s.contains(&format!(
            "set /Sets/X/Network/Service/{SERVICE_ID} /NetworkServices/{SERVICE_ID}\n"
        )));
        assert!(s.contains(&format!("d.add ServiceOrder * {SERVICE_ID} A\n")));
        assert!(s.ends_with("commit\napply\n"));
    }

    #[test]
    fn quotes_are_escaped_for_scutil() {
        assert_eq!(quote("plain"), "\"plain\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn rejects_values_that_would_split_a_scutil_command() {
        for bad in [
            "feth 7070",
            "feth7070\nset /x",
            "feth7070\"",
            "",
            "/Sets/X/../Y",
        ] {
            assert!(checked_ifname(bad).is_err(), "accepted ifname {bad:?}");
        }
        assert_eq!(checked_ifname("feth7070").unwrap(), "feth7070");
        for bad in [
            "/Sets/X Y",
            "/Sets/X\nremove /NetworkServices/X",
            "/NetworkServices/X",
            "/Sets/../../etc",
            "",
        ] {
            assert!(checked_set(bad).is_err(), "accepted set path {bad:?}");
        }
        assert_eq!(
            checked_set("/Sets/6B650309-2EDD-491A-B9D5-EA2AD9719E6D").unwrap(),
            "/Sets/6B650309-2EDD-491A-B9D5-EA2AD9719E6D"
        );
    }

    #[test]
    fn recognises_scutil_failures() {
        for bad in [
            "Invalid argument\npreference changes abandoned\n",
            "d.add: too many arguments\n",
            "bogus: unknown, type \"help\" for command info\n",
            "preference changes abandoned\n",
        ] {
            assert!(check(bad).is_err(), "missed failure in {bad:?}");
        }
        // The expected answer when removing a link that is already gone is not a failure.
        check("  No such key\n<dictionary> {\n}\n").unwrap();
        check("<dictionary> {\n  CurrentSet : /Sets/X\n}\n").unwrap();
    }
}
