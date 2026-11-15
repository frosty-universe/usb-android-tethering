//! Leveled logger: timestamped lines on stderr (coloured on a terminal unless NO_COLOR is
//! set) and optionally appended to a file that is rotated at 5 MB.

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

const ROTATE_AT: u64 = 5 * 1024 * 1024;

struct Logger {
    level: LevelFilter,
    color: bool,
    to_stderr: bool,
    /// Log file, its path, and bytes written so far (tracked to avoid a stat() per line).
    file: Option<Mutex<(File, PathBuf, u64)>>,
    /// A broken log file is reported once, not on every line.
    file_error_reported: AtomicBool,
    /// Likewise for a log file that cannot be rotated and so grows without bound.
    rotate_error_reported: AtomicBool,
}

/// Keep one record per line and let no terminal control through: product strings and error
/// text from an attached phone reach the log unsanitised.
fn sanitize(msg: &str) -> Cow<'_, str> {
    if !msg.chars().any(|c| c.is_control() && c != '\t') {
        return Cow::Borrowed(msg);
    }
    let mut out = String::with_capacity(msg.len());
    for c in msg.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push('\t'),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

fn warn_once(flag: &AtomicBool, msg: std::fmt::Arguments<'_>) {
    if !flag.swap(true, Ordering::Relaxed) {
        eprintln!("warning: {msg}");
    }
}

/// Where a full log is moved: `x.log` → `x.log.1`, `x.txt` → `x.txt.1`.
fn rotated(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.1", path.display()))
}

fn timestamp() -> String {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::gettimeofday(&mut tv, std::ptr::null_mut());
        libc::localtime_r(&tv.tv_sec, &mut tm);
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        tv.tv_usec / 1000
    )
}

fn open_append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

