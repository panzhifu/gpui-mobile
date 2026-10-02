//! Frame scheduling via `OH_NativeVSync`.
//!
//! `OH_NativeVSync` delivers a callback aligned to the display refresh and is
//! the OHOS equivalent of Android's `Choreographer` / iOS's `CADisplayLink`.
//! The driver re-arms itself every frame, so as long as it is started it
//! provides a steady main-thread tick that:
//!
//! 1. drains the foreground task queue (`OhosDispatcher`), and
//! 2. invokes the window's `request_frame` callback (layout → paint → present).
//!
//! A second `request_frame` while one is already pending is a no-op, so the
//! waker installed on the dispatcher (see `napi.rs`) can safely call
//! [`VsyncDriver::request_frame`] to shorten latency.

#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use parking_lot::Mutex;

/// Opaque `OH_NativeVSync`.
#[repr(C)]
pub struct OhNativeVSync {
    _private: [u8; 0],
}

/// `OH_NativeVSync_FrameCallback`.
type FrameCallbackFn = extern "C" fn(timestamp: i64, data: *mut c_void);

extern "C" {
    fn OH_NativeVSync_Create(name: *const c_char, length: c_uint) -> *mut OhNativeVSync;
    fn OH_NativeVSync_Destroy(vsync: *mut OhNativeVSync);
    fn OH_NativeVSync_RequestFrame(
        vsync: *mut OhNativeVSync,
        callback: Option<FrameCallbackFn>,
        data: *mut c_void,
    ) -> c_int;
}

// ── driver ────────────────────────────────────────────────────────────────────

struct VsyncDriver {
    vsync: *mut OhNativeVSync,
    frame_cb: Mutex<Option<Box<dyn FnMut() + Send>>>,
    running: AtomicBool,
}

// SAFETY: the driver is created and driven exclusively on the OHOS main thread;
// `Send`/`Sync` are required only so it can live in a `OnceLock`.
unsafe impl Send for VsyncDriver {}
unsafe impl Sync for VsyncDriver {}

impl VsyncDriver {
    fn create() -> Option<Self> {
        let name = c"gpui-ohos-vsync";
        let vsync = unsafe { OH_NativeVSync_Create(name.as_ptr(), 0) };
        if vsync.is_null() {
            log::error!("OH_NativeVSync_Create returned null");
            return None;
        }
        Some(Self {
            vsync,
            frame_cb: Mutex::new(None),
            running: AtomicBool::new(false),
        })
    }

    fn request_frame(&self) {
        if !self.running.load(Ordering::Acquire) {
            return;
        }
        let data = self as *const Self as *mut c_void;
        let status =
            unsafe { OH_NativeVSync_RequestFrame(self.vsync, Some(vsync_trampoline), data) };
        if status != 0 {
            log::warn!("OH_NativeVSync_RequestFrame returned {status}");
            self.running.store(false, Ordering::Release);
        }
    }
}

/// The global driver singleton.
static DRIVER: OnceLock<VsyncDriver> = OnceLock::new();

/// Static trampoline invoked by the runtime every vsync.
extern "C" fn vsync_trampoline(_timestamp: i64, data: *mut c_void) {
    if data.is_null() {
        return;
    }
    // SAFETY: `data` is `&VsyncDriver` passed in `request_frame`, and the
    // driver is a 'static singleton.
    let driver = unsafe { &*(data as *const VsyncDriver) };

    if !driver.running.load(Ordering::Acquire) {
        return;
    }

    // 1. Run the frame callback (which pumps momentum + the platform tick).
    let cb = {
        let mut guard = driver.frame_cb.lock();
        guard.take()
    };
    if let Some(mut cb) = cb {
        cb();
        let mut guard = driver.frame_cb.lock();
        if guard.is_none() {
            *guard = Some(cb);
        }
    }

    // 2. Re-arm for the next frame.
    driver.request_frame();
}

/// Start the frame loop, invoking `frame_cb` once per vsync.
///
/// Idempotent: a second call replaces the callback and keeps the loop running.
pub fn start(frame_cb: Box<dyn FnMut() + Send>) {
    let driver = DRIVER.get_or_init(|| {
        VsyncDriver::create().unwrap_or_else(|| {
            // Degenerate driver: `running` stays false and `request_frame`
            // becomes a no-op, so no frames are ever scheduled.
            VsyncDriver {
                vsync: std::ptr::null_mut(),
                frame_cb: Mutex::new(None),
                running: AtomicBool::new(false),
            }
        })
    });

    if driver.vsync.is_null() {
        log::error!("vsync::start called but OH_NativeVSync is unavailable");
        return;
    }

    *driver.frame_cb.lock() = Some(frame_cb);
    driver.running.store(true, Ordering::Release);
    driver.request_frame();
    log::info!("vsync frame loop started");
}

/// Ask for an extra frame as soon as possible.
///
/// Safe to call from any thread; a no-op if the loop has not been started.
pub fn request_frame() {
    if let Some(driver) = DRIVER.get() {
        driver.request_frame();
    }
}

/// Stop the frame loop.
pub fn stop() {
    if let Some(driver) = DRIVER.get() {
        driver.running.store(false, Ordering::Release);
        log::info!("vsync frame loop stopped");
    }
}

/// Destroy the vsync handle (called on app teardown).
pub fn shutdown() {
    if let Some(driver) = DRIVER.get() {
        driver.running.store(false, Ordering::Release);
        if !driver.vsync.is_null() {
            unsafe { OH_NativeVSync_Destroy(driver.vsync) };
        }
    }
}
