//! libusb plumbing for the fast path:
//!
//! * one event thread for the whole process (async completions and hotplug);
//! * hotplug notifications, so waiting for a phone costs no CPU instead of polling;
//! * [`RxRing`]: several bulk-IN transfers always in flight, so the phone never waits for us;
//! * [`TxPool`]: bulk-OUT transfers submitted asynchronously, so sending is pipelined.
//!
//! Transfers are never freed while libusb owns them: shutdown cancels them and waits until
//! every completion callback has run (leaking instead of freeing if that ever times out).

use anyhow::{Context, Result, bail};
use rusb::ffi::{self, constants::*};
use rusb::{GlobalContext, Hotplug, HotplugBuilder, UsbContext};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, Once};
use std::time::{Duration, Instant};

// Event thread + hotplug

static CHANGES: Mutex<u64> = Mutex::new(0);
static CHANGED: Condvar = Condvar::new();

/// Wakes anyone blocked in [`wait_for_change`] (USB change or shutdown request).
pub fn wake() {
    *CHANGES.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    CHANGED.notify_all();
}

struct Notify;

impl Hotplug<GlobalContext> for Notify {
    fn device_arrived(&mut self, _: rusb::Device<GlobalContext>) {
        wake();
    }
    fn device_left(&mut self, _: rusb::Device<GlobalContext>) {
        wake();
    }
}

