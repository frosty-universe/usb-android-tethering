//! usb-android-tethering: Android USB tethering for macOS, entirely in userspace.
//!
//! The phone's RNDIS function is driven over libusb and presented to macOS as an ordinary
//! Ethernet interface (a `feth` pair) with its own network service, "USB Android Tethering",
//! so macOS, apps and VPN clients treat it like a wired network. The interface and service
//! exist only while the tether runs; a crash guard removes them if the process dies.

mod adb;
mod dhcp;
mod feth;
mod guard;
mod launchd;
mod logging;
mod ncm;
mod net;
mod netservice;
mod rndis;
mod state;
mod tether;
mod usbio;
mod usbnet;

use clap::{Args, Parser, Subcommand};
use log::{error, info, warn};
use serde_json::json;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::Ordering;

#[derive(Parser)]
#[command(
    name = "usb-android-tethering",
    version,
    about = "Android USB tethering for macOS — no kernel extension, works with VPNs",
    long_about = "Android USB tethering for macOS — no kernel extension, works with VPNs.\n\n\
        Plug in the phone, turn on USB tethering on it, and run:\n\n    \
        sudo usb-android-tethering\n\n\
        or install it once so it runs automatically in the background:\n\n    \
        sudo usb-android-tethering install",
    after_help = "Run without a command to connect now and keep reconnecting (same as `run`)."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// More detail: -v debug, -vv trace (per-packet)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Only show warnings and errors
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,

    /// Also append logs to this file (rotated at 5 MB)
    #[arg(long, global = true, value_name = "PATH")]
    log_file: Option<PathBuf>,
}

#[derive(Args, Clone, Default)]
struct RunArgs {
    /// Exit when the phone disconnects instead of waiting for it to come back
    #[arg(long)]
    once: bool,
    /// Add the phone after your other networks instead of before them (used only as a fallback)
    #[arg(long)]
    no_primary: bool,
    /// Don't switch on tethering through adb when the phone has USB debugging on
    #[arg(long)]
    no_adb: bool,
}

impl RunArgs {
    fn to_args(&self) -> Vec<String> {
        [
            (self.once, "--once"),
            (self.no_primary, "--no-primary"),
            (self.no_adb, "--no-adb"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, f)| f.to_string())
        .collect()
    }
}

