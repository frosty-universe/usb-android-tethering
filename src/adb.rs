//! Optional automation: if the phone has USB debugging on and `adb` is installed, switch USB
//! tethering on from the Mac, so plugging the phone in is all that's needed.
//! The phone asks once to trust this computer; after that it is fully automatic.
//!
//! Security: the daemon runs as root, but `adb` lives in user-writable places (Homebrew, the
//! Android SDK). It is therefore never run as root: it runs as the logged-in console user, with
//! that user's HOME (so their already-trusted adb keys are used), and only if the binary is
//! owned by root or that user and not writable by anyone else.

use log::{debug, info, warn};
use std::ffi::CStr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Who adb runs as.
struct RunAs {
    uid: u32,
    gid: u32,
    home: PathBuf,
}

/// The user logged in at the console (owner of /dev/console), if any.
fn console_user() -> Option<RunAs> {
    let uid = std::fs::metadata("/dev/console").ok()?.uid();
    if uid == 0 {
        return None; // nobody logged in (login window)
    }
    // The passwd entry may not fit in the first buffer; grow it on ERANGE.
    let mut buf = vec![0 as libc::c_char; 4096];
    loop {
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();
        let rc =
            unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut result) };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pw.pw_dir.is_null() {
            return None;
        }
        let home = unsafe { CStr::from_ptr(pw.pw_dir) }
            .to_string_lossy()
            .into_owned();
        return Some(RunAs {
            uid,
            gid: pw.pw_gid,
            home: home.into(),
        });
    }
}

/// True if `bin` is safe to execute on behalf of `user`: a regular file owned by root or the
/// user, and not writable by group or others.
fn trusted(bin: &Path, user: Option<&RunAs>) -> bool {
    let Ok(real) = bin.canonicalize() else {
        return false;
    };
    let Ok(meta) = std::fs::metadata(&real) else {
        return false;
    };
    let owner_ok = meta.uid() == 0 || user.is_some_and(|u| u.uid == meta.uid());
    meta.is_file() && owner_ok && meta.permissions().mode() & 0o022 == 0
}

/// Finds adb in PATH, Homebrew, or the console user's Android SDK.
pub fn find() -> Option<PathBuf> {
    let user = console_user();
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("adb")).collect())
        .unwrap_or_default();
    candidates.push("/opt/homebrew/bin/adb".into());
    candidates.push("/usr/local/bin/adb".into());
    if let Some(u) = &user {
        candidates.push(u.home.join("Library/Android/sdk/platform-tools/adb"));
    }
    candidates
        .into_iter()
        .find(|p| p.is_file() && trusted(p, user.as_ref()))
}

/// `adb get-state`, from a run that may also have printed daemon banners on either stream.
#[derive(Debug, PartialEq)]
enum State {
    Device,
    Unauthorized,
    Other,
}

fn parse_state(text: &str) -> State {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.iter().any(|l| l.contains("unauthorized")) {
        return State::Unauthorized;
    }
    if lines.contains(&"device") {
        return State::Device;
    }
    State::Other
}

fn adb(bin: &Path, serial: &str, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new(bin);
    cmd.arg("-s")
        .arg(serial)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if unsafe { libc::geteuid() } == 0 {
        // Never run a user-writable binary as root: drop to the console user. Root's
        // supplementary groups go too (`CommandExt::groups` is unstable; pre_exec is the only
        // stable hook, and it runs before exec, in the child).
        let user = console_user()?;
        let (uid, gid) = (user.uid, user.gid);
        cmd.env("HOME", &user.home);
        unsafe {
            cmd.pre_exec(move || {
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(gid) != 0
                    || libc::setuid(uid) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    } else if let Some(home) = std::env::var_os("HOME") {
        cmd.env("HOME", home);
    }
    let mut child = cmd.spawn().ok()?;
    // Bound the call: adb can hang while a phone is re-enumerating.
    for _ in 0..100 {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    if child.try_wait().ok()?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let out = child.wait_with_output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
}

/// Tries to turn on USB tethering on the phone with this serial. Returns true if requested.
/// `function` is the USB gadget function to select: `rndis` or `ncm`.
pub fn enable_tethering(serial: &str, function: &str) -> bool {
    let Some(bin) = find() else {
        debug!("no trusted adb found; automatic tethering switch unavailable");
        return false;
    };
    let state = adb(&bin, serial, &["get-state"]).map(|o| parse_state(&o));
    match state {
        Some(State::Device) => {}
        Some(State::Unauthorized) => {
            warn!(
                "allow USB debugging on the phone (tap \"Allow\" on the prompt) so tethering can switch on automatically"
            );
            return false;
        }
        other => {
            debug!("adb state for {serial}: {other:?}");
            return false;
        }
    }
    info!("switching on USB tethering on the phone via adb ({function})…");
    // Android 9+: selects the gadget function; the tethering service starts on it if it is the
    // function this phone tethers over.
    match adb(
        &bin,
        serial,
        &["shell", "svc", "usb", "setFunctions", function],
    ) {
        Some(out) if out.trim().is_empty() => true,
        Some(out) => {
            warn!("phone refused to switch on tethering: {}", out.trim());
            false
        }
        None => {
            // The phone drops the adb connection while switching USB modes; that's expected.
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_writable_or_foreign_binaries() {
        let dir = std::env::temp_dir().join(format!("uat-adb-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("adb");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        let me = RunAs {
            uid: unsafe { libc::getuid() },
            gid: 0,
            home: dir.clone(),
        };

        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            trusted(&bin, Some(&me)),
            "owned by the user, not writable by others"
        );

        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!trusted(&bin, Some(&me)), "world-writable must be rejected");

        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let stranger = RunAs {
            uid: me.uid.wrapping_add(4242),
            gid: 0,
            home: dir.clone(),
        };
        if me.uid != 0 {
            assert!(
                !trusted(&bin, Some(&stranger)),
                "owned by someone else must be rejected"
            );
        }
        assert!(!trusted(&dir.join("missing"), Some(&me)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_is_not_a_trusted_binary() {
        let dir = std::env::temp_dir().join(format!("uat-adb-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let me = RunAs {
            uid: unsafe { libc::getuid() },
            gid: 0,
            home: dir.clone(),
        };
        assert!(!trusted(&dir, Some(&me)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_state_through_adb_banners() {
        // The first adb run starts a daemon and prints banners on the same stream.
        let t = "* daemon not running; starting now at tcp:5037\n* daemon started successfully\ndevice\n";
        assert_eq!(parse_state(t), State::Device);
        assert_eq!(parse_state("device\n"), State::Device);
        assert_eq!(parse_state("unauthorized\n"), State::Unauthorized);
        assert_eq!(
            parse_state("error: device unauthorized.\n"),
            State::Unauthorized
        );
        assert_eq!(parse_state("offline\n"), State::Other);
        assert_eq!(parse_state(""), State::Other);
        assert_eq!(
            parse_state("error: no devices/emulators found\n"),
            State::Other
        );
    }
}
