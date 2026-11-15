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
                    let _ = GlobalContext::default().handle_events(Some(Duration::from_secs(60)));
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

/// Allocates `count` bulk transfers with `size`-byte buffers on `ep`.
unsafe fn alloc(
    handle: *mut ffi::libusb_device_handle,
    ep: u8,
    count: usize,
    size: usize,
    cb: ffi::libusb_transfer_cb_fn,
    user_data: *mut c_void,
) -> Result<Transfers> {
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
            (*t).timeout = 0;
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

/// Cancels everything and waits for all callbacks. Returns false if they did not all finish.
fn drain(ts: &[*mut ffi::libusb_transfer], inflight: &AtomicUsize) -> bool {
    for &t in ts {
        unsafe { ffi::libusb_cancel_transfer(t) }; // NOT_FOUND for idle ones is fine
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while inflight.load(Ordering::Acquire) > 0 {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    true
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

extern "system" fn rx_done(t: *mut ffi::libusb_transfer) {
    unsafe {
        let sh = &*((*t).user_data as *const RxShared);
        match (*t).status {
            LIBUSB_TRANSFER_COMPLETED => {
                let data =
                    std::slice::from_raw_parts((*t).buffer, (*t).actual_length.max(0) as usize);
                (sh.on_data)(data);
            }
            // Cancelled: we're stopping. Overflow: a bigger-than-announced burst; drop it.
            LIBUSB_TRANSFER_CANCELLED | LIBUSB_TRANSFER_OVERFLOW => {}
            s => {
                if !sh.stopping.swap(true, Ordering::AcqRel) {
                    (sh.on_fatal)(status_name(s));
                }
            }
        }
        if !sh.stopping.load(Ordering::Acquire) && ffi::libusb_submit_transfer(t) == 0 {
            return; // still in flight
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
        let (ts, bufs) = unsafe { alloc(handle, ep, count, size, rx_done, ud)? };
        let ring = RxRing { shared, ts, bufs };
        for &t in &ring.ts {
            ring.shared.inflight.fetch_add(1, Ordering::AcqRel);
            let rc = unsafe { ffi::libusb_submit_transfer(t) };
            if rc != 0 {
                ring.shared.inflight.fetch_sub(1, Ordering::AcqRel);
                bail!("cannot queue USB receive transfer (libusb error {rc})");
            }
        }
        Ok(ring)
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
        let (ts, bufs) = unsafe { alloc(handle, ep, count, size, tx_done, ud)? };
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