/// Starts the event thread and hotplug (idempotent). Returns whether hotplug is available.
pub fn start() -> bool {
    static ONCE: Once = Once::new();
    static HOTPLUG: AtomicBool = AtomicBool::new(false);
    ONCE.call_once(|| {
        std::thread::Builder::new()
            .name("usb-events".into())
            .spawn(|| {
                loop {
                    // Blocks until a completion or hotplug event; the timeout is only a backstop.
                    if GlobalContext::default()
                        .handle_events(Some(Duration::from_secs(60)))
                        .is_err()
                    {
                        // An error returns at once; without a pause this would spin a core.
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            })
            .map_err(|e| log::error!("cannot start USB event thread: {e}"))
            .ok();
        let ok = rusb::has_hotplug()
            && HotplugBuilder::new()
                .enumerate(false)
                .register(GlobalContext::default(), Box::new(Notify))
                // The registration lives for the whole process.
                .map(std::mem::forget)
                .is_ok();
        HOTPLUG.store(ok, Ordering::Release);
    });
    HOTPLUG.load(Ordering::Acquire)
}

/// Blocks until a USB device arrives or leaves, [`wake`] is called, or `timeout` passes.
pub fn wait_for_change(seen: &mut u64, timeout: Duration) {
    let guard = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
    let (guard, _) = CHANGED
        .wait_timeout_while(guard, timeout, |c| *c == *seen)
        .unwrap_or_else(|e| e.into_inner());
    *seen = *guard;
}

pub fn change_count() -> u64 {
    *CHANGES.lock().unwrap_or_else(|e| e.into_inner())
}

// Shared transfer bookkeeping

type Fatal = Box<dyn Fn(&str) + Send + Sync>;
type OnData = Box<dyn Fn(&[u8]) + Send + Sync>;
type Transfers = (Vec<*mut ffi::libusb_transfer>, Vec<Box<[u8]>>);

fn status_name(s: i32) -> &'static str {
    match s {
        LIBUSB_TRANSFER_ERROR => "transfer error",
        LIBUSB_TRANSFER_TIMED_OUT => "timed out",
        LIBUSB_TRANSFER_STALL => "endpoint stalled",
        LIBUSB_TRANSFER_NO_DEVICE => "device disconnected",
        LIBUSB_TRANSFER_OVERFLOW => "overflow",
        _ => "unknown error",
    }
}

/// Rejects a transfer geometry libusb cannot express (`length` is an `i32`).
fn check_geometry(count: usize, size: usize) -> Result<()> {
    anyhow::ensure!(
        count > 0 && size > 0 && size <= i32::MAX as usize,
        "invalid transfer geometry ({count} × {size} bytes)"
    );
    Ok(())
}

/// Receive transfers give up after this long. Without a timeout a phone that stops sending
/// leaves every transfer queued forever: no completion, no error, and no resubmission, so a
/// wedged endpoint is never recovered. The timeout only bounds how long that goes unnoticed.
/// Generous, because an idle link legitimately sends nothing for many seconds; libusb reports
/// `LIBUSB_TRANSFER_TIMED_OUT`, which [`rx_done`] treats as idle and requeues.
const RX_TIMEOUT_MS: u32 = 30_000;

/// Allocates `count` bulk transfers with `size`-byte buffers on `ep`. `timeout_ms` 0 = forever.
unsafe fn alloc(
    handle: *mut ffi::libusb_device_handle,
    ep: u8,
    count: usize,
    size: usize,
    timeout_ms: u32,
    cb: ffi::libusb_transfer_cb_fn,
    user_data: *mut c_void,
) -> Result<Transfers> {
    check_geometry(count, size)?;
    let mut ts = Vec::with_capacity(count);
    let mut bufs = Vec::with_capacity(count);
    for _ in 0..count {
        let t = unsafe { ffi::libusb_alloc_transfer(0) };
        if t.is_null() {
            ts.iter()
                .for_each(|&t| unsafe { ffi::libusb_free_transfer(t) });
            bail!("libusb_alloc_transfer failed");
        }
        let mut buf = vec![0u8; size].into_boxed_slice();
        unsafe {
            (*t).dev_handle = handle;
            (*t).endpoint = ep;
            (*t).transfer_type = LIBUSB_TRANSFER_TYPE_BULK;
            (*t).timeout = timeout_ms;
            (*t).flags = 0;
            (*t).length = size as i32;
            (*t).buffer = buf.as_mut_ptr();
            (*t).callback = cb;
            (*t).user_data = user_data;
            (*t).num_iso_packets = 0;
        }
        ts.push(t);
        bufs.push(buf);
    }
    Ok((ts, bufs))
}

/// Bytes to read out of a completed transfer. Clamped to the buffer: a slice past it is UB.
fn rx_len(actual: i32, capacity: i32) -> usize {
    actual.clamp(0, capacity.max(0)) as usize
}

/// Cancels everything and waits for all callbacks. Returns false if they did not all finish.
fn drain(ts: &[*mut ffi::libusb_transfer], inflight: &AtomicUsize) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        // Repeat the cancel: a receive callback can resubmit just after a single pass.
        for &t in ts {
            unsafe { ffi::libusb_cancel_transfer(t) }; // NOT_FOUND for idle ones is fine
        }
        if inflight.load(Ordering::Acquire) == 0 {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

struct RxShared {
    stopping: AtomicBool,
    inflight: AtomicUsize,
    on_data: OnData,
    on_fatal: Fatal,
}

/// Keeps `count` bulk-IN transfers queued; every completed buffer goes to `on_data` (called on
/// the USB event thread) and the transfer is resubmitted immediately.
pub struct RxRing {
    shared: Arc<RxShared>,
    ts: Vec<*mut ffi::libusb_transfer>,
    bufs: Vec<Box<[u8]>>,
}

// The raw transfer pointers are only touched by libusb and by Drop after draining.
unsafe impl Send for RxRing {}

/// What a completed receive transfer means for the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RxOutcome {
    /// Data (possibly none) arrived; deliver it and requeue.
    Data,
    /// Nothing usable but the link is fine (cancelled, oversized burst): drop and requeue.
    Requeue,
    /// A real error: report it and stop.
    Fail,
}

fn rx_outcome(status: i32) -> RxOutcome {
    match status {
        LIBUSB_TRANSFER_COMPLETED => RxOutcome::Data,
        // An idle link: the transfer expired with the phone silent. Requeue it, which is what
        // recovers a stalled endpoint. Any bytes that did arrive are dropped; RNDIS is
        // packet-framed and the parser discards a truncated tail, so nothing usable is lost.
        LIBUSB_TRANSFER_TIMED_OUT => RxOutcome::Requeue,
        LIBUSB_TRANSFER_CANCELLED | LIBUSB_TRANSFER_OVERFLOW => RxOutcome::Requeue,
        _ => RxOutcome::Fail,
    }
}

extern "system" fn rx_done(t: *mut ffi::libusb_transfer) {
    unsafe {
        let sh = &*((*t).user_data as *const RxShared);
        match rx_outcome((*t).status) {
            RxOutcome::Data => {
                let data = std::slice::from_raw_parts(
                    (*t).buffer,
                    rx_len((*t).actual_length, (*t).length),
                );
                (sh.on_data)(data);
            }
            RxOutcome::Requeue => {}
            RxOutcome::Fail => {
                if !sh.stopping.swap(true, Ordering::AcqRel) {
                    (sh.on_fatal)(status_name((*t).status));
                }
            }
        }
        if !sh.stopping.load(Ordering::Acquire) {
            let rc = ffi::libusb_submit_transfer(t);
            if rc == 0 {
                return; // still in flight
            }
            // A transfer the ring cannot requeue is one it never gets back: without this the
            // ring shrinks one buffer at a time until nothing is listening, and the link still
            // looks alive.
            if !sh.stopping.swap(true, Ordering::AcqRel) {
                (sh.on_fatal)(&format!(
                    "cannot requeue USB receive transfer (libusb error {rc})"
                ));
            }
        }
        sh.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl RxRing {
    pub fn start(
        handle: *mut ffi::libusb_device_handle,
        ep: u8,
        count: usize,
        size: usize,
        on_data: impl Fn(&[u8]) + Send + Sync + 'static,
        on_fatal: impl Fn(&str) + Send + Sync + 'static,
    ) -> Result<Self> {
        start();
        let shared = Arc::new(RxShared {
            stopping: AtomicBool::new(false),
            inflight: AtomicUsize::new(0),
            on_data: Box::new(on_data),
            on_fatal: Box::new(on_fatal),
        });
        let ud = Arc::as_ptr(&shared) as *mut c_void;
        let (ts, bufs) = unsafe { alloc(handle, ep, count, size, RX_TIMEOUT_MS, rx_done, ud)? };
        for (i, &t) in ts.iter().enumerate() {
            shared.inflight.fetch_add(1, Ordering::AcqRel);
            let rc = unsafe { ffi::libusb_submit_transfer(t) };
            if rc != 0 {
                shared.inflight.fetch_sub(1, Ordering::AcqRel);
                // Hand the ring to Drop so the transfers already submitted are cancelled and
                // every allocation (transfers and buffers) is released.
                let ring = RxRing { shared, ts, bufs };
                drop(ring);
                bail!("cannot queue USB receive transfer (libusb error {rc}, after {i} queued)");
            }
        }
        Ok(RxRing { shared, ts, bufs })
    }
}

impl Drop for RxRing {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        if drain(&self.ts, &self.shared.inflight) {
            self.ts
                .iter()
                .for_each(|&t| unsafe { ffi::libusb_free_transfer(t) });
        } else {
            // libusb still owns some transfers: leak rather than risk a use-after-free.
            log::error!("USB receive transfers did not finish; leaking them");
            std::mem::forget(std::mem::take(&mut self.bufs));
            std::mem::forget(self.shared.clone());
        }
    }
}

struct TxShared {
    stopping: AtomicBool,
    inflight: AtomicUsize,
    free: Mutex<Sender<usize>>,
    on_fatal: Fatal,
}

/// A pool of bulk-OUT transfers. [`TxPool::send`] fills a free one and submits it without
/// waiting for completion, so several transmissions overlap on the bus.
pub struct TxPool {
    shared: Arc<TxShared>,
    free: Receiver<usize>,
    ts: Vec<*mut ffi::libusb_transfer>,
    bufs: Vec<Box<[u8]>>,
    size: usize,
}

unsafe impl Send for TxPool {}

extern "system" fn tx_done(t: *mut ffi::libusb_transfer) {
    unsafe {
        let sh = &*((*t).user_data as *const TxShared);
        let s = (*t).status;
        if s != LIBUSB_TRANSFER_COMPLETED
            && s != LIBUSB_TRANSFER_CANCELLED
            && !sh.stopping.swap(true, Ordering::AcqRel)
        {
            (sh.on_fatal)(status_name(s));
        }
        let _ = sh
            .free
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(t as usize);
        sh.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl TxPool {
    pub fn new(
        handle: *mut ffi::libusb_device_handle,
        ep: u8,
        count: usize,
        size: usize,
        on_fatal: impl Fn(&str) + Send + Sync + 'static,
    ) -> Result<Self> {
        start();
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(TxShared {
            stopping: AtomicBool::new(false),
            inflight: AtomicUsize::new(0),
            free: Mutex::new(tx.clone()),
            on_fatal: Box::new(on_fatal),
        });
        let ud = Arc::as_ptr(&shared) as *mut c_void;
        let (ts, bufs) = unsafe { alloc(handle, ep, count, size, 0, tx_done, ud)? };
        for &t in &ts {
            let _ = tx.send(t as usize);
        }
        Ok(TxPool {
            shared,
            free: rx,
            ts,
            bufs,
            size,
        })
    }

    /// Takes a free transfer (waiting while all are busy), lets `fill` write up to `size`
    /// bytes into it, and submits it. `fill` returns the length to send (0 = nothing).
    pub fn send(&mut self, fill: impl FnOnce(&mut [u8]) -> usize) -> Result<()> {
        if self.shared.stopping.load(Ordering::Acquire) {
            bail!("USB link is down");
        }
        let t = self
            .free
            .recv_timeout(Duration::from_secs(5))
            .context("USB transmit stalled")? as *mut ffi::libusb_transfer;
        let buf = unsafe { std::slice::from_raw_parts_mut((*t).buffer, self.size) };
        let n = fill(buf).min(self.size);
        if n == 0 {
            let _ = self
                .shared
                .free
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .send(t as usize);
            return Ok(());
        }
        unsafe { (*t).length = n as i32 };
        self.shared.inflight.fetch_add(1, Ordering::AcqRel);
        let rc = unsafe { ffi::libusb_submit_transfer(t) };
        if rc != 0 {
            self.shared.inflight.fetch_sub(1, Ordering::AcqRel);
            let _ = self
                .shared
                .free
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .send(t as usize);
            bail!("USB transmit failed (libusb error {rc})");
        }
        Ok(())
    }
}

impl Drop for TxPool {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        if drain(&self.ts, &self.shared.inflight) {
            self.ts
                .iter()
                .for_each(|&t| unsafe { ffi::libusb_free_transfer(t) });
        } else {
            log::error!("USB transmit transfers did not finish; leaking them");
            std::mem::forget(std::mem::take(&mut self.bufs));
            std::mem::forget(self.shared.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rx_len_never_exceeds_the_buffer() {
        assert_eq!(rx_len(0, 1024), 0);
        assert_eq!(rx_len(512, 1024), 512);
        assert_eq!(rx_len(1024, 1024), 1024);
        assert_eq!(rx_len(1025, 1024), 1024, "over-report must be clamped");
        assert_eq!(rx_len(i32::MAX, 1024), 1024);
        assert_eq!(rx_len(-1, 1024), 0);
        assert_eq!(rx_len(8, -1), 0);
    }

    #[test]
    fn geometry_rejects_what_libusb_cannot_express() {
        assert!(check_geometry(8, 16384).is_ok());
        assert!(check_geometry(0, 16384).is_err());
        assert!(check_geometry(8, 0).is_err());
        assert!(check_geometry(1, i32::MAX as usize).is_ok());
        assert!(check_geometry(1, i32::MAX as usize + 1).is_err());
    }

    #[test]
    fn rx_outcome_requeues_on_an_idle_link() {
        // A phone that goes quiet must not kill the ring: the transfer is requeued.
        assert_eq!(rx_outcome(LIBUSB_TRANSFER_TIMED_OUT), RxOutcome::Requeue);
        assert_eq!(rx_outcome(LIBUSB_TRANSFER_CANCELLED), RxOutcome::Requeue);
        assert_eq!(rx_outcome(LIBUSB_TRANSFER_OVERFLOW), RxOutcome::Requeue);
        assert_eq!(rx_outcome(LIBUSB_TRANSFER_COMPLETED), RxOutcome::Data);
        assert_eq!(rx_outcome(LIBUSB_TRANSFER_NO_DEVICE), RxOutcome::Fail);
        assert_eq!(rx_outcome(LIBUSB_TRANSFER_STALL), RxOutcome::Fail);
        assert_eq!(rx_outcome(LIBUSB_TRANSFER_ERROR), RxOutcome::Fail);
    }

    #[test]
    fn wait_for_change_returns_early_on_wake() {
        let mut seen = change_count();
        let before = Instant::now();
        let woken = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            wake();
            wake();
        });
        wait_for_change(&mut seen, Duration::from_secs(10));
        woken.join().unwrap();
        assert!(
            before.elapsed() < Duration::from_secs(5),
            "did not wake early"
        );
        assert_eq!(seen, change_count());
    }

    #[test]
    fn wait_for_change_times_out_without_a_change() {
        let mut seen = change_count();
        let before = Instant::now();
        wait_for_change(&mut seen, Duration::from_millis(20));
        assert!(before.elapsed() >= Duration::from_millis(20));
        assert_eq!(seen, change_count());
    }
}
