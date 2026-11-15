//! Crash guard: a detached helper that waits for the tether process to exit and removes the
//! network service and interface if it died without cleaning up (e.g. SIGKILL or a panic).
//! Without it a dead tether could stay first in the service order until the next start.

use crate::{state, tether};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn spawn() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.args(["__guard", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // New session: Ctrl+C in the terminal (or launchd stopping the job) doesn't reach it.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    if let Err(e) = cmd.spawn() {
        log::warn!("crash guard not started: {e}");
    }
}

/// Blocks until `pid` exits, using kqueue so the guard costs nothing while it waits.
fn wait_exit(pid: libc::pid_t) {
    if pid <= 0 {
        return;
    }
    unsafe {
        let kq = libc::kqueue();
        if kq >= 0 {
            let mut ev: libc::kevent = std::mem::zeroed();
            ev.ident = pid as usize;
            ev.filter = libc::EVFILT_PROC;
            ev.flags = libc::EV_ADD | libc::EV_ONESHOT;
            ev.fflags = libc::NOTE_EXIT;
            let rc = libc::kevent(kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null());
            // ESRCH: the process is already gone. Read errno before close() can change it.
            let errno = (rc < 0).then(|| std::io::Error::last_os_error().raw_os_error());
            let mut done = errno.flatten() == Some(libc::ESRCH);
            if rc == 0 {
                let mut out: libc::kevent = std::mem::zeroed();
                loop {
                    let n = libc::kevent(kq, std::ptr::null(), 0, &mut out, 1, std::ptr::null());
                    if n > 0 {
                        done = true;
                        break;
                    }
                    if n < 0
                        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    {
                        continue;
                    }
                    // The watch was lost: never assume the process died, poll instead.
                    break;
                }
            }
            libc::close(kq);
            if done {
                return;
            }
        }
        while alive(pid) {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

fn alive(pid: libc::pid_t) -> bool {
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub fn run(pid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    wait_exit(pid);
    // A newer instance (e.g. restarted by launchd) owns the network now: leave it alone.
    if state::read().is_some() {
        return;
    }
    tether::cleanup_leftovers();
    let _ = std::fs::remove_file(state::PATH);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn alive_sees_a_running_process_only() {
        assert!(alive(std::process::id() as libc::pid_t));
        assert!(!alive(1_000_000_000));
    }

    #[test]
    fn wait_exit_blocks_until_the_process_is_gone() {
        let mut child = Command::new("/bin/sleep")
            .arg("0.4")
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn /bin/sleep");
        let started = Instant::now();
        wait_exit(child.id() as libc::pid_t);
        let elapsed = started.elapsed();
        let _ = child.wait();
        assert!(
            elapsed >= Duration::from_millis(200),
            "returned after {elapsed:?}, before the process exited"
        );
    }

    #[test]
    fn wait_exit_returns_at_once_for_an_invalid_pid() {
        let started = Instant::now();
        wait_exit(0);
        wait_exit(-1);
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
