//! Installs the tether as a launchd daemon so it runs automatically at boot and picks up
//! the phone whenever USB tethering is switched on.

use anyhow::{Context, Result, bail};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub const LABEL: &str = "com.usb-android-tethering";
pub const PLIST: &str = "/Library/LaunchDaemons/com.usb-android-tethering.plist";
pub const BIN: &str = "/usr/local/bin/usb-android-tethering";
pub const LOG: &str = "/var/log/usb-android-tethering.log";

fn launchctl(args: &[&str]) -> Result<()> {
    let out = Command::new("/bin/launchctl")
        .args(args)
        .output()
        .context("run launchctl")?;
    if !out.status.success() {
        let mut msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if msg.is_empty() {
            msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        bail!("launchctl {} failed: {msg}", args.join(" "));
    }
    Ok(())
}

fn is_loaded() -> bool {
    Command::new("/bin/launchctl")
        .args(["print", &format!("system/{LABEL}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Unloads the job and waits until launchd has really let go of it (the daemon's teardown
/// takes a moment; bootstrapping again before that fails with "Input/output error").
fn bootout() -> Result<()> {
    if !is_loaded() {
        return Ok(());
    }
    let _ = launchctl(&["bootout", &format!("system/{LABEL}")]);
    let deadline = Instant::now() + Duration::from_secs(15);
    while is_loaded() {
        if Instant::now() >= deadline {
            bail!(
                "the background service did not stop; run `sudo launchctl bootout system/{LABEL}` and try again"
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

fn bootstrap() -> Result<()> {
    let mut last = None;
    for _ in 0..5 {
        match launchctl(&["bootstrap", "system", PLIST]) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(last.expect("loop runs at least once"))
}

pub fn is_installed() -> bool {
    Path::new(PLIST).exists()
}

/// PID of the running daemon, if launchd has it running.
pub fn daemon_pid() -> Option<u32> {
    let out = Command::new("/bin/launchctl")
        .args(["print", &format!("system/{LABEL}")])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("pid = "))
        .and_then(|p| p.trim().parse().ok())
}

/// A directory the daemon's binary lives in must not be writable by non-root users, or they
/// could replace code that launchd runs as root.
fn safe_dir(uid: u32, mode: u32) -> bool {
    uid == 0 && mode & 0o022 == 0
}

fn check_dir(dir: &Path) -> Result<()> {
    let meta = std::fs::metadata(dir)
        .with_context(|| format!("inspect {} (the daemon's binary directory)", dir.display()))?;
    if !safe_dir(meta.uid(), meta.mode()) {
        bail!(
            "{} is writable by users other than root, so the daemon's binary could be replaced; \
             run: sudo chown root:wheel {} && sudo chmod 755 {}",
            dir.display(),
            dir.display(),
            dir.display()
        );
    }
    Ok(())
}

fn set_root_owned(path: &Path) -> Result<()> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .with_context(|| format!("path {}", path.display()))?;
    if unsafe { libc::chown(c.as_ptr(), 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("chown {} to root", path.display()));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("make {} executable", path.display()))
}

/// Copies the running executable to `BIN` via a temp name, so a daemon that launchd restarts
/// mid-install never sees a half-written file.
fn install_binary(exe: &Path) -> Result<()> {
    let dir = Path::new(BIN).parent().expect("BIN has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    check_dir(dir)?;
    let tmp = format!("{BIN}.new");
    std::fs::copy(exe, &tmp).with_context(|| format!("copy binary to {BIN}"))?;
    // Set owner and mode before the rename, so the file is never briefly unsafe or unusable.
    set_root_owned(Path::new(&tmp))?;
    std::fs::rename(&tmp, BIN).with_context(|| format!("install {BIN}"))?;
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.chars()
        // XML 1.0 forbids most control characters outright.
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .flat_map(|c| match c {
            '&' => "&amp;".chars().collect::<Vec<_>>(),
            '<' => "&lt;".chars().collect(),
            '>' => "&gt;".chars().collect(),
            c => vec![c],
        })
        .collect()
}

/// The launchd job. Split from [`install`] so it can be checked without touching the system.
fn plist(run_args: &[String], verbose: u8) -> String {
    let mut args = vec![
        BIN.to_string(),
        "run".into(),
        "--log-file".into(),
        LOG.into(),
    ];
    if verbose > 0 {
        args.push(format!("-{}", "v".repeat(verbose as usize)));
    }
    args.extend_from_slice(run_args);
    let args_xml: String = args
        .iter()
        .map(|a| format!("\t\t<string>{}</string>\n", xml_escape(a)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
{args_xml}	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ThrottleInterval</key>
	<integer>5</integer>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>StandardErrorPath</key>
	<string>{LOG}</string>
</dict>
</plist>
"#
    )
}

pub fn install(run_args: &[String], verbose: u8, first: bool) -> Result<()> {
    let exe = std::env::current_exe().context("locate own executable")?;
    if exe != Path::new(BIN) {
        install_binary(&exe)?;
    } else {
        check_dir(Path::new(BIN).parent().expect("BIN has a parent"))?;
    }
    set_root_owned(Path::new(BIN))?;
    let job = plist(run_args, verbose);
    let previous = std::fs::read(PLIST).ok();
    // Do everything that can fail before taking the old job down, so a failure here leaves a
    // working install running instead of a loaded-out daemon with a stale plist.
    crate::netservice::install(&crate::feth::host_name(), first)?;
    std::fs::write(PLIST, &job).with_context(|| format!("write {PLIST}"))?;
    bootout()?;
    if let Err(e) = bootstrap() {
        // Put the previous job definition back and try to restore it.
        match previous {
            Some(p) => {
                let _ = std::fs::write(PLIST, p);
                let _ = bootstrap();
            }
            None => {
                let _ = std::fs::remove_file(PLIST);
                let _ = crate::netservice::remove();
            }
        }
        return Err(e.context(format!(
            "start the background service (log: {LOG}); the previous state was restored"
        )));
    }
    Ok(())
}

pub fn uninstall(purge_logs: bool) -> Result<bool> {
    let was = is_installed() || is_loaded();
    bootout()?;
    if is_installed() {
        std::fs::remove_file(PLIST).with_context(|| format!("remove {PLIST}"))?;
    }
    // The daemon keeps the service while installed; remove it now that the plist is gone.
    crate::netservice::remove()
        .context("remove the network service from System Settings → Network")?;
    if Path::new(BIN).exists() {
        std::fs::remove_file(BIN).with_context(|| format!("remove {BIN}"))?;
    }
    if purge_logs {
        let _ = std::fs::remove_file(LOG);
        let _ = std::fs::remove_file(format!("{LOG}.1"));
    }
    Ok(was)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_plist_is_valid_and_escaped() {
        let args = vec!["--no-primary".to_string(), "a&b<c>d\"e".to_string()];
        let p = plist(&args, 2);
        assert!(p.starts_with("<?xml version=\"1.0\""));
        assert!(p.contains("<string>a&amp;b&lt;c&gt;d\"e</string>"));
        assert!(p.contains("<string>-vv</string>"));
        assert!(p.contains("<string>--no-primary</string>"));
        assert!(p.contains(&format!("<string>{BIN}</string>")));
        assert!(p.contains(&format!("<string>{LOG}</string>")));
        assert_eq!(p.matches("<key>Label</key>").count(), 1);

        // The real parser is the strongest check that launchd will accept it.
        let path = std::env::temp_dir().join(format!("uat-plist-{}.plist", std::process::id()));
        std::fs::write(&path, &p).unwrap();
        let out = Command::new("/usr/bin/plutil")
            .args(["-lint", path.to_str().unwrap()])
            .output()
            .expect("run plutil");
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "plutil rejected the plist: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(xml_escape("a\u{1}b\tc"), "ab\tc");
    }

    #[test]
    fn binary_directory_must_be_root_owned_and_not_shared() {
        assert!(safe_dir(0, 0o755));
        assert!(safe_dir(0, 0o700));
        // Group- or world-writable: anyone could swap the binary launchd runs as root.
        assert!(!safe_dir(0, 0o775));
        assert!(!safe_dir(0, 0o757));
        // Not root-owned.
        assert!(!safe_dir(501, 0o755));
    }
}