/// `run --once --no-adb`, for the sudo hint after a root check fails.
fn command_line(sub: &str, a: &RunArgs) -> String {
    std::iter::once(sub.to_string())
        .chain(a.to_args())
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Subcommand)]
enum Cmd {
    /// Connect now and keep reconnecting; the default command (requires root)
    Run(RunArgs),
    /// Show whether the phone is connected and how traffic is flowing
    Status {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// List attached phones and whether they can tether
    Devices {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Test the phone link (USB handshake + DHCP) without changing any network settings
    Probe,
    /// Install as a background service: starts at boot, connects whenever the phone tethers (requires root)
    Install(RunArgs),
    /// Remove the background service (requires root)
    Uninstall {
        /// Also delete the log files
        #[arg(long)]
        purge: bool,
    },
    /// Remove the network service and interface left behind by a crash (requires root)
    Cleanup,
    #[command(name = "__guard", hide = true)]
    Guard { pid: u32 },
}

fn require_root(what: &str) -> bool {
    if unsafe { libc::geteuid() } == 0 {
        return true;
    }
    error!("`{what}` needs administrator rights — run: sudo usb-android-tethering {what}");
    false
}

fn verbosity(quiet: bool, verbose: u8) -> i8 {
    if quiet { -1 } else { verbose.min(2) as i8 }
}

fn install_signal_handler() {
    if let Err(e) = ctrlc::set_handler(|| {
        if tether::STOP.load(Ordering::Relaxed) {
            std::process::exit(130); // second Ctrl+C: leave now
        }
        info!("stopping… (restoring network settings)");
        tether::request_stop();
    }) {
        warn!("cannot install signal handler: {e}");
    }
}

fn cmd_run(a: &RunArgs) -> bool {
    if !require_root(&command_line("run", a)) {
        return false;
    }
    if let Some(v) = state::read().filter(|v| v["pid"] != std::process::id()) {
        error!(
            "already running (pid {}){}",
            v["pid"],
            if launchd::is_installed() {
                " as the background service; see `usb-android-tethering status`"
            } else {
                ""
            }
        );
        return false;
    }
    install_signal_handler();
    guard::spawn();
    let opts = tether::Options {
        once: a.once,
        probe: false,
        no_primary: a.no_primary,
        no_adb: a.no_adb,
    };
    let ok = tether::run(&opts);
    info!("stopped; network settings restored");
    ok
}

fn cmd_probe() -> bool {
    if let Some(v) = state::read() {
        error!(
            "a tether is already using the phone (pid {}); stop it first, then probe",
            v["pid"]
        );
        return false;
    }
    install_signal_handler();
    let ok = tether::run(&tether::Options {
        probe: true,
        once: true,
        ..Default::default()
    });
    if ok {
        info!("probe OK — the phone link works. Start it with: sudo usb-android-tethering");
    }
    ok
}

fn ago(secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let d = now.saturating_sub(secs);
    match d {
        0..60 => format!("{d}s"),
        60..3600 => format!("{}m {}s", d / 60, d % 60),
        _ => format!("{}h {}m", d / 3600, (d % 3600) / 60),
    }
}

fn mb(v: &serde_json::Value) -> String {
    format!("{:.1} MB", v.as_u64().unwrap_or(0) as f64 / 1e6)
}

fn cmd_status(as_json: bool) -> bool {
    let st = state::read();
    let primary = netservice::primary_interface();
    let usb = usbnet::find_all();
    let devices: Vec<String> = usb
        .as_ref()
        .map(|v| v.iter().map(|f| f.describe()).collect())
        .unwrap_or_default();
    // An enumeration failure is not "no phone": say so instead of implying the cable is out.
    let usb_error = usb.as_ref().err().map(|e| format!("{e:#}"));
    let installed = launchd::is_installed();
    if as_json {
        println!(
            "{}",
            json!({
                "service_installed": installed,
                "daemon_pid": launchd::daemon_pid(),
                "running": st,
                "primary_interface": primary,
                "tethering_devices": devices,
                "usb_error": usb_error,
            })
        );
        return usb_error.is_none();
    }
    println!(
        "Background service: {}",
        match (installed, launchd::daemon_pid()) {
            (true, Some(p)) => format!("installed, running (pid {p})"),
            (true, None) => "installed, not running".into(),
            (false, _) => "not installed (enable with: sudo usb-android-tethering install)".into(),
        }
    );
    match &st {
        None => println!("Tether:             not running"),
        Some(v) => {
            let phase = v["phase"].as_str().unwrap_or("?");
            println!(
                "Tether:             {phase} for {} (pid {})",
                ago(v["since"].as_u64().unwrap_or(0)),
                v["pid"]
            );
            if let Some(i) = v["interface"].as_str() {
                let is_primary = primary.as_deref() == Some(i);
                println!(
                    "Interface:          {i}{}",
                    if is_primary { " (primary network)" } else { "" }
                );
            }
            if let Some(l) = v["network"].as_object() {
                println!(
                    "Address:            {} via {}  DNS {}",
                    l["ip"].as_str().unwrap_or("?"),
                    l["gateway"].as_str().unwrap_or("?"),
                    l["dns"]
                        .as_array()
                        .map(|a| a
                            .iter()
                            .filter_map(|d| d.as_str())
                            .collect::<Vec<_>>()
                            .join(", "))
                        .unwrap_or_default()
                );
            }
            if let Some(t) = v["traffic"].as_object() {
                println!(
                    "Traffic:            ↓ {}  ↑ {}",
                    mb(&t["rx_bytes"]),
                    mb(&t["tx_bytes"])
                );
            }
        }
    }
    println!(
        "Primary network:    {}",
        primary.unwrap_or_else(|| "none".into())
    );
    if let Some(e) = usb_error {
        println!("Phone:              USB enumeration failed: {e}");
        return false;
    }
    if devices.is_empty() {
        println!("Phone:              no phone with USB tethering on");
        for h in usbnet::hints() {
            println!("                    {}", h.describe());
        }
    } else {
        devices
            .iter()
            .for_each(|d| println!("Phone:              {d}"));
    }
    true
}

fn cmd_devices(as_json: bool) -> bool {
    let found = match usbnet::find_all() {
        Ok(f) => f,
        Err(e) => {
            error!("USB enumeration failed: {e:#}");
            return false;
        }
    };
    let hints = usbnet::hints();
    if as_json {
        let mut list: Vec<_> = found
            .iter()
            .map(|f| json!({"vid": format!("{:04x}", f.vid), "pid": format!("{:04x}", f.pid), "state": "ready", "description": f.describe()}))
            .collect();
        list.extend(hints.iter().map(usbnet::Hint::to_json));
        println!("{}", serde_json::Value::Array(list));
        return true;
    }
    if found.is_empty() && hints.is_empty() {
        println!("No Android phone found. Connect it with a data cable (not charge-only).");
    }
    for f in &found {
        println!("✔ ready: {}", f.describe());
    }
    for h in &hints {
        println!("• {}", h.describe());
    }
    true
}

fn cmd_install(a: &RunArgs, verbose: u8, log_file: Option<&std::path::Path>) -> bool {
    if !require_root(&command_line("install", a)) {
        return false;
    }
    if a.once {
        error!(
            "`--once` does not work with the background service, which always waits for the phone; \
             use `sudo usb-android-tethering run --once` instead"
        );
        return false;
    }
    if log_file.is_some_and(|p| p != std::path::Path::new(launchd::LOG)) {
        warn!("the background service always logs to {}", launchd::LOG);
    }
    if let Some(v) = state::read().filter(|_| !launchd::is_installed()) {
        error!(
            "a foreground instance is running (pid {}); stop it with Ctrl+C, then run install again",
            v["pid"]
        );
        return false;
    }
    match launchd::install(&a.to_args(), verbose, !a.no_primary) {
        Ok(()) => {
            info!(
                "installed: {} (runs at boot, connects whenever the phone tethers)",
                launchd::BIN
            );
            info!(
                "\"{}\" was added to System Settings → Network",
                netservice::SERVICE_NAME
            );
            info!(
                "logs: {}   status: usb-android-tethering status",
                launchd::LOG
            );
            true
        }
        Err(e) => {
            error!("install failed: {e:#}");
            false
        }
    }
}

fn cmd_uninstall(purge: bool) -> bool {
    if !require_root("uninstall") {
        return false;
    }
    match launchd::uninstall(purge) {
        Ok(true) => {
            info!("background service removed; network settings restored");
            true
        }
        Ok(false) => {
            info!("background service was not installed");
            true
        }
        Err(e) => {
            error!("uninstall failed: {e:#}");
            false
        }
    }
}

fn cmd_cleanup() -> bool {
    if !require_root("cleanup") {
        return false;
    }
    if let Some(v) = state::read() {
        error!(
            "a tether is running (pid {}); stop it instead of cleaning up",
            v["pid"]
        );
        return false;
    }
    if tether::cleanup_leftovers() {
        info!("removed the leftover network service and interfaces");
    } else {
        info!("nothing to clean up");
    }
    let _ = std::fs::remove_file(state::PATH);
    true
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let verbosity = verbosity(cli.quiet, cli.verbose);
    if let Err(e) = logging::init(verbosity, cli.log_file.as_deref()) {
        eprintln!("error: {e:#}");
        return ExitCode::from(2);
    }
    let ok = match cli.cmd.unwrap_or(Cmd::Run(RunArgs::default())) {
        Cmd::Run(a) => cmd_run(&a),
        Cmd::Status { json } => cmd_status(json),
        Cmd::Devices { json } => cmd_devices(json),
        Cmd::Probe => cmd_probe(),
        Cmd::Install(a) => {
            if cli.quiet {
                warn!(
                    "the background service always logs at info level; `--quiet` only applies now"
                );
            }
            cmd_install(&a, verbosity.max(0) as u8, cli.log_file.as_deref())
        }
        Cmd::Uninstall { purge } => cmd_uninstall(purge),
        Cmd::Cleanup => cmd_cleanup(),
        Cmd::Guard { pid } => {
            guard::run(pid);
            true
        }
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
