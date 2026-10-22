//! Frame scheduling via `OH_NativeVSync`.
//!
//! `OH_NativeVSync` delivers a callback aligned to the display refresh and is
//! the OHOS equivalent of Android's `Choreographer` / iOS's `CADisplayLink`.
//!
//! Frames are **demand-driven**: a callback is only requested once something
//! wants to be drawn, and the two flags below are the whole protocol.
//!
//! * `demand` — a frame is wanted. Raised by [`schedule`], which GPUI calls
//!   through `PlatformWindow::frame_waker` / `schedule_frame` when a window goes
//!   dirty, when it stays dirty after a frame, and via the dispatcher waker when
//!   a background task lands. `run_frame` takes it.
//! * `armed` — a `RequestFrame` is outstanding. Set by [`VsyncDriver::arm`]
//!   through `swap`, so a burst of demands costs one vsync rather than sixty.
//!   Retired at the *start* of `run_frame`, never in the trampoline: a demand
//!   raised while a frame is being drawn has to find `armed` false so it can
//!   schedule the next one instead of being swallowed by a flag that reads "a
//!   frame is already coming".
//!
//! Draining on demand is not a battery optimisation here, it is the only way to
//! keep the main thread responsive. Presenting goes
//! `eglSwapBuffers → ProducerSurface::RequestBuffer → binder WaitForCompletion`:
//! the buffer queue is the compositor's, and when another window covers ours —
//! which is exactly what `@ohos.file.picker` does — it stops being drained and
//! that call stops returning. Re-arming every vsync regardless turns that into a
//! main thread wedged inside `eglSwapBuffers`, and the watchdog kills the app as
//! `THREAD_BLOCK_6S` six seconds later.
//!
//! "main-thread" is doing work in the frame's own sentence: the vsync callback
//! arrives on a vsync thread the platform picks, so [`vsync_trampoline`] only
//! posts the frame to [`super::main_thread`]. Running it there instead keeps
//! GPUI's `App` on one thread — see that module for what happens when it doesn't.

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
    /// A frame is wanted.
    demand: AtomicBool,
    /// A `RequestFrame` is outstanding: its callback has not hopped to the UI
    /// thread and run yet.
    armed: AtomicBool,
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
            demand: AtomicBool::new(false),
            armed: AtomicBool::new(false),
        })
    }

    /// Ask the platform for one vsync, unless one is already outstanding.
    fn arm(&self) {
        if self.armed.swap(true, Ordering::AcqRel) {
            return;
        }
        let data = self as *const Self as *mut c_void;
        let status =
            unsafe { OH_NativeVSync_RequestFrame(self.vsync, Some(vsync_trampoline), data) };
        if status != 0 {
            log::warn!("OH_NativeVSync_RequestFrame returned {status}");
            self.armed.store(false, Ordering::Release);
            self.running.store(false, Ordering::Release);
        }
    }

    /// Raise demand for a frame and make sure a vsync is coming to serve it.
    fn schedule(&self) {
        if !self.running.load(Ordering::Acquire) {
            return;
        }
        self.demand.store(true, Ordering::Release);
        self.arm();
    }

    /// Hand the demand to one frame. False when nothing wants to be drawn.
    fn take_demand(&self) -> bool {
        self.demand.swap(false, Ordering::AcqRel)
    }

    /// Run one frame: the platform tick (which drains GPUI's foreground queue)
    /// and, if something asked for one, the window's `request_frame`.
    ///
    /// Only ever called from the UI thread, via [`super::main_thread`]. Re-checks
    /// `running` because a stop may have landed between the hop and this.
    fn run_frame(&self) {
        if !self.running.load(Ordering::Acquire) {
            return;
        }
        // This callback is the outstanding vsync, so retire it before drawing:
        // demand raised *during* the draw must be able to arm the next one.
        self.armed.store(false, Ordering::Release);

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

/// Static trampoline invoked by the runtime for the one frame we asked for.
///
/// This runs on a vsync thread chosen by the platform, **not** on the thread that
/// owns GPUI's `App` — so it never runs a frame here. It posts the frame to
/// [`super::main_thread`].
extern "C" fn vsync_trampoline(_timestamp: i64, data: *mut c_void) {
    if data.is_null() {
        return;
    }
    // SAFETY: `data` is `&VsyncDriver` passed in `arm`, and the driver is a
    // 'static singleton.
    let driver = unsafe { &*(data as *const VsyncDriver) };

    if !driver.running.load(Ordering::Acquire) {
        return;
    }

    // The driver pointer travels as a `usize` so the closure stays `Send`.
    let raw = data as usize;
    let posted = super::main_thread::post(Box::new(move || {
        // SAFETY: `raw` is the `&VsyncDriver` the runtime handed us; the driver is
        // a 'static singleton and this closure runs on the thread that owns GPUI.
        unsafe { &*(raw as *const VsyncDriver) }.run_frame();
    }));

    if posted {
        return;
    }

    if !HOP_MISS_WARNED.swap(true, Ordering::Relaxed) {
        log::warn!("vsync: no UI-thread hop yet; frames wait for a native_* call to land");
    }
    // There is no hop until the first live ArkTS → native call, so this frame did
    // not run and its demand is still outstanding. Keep asking for a vsync while
    // that is true — a wake costs nothing on its own, and it is `run_frame`'s
    // present that the queue can block on. Once the hop exists the next callback
    // runs the frame and the loop goes back to idling.
    driver.armed.store(false, Ordering::Release);
    driver.arm();
}

/// Start the frame loop. The first frame draws whether or not anything asked for
/// it, so the window is on screen before GPUI has had a chance to mark it dirty.
///
/// Idempotent: a second call replaces the callback and keeps the loop running.
pub fn start(frame_cb: Box<dyn FnMut() + Send>) {
    let driver = DRIVER.get_or_init(|| {
        VsyncDriver::create().unwrap_or_else(|| {
            // Degenerate driver: `running` stays false and `arm` becomes a no-op,
            // so no frames are ever scheduled.
            VsyncDriver {
                vsync: std::ptr::null_mut(),
                frame_cb: Mutex::new(None),
                running: AtomicBool::new(false),
                demand: AtomicBool::new(false),
                armed: AtomicBool::new(false),
            }
        })
    });

    if driver.vsync.is_null() {
        log::error!("vsync::start called but OH_NativeVSync is unavailable");
        return;
    }

    *driver.frame_cb.lock() = Some(frame_cb);
    driver.running.store(true, Ordering::Release);
    driver.armed.store(false, Ordering::Release);
    driver.demand.store(true, Ordering::Release);
    driver.arm();
    log::info!("vsync frame loop started");
}

/// Raise demand for the next frame.
///
/// Safe to call from any thread; a no-op before [`start`] and after [`stop`].
/// The second call in a row is free: the frame already armed serves it.
pub fn schedule() {
    if let Some(driver) = DRIVER.get() {
        driver.schedule();
    }
}

/// Take the demand on offer for this frame; call once per frame, from the frame
/// callback. False means the window is idle and must not be presented.
pub fn take_demand() -> bool {
    DRIVER.get().is_some_and(VsyncDriver::take_demand)
}

/// Stop the frame loop. Demand and the armed flag are left as they were:
/// [`start`] resets both.
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