impl Log for Logger {
    fn enabled(&self, m: &Metadata) -> bool {
        m.level() <= self.level && m.target().starts_with("usb_android_tethering")
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let ts = timestamp();
        let lvl = match r.level() {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        };
        let raw = r.args().to_string();
        let msg = sanitize(&raw);
        if self.to_stderr {
            let line = if self.color {
                let c = match r.level() {
                    Level::Error => "31;1",
                    Level::Warn => "33;1",
                    Level::Info => "32",
                    Level::Debug => "36",
                    Level::Trace => "90",
                };
                format!("\x1b[90m{ts}\x1b[0m \x1b[{c}m{lvl}\x1b[0m {msg}\n")
            } else {
                format!("{ts} {lvl} {msg}\n")
            };
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
        if let Some(f) = &self.file {
            let line = format!("{ts} {lvl} {msg}\n");
            // A panic elsewhere must never silence (or crash) logging: ignore poisoning.
            let mut g = f.lock().unwrap_or_else(|e| e.into_inner());
            if g.0.write_all(line.as_bytes()).is_ok() {
                g.2 += line.len() as u64;
                // Arm the once-only warning again, so a later failure is not silent.
                self.file_error_reported.store(false, Ordering::Relaxed);
            } else {
                warn_once(
                    &self.file_error_reported,
                    format_args!(
                        "cannot write log file {} — continuing without it",
                        g.1.display()
                    ),
                );
            }
            if g.2 > ROTATE_AT {
                let path = g.1.clone();
                let result =
                    std::fs::rename(&path, rotated(&path)).and_then(|()| open_append(&path));
                // Reset either way: a failed rotation must not be retried on every line.
                g.2 = 0;
                match result {
                    Ok(new) => {
                        g.0 = new;
                        self.rotate_error_reported.store(false, Ordering::Relaxed);
                    }
                    // Logging goes on through the old handle; only the rotation was missed.
                    Err(e) => warn_once(
                        &self.rotate_error_reported,
                        format_args!(
                            "cannot rotate log file {}: {e} — it keeps growing",
                            path.display()
                        ),
                    ),
                }
            }
        }
    }

    fn flush(&self) {}
}

/// Verbosity flag to a filter: -1 quiet (warnings+), 0 info, 1 debug, 2+ trace.
fn level_for(verbosity: i8) -> LevelFilter {
    match verbosity {
        i8::MIN..=-1 => LevelFilter::Warn,
        0 => LevelFilter::Info,
        1 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    }
}

/// Colour only on a terminal, and never when `NO_COLOR` is set to a non-empty value
/// (https://no-color.org).
fn use_color(stderr_tty: bool, no_color: bool) -> bool {
    stderr_tty && !no_color
}

/// `verbosity`: -1 quiet (warnings+), 0 info, 1 debug, 2+ trace.
pub fn init(verbosity: i8, file: Option<&Path>) -> anyhow::Result<()> {
    let level = level_for(verbosity);
    let stderr_tty = std::io::stderr().is_terminal();
    let file = match file {
        Some(p) => {
            let f = open_append(p)
                .map_err(|e| anyhow::anyhow!("cannot open log file {}: {e}", p.display()))?;
            let size = f.metadata().map(|m| m.len()).unwrap_or(0);
            Some(Mutex::new((f, p.to_path_buf(), size)))
        }
        None => None,
    };
    let logger = Logger {
        level,
        color: use_color(
            stderr_tty,
            std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
        ),
        // Under launchd (no terminal) with a log file, don't write every line twice.
        to_stderr: stderr_tty || file.is_none(),
        file,
        file_error_reported: AtomicBool::new(false),
        rotate_error_reported: AtomicBool::new(false),
    };
    log::set_boxed_logger(Box::new(logger)).map_err(|e| anyhow::anyhow!("{e}"))?;
    log::set_max_level(level);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logger(level: LevelFilter, file: Option<Mutex<(File, PathBuf, u64)>>) -> Logger {
        Logger {
            level,
            color: false,
            to_stderr: false,
            file,
            file_error_reported: AtomicBool::new(false),
            rotate_error_reported: AtomicBool::new(false),
        }
    }

    macro_rules! record {
        ($level:expr, $msg:expr) => {
            Record::builder()
                .level($level)
                .target("usb_android_tethering")
                .args(format_args!("{}", $msg))
                .build()
        };
    }

    #[test]
    fn verbosity_maps_to_a_level() {
        assert_eq!(level_for(-5), LevelFilter::Warn);
        assert_eq!(level_for(-1), LevelFilter::Warn);
        assert_eq!(level_for(0), LevelFilter::Info);
        assert_eq!(level_for(1), LevelFilter::Debug);
        assert_eq!(level_for(2), LevelFilter::Trace);
        assert_eq!(level_for(100), LevelFilter::Trace);
    }

    #[test]
    fn colour_needs_a_terminal_and_no_no_color() {
        assert!(use_color(true, false));
        assert!(!use_color(true, true), "NO_COLOR wins on a terminal");
        assert!(!use_color(false, false), "never colour a pipe");
    }

    #[test]
    fn rotation_name_only_appends_a_suffix() {
        assert_eq!(
            rotated(Path::new("/var/log/usb.log")),
            Path::new("/var/log/usb.log.1")
        );
        assert_eq!(
            rotated(Path::new("/tmp/daemon")),
            Path::new("/tmp/daemon.1")
        );
    }

    #[test]
    fn level_and_target_filtering() {
        let l = logger(LevelFilter::Info, None);
        let info = Metadata::builder()
            .level(Level::Info)
            .target("usb_android_tethering::tether")
            .build();
        let debug = Metadata::builder()
            .level(Level::Debug)
            .target("usb_android_tethering::tether")
            .build();
        let other = Metadata::builder()
            .level(Level::Error)
            .target("rusb")
            .build();
        assert!(l.enabled(&info));
        assert!(!l.enabled(&debug), "debug is below the info level");
        assert!(!l.enabled(&other), "other crates are not our log");
    }

    #[test]
    fn writes_lines_and_rotates_at_the_limit() {
        let path = std::env::temp_dir().join(format!("uat-log-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(rotated(&path));
        // Start already past the rotation limit so the next line rotates.
        let f = open_append(&path).unwrap();
        let l = logger(
            LevelFilter::Trace,
            Some(Mutex::new((f, path.clone(), ROTATE_AT + 1))),
        );
        l.log(&record!(Level::Info, "hello"));
        let kept = std::fs::read_to_string(rotated(&path)).unwrap();
        assert!(
            kept.contains("INFO  hello"),
            "old file kept as .log.1: {kept:?}"
        );
        let fresh = std::fs::read_to_string(&path).unwrap();
        assert!(fresh.is_empty(), "new file starts empty");
        // The next line lands in the new file, and the counter restarts, so rotation does not loop.
        l.log(&record!(Level::Info, "after"));
        let fresh = std::fs::read_to_string(&path).unwrap();
        assert!(fresh.contains("INFO  after"), "{fresh:?}");
        let counted = l.file.as_ref().unwrap().lock().unwrap().2;
        assert_eq!(counted, fresh.len() as u64, "counter tracks the new file");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(rotated(&path));
    }

    #[test]
    fn a_line_carries_timestamp_level_and_message() {
        let path = std::env::temp_dir().join(format!("uat-line-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let l = logger(
            LevelFilter::Trace,
            Some(Mutex::new((open_append(&path).unwrap(), path.clone(), 0))),
        );
        l.log(&record!(Level::Warn, "disk full"));
        // Below the limit: no rotation, file written and counter advanced.
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.len() > 20, "{body:?}");
        assert!(body.ends_with("WARN  disk full\n"), "{body:?}");
        assert_eq!(&body[4..5], "-");
        assert!(
            !rotated(&path).exists(),
            "under the limit nothing is rotated"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn messages_cannot_forge_lines_or_emit_escapes() {
        assert_eq!(sanitize("plain text"), "plain text");
        assert_eq!(sanitize("tab\tkept"), "tab\tkept");
        assert_eq!(sanitize("a\nb"), "a\\nb", "no forged log line");
        assert_eq!(sanitize("a\r\nb"), "a\\r\\nb");
        assert_eq!(sanitize("bell\u{7}"), "bell\\u{7}");
        assert_eq!(
            sanitize("\x1b[31mred"),
            "\\u{1b}[31mred",
            "no terminal escapes"
        );
    }

    #[test]
    fn a_rotation_that_fails_keeps_logging_and_says_so_once() {
        let path = std::env::temp_dir().join(format!("uat-rot-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(rotated(&path));
        // A directory at the .1 name makes every rename fail.
        std::fs::create_dir_all(rotated(&path)).unwrap();
        let l = logger(
            LevelFilter::Trace,
            Some(Mutex::new((
                open_append(&path).unwrap(),
                path.clone(),
                ROTATE_AT + 1,
            ))),
        );
        l.log(&record!(Level::Info, "one"));
        l.log(&record!(Level::Info, "two"));
        assert!(l.rotate_error_reported.load(Ordering::Relaxed));
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            body.matches("INFO ").count(),
            2,
            "logging went on: {body:?}"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(rotated(&path));
    }

    #[test]
    fn a_failing_file_is_skipped_not_crashed() {
        let path = std::env::temp_dir().join(format!("uat-ro-{}.log", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        // A read-only handle fails every write, like a full disk.
        let l = logger(
            LevelFilter::Trace,
            Some(Mutex::new((File::open(&path).unwrap(), path.clone(), 0))),
        );
        l.log(&record!(Level::Error, "first"));
        l.log(&record!(Level::Error, "second"));
        assert!(l.file_error_reported.load(Ordering::Relaxed));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_log_file_stays_plain_text() {
        let path = std::env::temp_dir().join(format!("uat-plain-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut l = logger(
            LevelFilter::Trace,
            Some(Mutex::new((open_append(&path).unwrap(), path.clone(), 0))),
        );
        l.color = true; // as it would be on a terminal
        l.log(&record!(Level::Error, "boom"));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            !body.contains('\u{1b}'),
            "no colour escapes in the file: {body:?}"
        );
        assert!(body.ends_with("ERROR boom\n"), "{body:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn timestamp_has_expected_shape() {
        let t = timestamp();
        // 2026-11-15 12:00:00.000
        assert_eq!(t.len(), 23, "{t}");
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[10..11], " ");
        assert_eq!(&t[19..20], ".");
    }
}
