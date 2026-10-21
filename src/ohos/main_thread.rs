//! Getting work onto the ArkTS UI thread.
//!
//! `OH_NativeVSync` delivers its frame callback on a vsync thread chosen by the
//! platform — not on the thread that ran [`crate::ohos::napi::install`] and that
//! receives every XComponent callback. That second thread is where GPUI's `App`
//! lives, and the gap is not a performance problem but an abort:
//! `gpui-pre-scheduler` stamps every local task with the [`std::thread::ThreadId`]
//! that spawned it and asserts on each poll, so the first foreground task after
//! startup dies with "local task polled by a thread that didn't spawn it".
//!
//! So the vsync thread never runs GPUI work. It posts a closure into [`QUEUE`]
//! and rings a `napi_threadsafe_function`; the ring is delivered on the UI
//! thread's libuv loop, whose callback drains the queue. This is the shape the
//! Android arm already ships on real devices, where the Choreographer callback
//! only marks a frame due and `ALooper_wake`s the render thread.
//!
//! The hop is one function call and a queue push per frame.
//!
//! This module is compiled only for `target_env = "ohos"`.

#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

use parking_lot::Mutex;

use super::napi::{NapiEnv, NapiValue};

type NapiStatus = c_int;
const NAPI_OK: NapiStatus = 0;
/// `napi_tsfn_nonblocking`.
const TSFN_NONBLOCKING: NapiStatus = 0;

type BoxedTask = Box<dyn FnOnce() + Send>;

extern "C" {
    fn napi_create_string_utf8(
        env: NapiEnv,
        str_: *const c_char,
        length: usize,
        result: *mut NapiValue,
    ) -> NapiStatus;

    /// OHOS's declaration has **no** `finalize_hint` between `thread_finalize_cb`
    /// and `context` — Node's does. Passing Node's twelve arguments here shifts
    /// `call_js_cb` into `context`'s slot, which shows up as a ring that never
    /// fires rather than a compile error.
    fn napi_create_threadsafe_function(
        env: NapiEnv,
        func: NapiValue,
        async_resource: NapiValue,
        async_resource_name: NapiValue,
        max_queue_size: usize,
        initial_thread_count: usize,
        thread_finalize_data: *mut c_void,
        thread_finalize_cb: Option<extern "C" fn(NapiEnv, *mut c_void, *mut c_void)>,
        context: *mut c_void,
        call_js_cb: Option<extern "C" fn(NapiEnv, NapiValue, *mut c_void, *mut c_void)>,
        result: *mut *mut c_void,
    ) -> NapiStatus;

    fn napi_call_threadsafe_function(
        func: *mut c_void,
        data: *mut c_void,
        is_blocking: NapiStatus,
    ) -> NapiStatus;
}

/// Tasks waiting for the UI thread. Tasks stay here if a ring is dropped, so the
/// queue is the source of truth and the ring is only a wakeup.
static QUEUE: Mutex<Vec<BoxedTask>> = Mutex::new(Vec::new());

/// The `napi_threadsafe_function` handle, or null until [`init`] runs.
static TSFN: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Create the hop. **Must** be called on the UI thread that owns GPUI — the one
/// `napi::install` runs on — because that is the loop the ring is delivered on.
/// Idempotent.
pub unsafe fn init(env: NapiEnv) -> bool {
    if !TSFN.load(Ordering::Acquire).is_null() {
        return true;
    }

    let mut name: NapiValue = std::ptr::null_mut();
    let label = c"gpui-ohos-main-thread";
    if napi_create_string_utf8(env, label.as_ptr(), label.count_bytes(), &mut name) != NAPI_OK
        || name.is_null()
    {
        log::error!("main_thread::init: napi_create_string_utf8 failed");
        return false;
    }

    let mut handle: *mut c_void = std::ptr::null_mut();
    // `func` is null on purpose: the ring has no JS callback, it only schedules
    // `drain_on_ui_thread`. `max_queue_size` 0 means unbounded — the queue above
    // is where the work actually lives, so a full ring queue is not a thing here.
    let status = napi_create_threadsafe_function(
        env,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        name,
        0,
        1,
        std::ptr::null_mut(),
        None,
        std::ptr::null_mut(),
        Some(drain_on_ui_thread),
        &mut handle,
    );
    if status != NAPI_OK || handle.is_null() {
        log::error!("main_thread::init: napi_create_threadsafe_function status={status}");
        return false;
    }

    TSFN.store(handle, Ordering::Release);
    log::info!("main_thread: UI-thread hop ready");
    true
}

/// Whether [`init`] has run. Before that, [`post`] cannot deliver anything.
pub fn is_ready() -> bool {
    !TSFN.load(Ordering::Acquire).is_null()
}

/// Hand a task to the UI thread. Returns `false` when the hop was never
/// initialised, in which case the caller must not assume the task will run.
pub fn post(task: BoxedTask) -> bool {
    let tsfn = TSFN.load(Ordering::Acquire);
    if tsfn.is_null() {
        return false;
    }

    QUEUE.lock().push(task);
    let status =
        unsafe { napi_call_threadsafe_function(tsfn, std::ptr::null_mut(), TSFN_NONBLOCKING) };
    if status != NAPI_OK {
        // Non-blocking, so this only happens while the ring queue is full or the
        // env is closing. The task stays in `QUEUE` and rides out with the next
        // ring; vsync re-arms every frame, so the cost is at most one frame.
        log::debug!("main_thread::post: ring status={status}, task stays queued");
    }
    true
}

/// Runs on the UI thread, on the libuv loop of the env that created the hop.
extern "C" fn drain_on_ui_thread(
    _env: NapiEnv,
    _js_callback: NapiValue,
    _context: *mut c_void,
    _data: *mut c_void,
) {
    // Take the whole batch first: a task that posts another task (GPUI draining
    // its foreground queue does exactly that) must not re-enter this lock.
    let tasks: Vec<BoxedTask> = {
        let mut queue = QUEUE.lock();
        std::mem::take(&mut *queue)
    };
    for task in tasks {
        task();
    }
}
