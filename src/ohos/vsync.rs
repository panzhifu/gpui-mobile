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
//! "main-thread" is doing work in that sentence: the vsync callback arrives on a
//! vsync thread the platform picks, so [`vsync_trampoline`] only posts the frame
//! to [`super::main_thread`] and re-arms. Running it there instead keeps GPUI's
//! `App` on one thread — see that module for what happens when it doesn't.
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

    /// Run one frame: the platform tick (which drains GPUI's foreground queue) and
    /// the window's own `request_frame`.
    ///
    /// Only ever called from the UI thread, via [`super::main_thread`]. Re-checks
    /// `running` because a stop may have landed between the hop and this.
    fn run_frame(&self) {
        if !self.running.load(Ordering::Acquire) {
            return;
        }
        let cb = {
            let mut guard = self.frame_cb.lock();
            guard.take()
        };
        if let Some(mut cb) = cb {
            cb();
            let mut guard = self.frame_cb.lock();
            if guard.is_none() {
                *guard = Some(cb);
            }
        }
    }
}

/// The global driver singleton.
static DRIVER: OnceLock<VsyncDriver> = OnceLock::new();

/// Set once the first missing-hop warning is logged, so a startup gap can't flood
/// hilog at 60 Hz.
static HOP_MISS_WARNED: AtomicBool = AtomicBool::new(false);

/// Static trampoline invoked by the runtime every vsync.
///
/// This runs on a vsync thread chosen by the platform, **not** on the thread that
/// owns GPUI's `App` — so it never runs a frame here. It posts the frame to
/// [`super::main_thread`] and re-arms.
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

    // 1. Hop onto the UI thread to run the frame. The driver pointer travels as a
    //    `usize` so the closure stays `Send`.
    let raw = data as usize;
    if !super::main_thread::post(Box::new(move || {
        // SAFETY: `raw` is the `&VsyncDriver` the runtime handed us; the driver is
        // a 'static singleton and this closure runs on the thread that owns GPUI.
        unsafe { &*(raw as *const VsyncDriver) }.run_frame();
    })) && !HOP_MISS_WARNED.swap(true, Ordering::Relaxed)
    {
        // There is no hop until the first live ArkTS → native call. Frames are
        // dropped meanwhile, and vsync re-arms, so this is a gap of a few frames
        // rather than a dead app.
        log::warn!("vsync: no UI-thread hop yet; dropping frames until a native_* call lands");
    }

    // 2. Re-arm for the next frame. Asking for vsync is the one thing this thread
    //    is for.
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
