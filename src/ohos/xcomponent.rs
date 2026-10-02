//! ArkUI `XComponent` native bridge.
//!
//! An OpenHarmony app embeds a native rendering surface via an ArkUI
//! `XComponent` of type `SURFACE`.  The ArkTS declaration names our native
//! module:
//!
//! ```ts
//! XComponent({ id: 'gpui', type: XComponentType.SURFACE,
//!              libraryname: 'gpui_mobile_example' })
//! ```
//!
//! After the module is loaded, the runtime hands us the `OH_NativeXComponent`
//! as the `__NATIVE_XCOMPONENT_OBJ__` property of the module's exports object
//! (see `napi.rs`).  We register a callback table with
//! `OH_NativeXComponent_RegisterCallback`; the runtime then calls:
//!
//! * `OnSurfaceCreated`   — a new `OHNativeWindow *` is available.
//! * `OnSurfaceChanged`   — the surface was resized.
//! * `OnSurfaceDestroyed` — the surface is going away.
//! * `DispatchTouchEvent` — a touch event arrived (query it with
//!   `OH_NativeXComponent_GetTouchEvent`).
//!
//! ## ABI note
//!
//! The struct layouts below must match `ace/xcomponent/native_interface_xcomponent.h`
//! for the target API version.  `OhosTouchEvent` deliberately declares only the
//! stable prefix (`id`, coordinates, type) followed by a generous reserved tail,
//! so that whatever `OH_NativeXComponent_GetTouchEvent` writes fits — the
//! platform writes *its* struct size, which is smaller than our buffer, and we
//! only read the stable prefix at the correct offsets.

#![allow(unsafe_code)]

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::OnceLock;

use parking_lot::Mutex;

// ── raw XComponent ABI ────────────────────────────────────────────────────────

/// Opaque `OH_NativeXComponent`.
#[repr(C)]
pub struct OhNativeXComponent {
    _private: [u8; 0],
}

/// `OH_NativeXComponent_TouchEventType`.
///
/// These values are translated to the 0/1/2/3 convention used by
/// `window.rs` (`0 = DOWN`, `1 = UP`, `2 = MOVE`, `3 = CANCEL`).
pub const TOUCH_DOWN: u32 = 0;
pub const TOUCH_UP: u32 = 1;
pub const TOUCH_MOVE: u32 = 2;
pub const TOUCH_CANCEL: u32 = 3;
pub const TOUCH_UNKNOWN: u32 = 4;

/// A single touch point.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OhNativeXComponentTouchPoint {
    pub id: i32,
    pub screen_x: f32,
    pub screen_y: f32,
    pub x: f32,
    pub y: f32,
    pub tool_type: i32,
}

/// `OH_NativeXComponent_TouchEvent`.
///
/// Only the prefix (`id`, `screenX/Y`, `x/y`, `type`) is guaranteed across API
/// versions; the rest is reserved so the C side never overflows our buffer.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OhosTouchEvent {
    pub id: i32,
    pub screen_x: f32,
    pub screen_y: f32,
    pub x: f32,
    pub y: f32,
    pub event_type: u32,
    /// Explicit padding to keep `size: f64` 8-byte aligned (matches C layout).
    pub _pad: u32,
    pub size: f64,
    pub force: f32,
    pub _pad2: u32,
    pub time_stamp: i64,
    /// Reserved: covers `touchPoints[10]` and `numPoints`.
    pub _reserved: [u8; 640],
}

impl Default for OhosTouchEvent {
    fn default() -> Self {
        // SAFETY: all-zero is a valid bit pattern for this POD struct.
        unsafe { std::mem::zeroed() }
    }
}

/// Surface-created / changed / destroyed callback.
type SurfaceCb = extern "C" fn(component: *mut OhNativeXComponent, window: *mut c_void);

/// `OH_NativeXComponent_OnSurfaceChangedCB`.
type SurfaceChangedCb =
    extern "C" fn(component: *mut OhNativeXComponent, window: *mut c_void, width: u64, height: u64);

/// `OH_NativeXComponent_DispatchTouchEventCB`.
type DispatchTouchCb = extern "C" fn(component: *mut OhNativeXComponent, window: *mut c_void);

/// `OH_NativeXComponent_OnFocusChangedCB`.
type FocusChangedCb = extern "C" fn(component: *mut OhNativeXComponent, is_focus: bool);

/// `OH_NativeXComponent_Callback`.
#[repr(C)]
struct OhNativeXComponentCallback {
    on_surface_created: Option<SurfaceCb>,
    on_surface_changed: Option<SurfaceChangedCb>,
    on_surface_destroyed: Option<SurfaceCb>,
    dispatch_touch_event: Option<DispatchTouchCb>,
    on_focus_changed: Option<FocusChangedCb>,
}

/// `OH_NativeXComponent_MouseEvent_Callback`.
#[repr(C)]
struct OhNativeXComponentMouseEventCallback {
    on_mouse_event: Option<extern "C" fn(*mut OhNativeXComponent, *mut c_void)>,
    on_hover_event: Option<extern "C" fn(*mut OhNativeXComponent, bool)>,
}

// ── OHOS XComponent C API ─────────────────────────────────────────────────────

/// `OH_NATIVEXCOMPONENT_RESULT_SUCCESS`.
pub const RESULT_SUCCESS: i32 = 0;

extern "C" {
    fn OH_NativeXComponent_RegisterCallback(
        component: *mut OhNativeXComponent,
        callback: *mut OhNativeXComponentCallback,
    ) -> i32;
    fn OH_NativeXComponent_RegisterMouseEventCallback(
        component: *mut OhNativeXComponent,
        callback: *mut OhNativeXComponentMouseEventCallback,
    ) -> i32;
    fn OH_NativeXComponent_GetXComponentSize(
        component: *mut OhNativeXComponent,
        window: *const c_void,
        width: *mut u64,
        height: *mut u64,
    ) -> i32;
    fn OH_NativeXComponent_GetXComponentId(
        component: *mut OhNativeXComponent,
        id: *mut c_char,
        size: *mut u64,
    ) -> i32;
    fn OH_NativeXComponent_GetTouchEvent(
        component: *mut OhNativeXComponent,
        window: *const c_void,
        touch_event: *mut OhosTouchEvent,
    ) -> i32;
}

// ── event handler ─────────────────────────────────────────────────────────────

/// Implemented by `napi.rs` to drive the platform / window.
pub trait XComponentHandler: Send {
    /// A new `OHNativeWindow *` is available.
    fn on_surface_created(&self, component: *mut OhNativeXComponent, window: *mut c_void);
    /// The surface was resized.
    fn on_surface_changed(
        &self,
        component: *mut OhNativeXComponent,
        window: *mut c_void,
        width: u64,
        height: u64,
    );
    /// The surface is being destroyed.
    fn on_surface_destroyed(&self, component: *mut OhNativeXComponent, window: *mut c_void);
    /// A touch event arrived.
    fn on_touch(
        &self,
        component: *mut OhNativeXComponent,
        window: *mut c_void,
        event: OhosTouchEvent,
    );
    /// Focus changed.
    fn on_focus_changed(&self, focused: bool);
}

static HANDLER: Mutex<Option<Box<dyn XComponentHandler>>> = Mutex::new(None);

/// The registered `OH_NativeXComponent *`, kept for size queries.
static COMPONENT: AtomicPtr<OhNativeXComponent> = AtomicPtr::new(std::ptr::null_mut());

/// The callback table handed to the runtime.  Must be `'static`.
static CALLBACKS: OnceLock<OhNativeXComponentCallback> = OnceLock::new();
static MOUSE_CALLBACKS: OnceLock<OhNativeXComponentMouseEventCallback> = OnceLock::new();

// ── static callbacks ──────────────────────────────────────────────────────────

extern "C" fn on_surface_created(component: *mut OhNativeXComponent, window: *mut c_void) {
    log::info!("XComponent OnSurfaceCreated window={:p}", window);
    COMPONENT.store(component, Ordering::Release);
    if let Some(h) = HANDLER.lock().as_ref() {
        h.on_surface_created(component, window);
    }
}

extern "C" fn on_surface_changed(
    component: *mut OhNativeXComponent,
    window: *mut c_void,
    width: u64,
    height: u64,
) {
    log::info!("XComponent OnSurfaceChanged {}×{}", width, height);
    if let Some(h) = HANDLER.lock().as_ref() {
        h.on_surface_changed(component, window, width, height);
    }
}

extern "C" fn on_surface_destroyed(component: *mut OhNativeXComponent, window: *mut c_void) {
    log::info!("XComponent OnSurfaceDestroyed window={:p}", window);
    if let Some(h) = HANDLER.lock().as_ref() {
        h.on_surface_destroyed(component, window);
    }
}

extern "C" fn dispatch_touch_event(component: *mut OhNativeXComponent, window: *mut c_void) {
    let mut event = OhosTouchEvent::default();
    let status = unsafe { OH_NativeXComponent_GetTouchEvent(component, window, &mut event) };
    if status != RESULT_SUCCESS {
        log::warn!("OH_NativeXComponent_GetTouchEvent failed: {status}");
        return;
    }
    if let Some(h) = HANDLER.lock().as_ref() {
        h.on_touch(component, window, event);
    }
}

extern "C" fn on_focus_changed(_component: *mut OhNativeXComponent, is_focus: bool) {
    if let Some(h) = HANDLER.lock().as_ref() {
        h.on_focus_changed(is_focus);
    }
}

// ── registration ──────────────────────────────────────────────────────────────

/// Install the event handler used by all XComponent callbacks.
///
/// Must be called before [`register`].
pub fn set_handler(handler: Box<dyn XComponentHandler>) {
    *HANDLER.lock() = Some(handler);
}

/// Register the surface / touch callbacks on `component`.
///
/// Returns the raw status from `OH_NativeXComponent_RegisterCallback`
/// (`0` = success).
pub fn register(component: *mut OhNativeXComponent) -> i32 {
    let callbacks = CALLBACKS.get_or_init(|| OhNativeXComponentCallback {
        on_surface_created: Some(on_surface_created),
        on_surface_changed: Some(on_surface_changed),
        on_surface_destroyed: Some(on_surface_destroyed),
        dispatch_touch_event: Some(dispatch_touch_event),
        on_focus_changed: Some(on_focus_changed),
    });
    COMPONENT.store(component, Ordering::Release);

    let status =
        unsafe { OH_NativeXComponent_RegisterCallback(component, callbacks as *const _ as *mut _) };
    log::info!("OH_NativeXComponent_RegisterCallback -> {status}");

    // Mouse events are optional — ignore failures on API levels without them.
    let mouse = MOUSE_CALLBACKS.get_or_init(|| OhNativeXComponentMouseEventCallback {
        on_mouse_event: None,
        on_hover_event: None,
    });
    let _ = unsafe {
        OH_NativeXComponent_RegisterMouseEventCallback(component, mouse as *const _ as *mut _)
    };

    status
}

/// Query the surface size in physical pixels for the given window handle.
pub fn surface_size(window: *mut c_void) -> Option<(u64, u64)> {
    let component = COMPONENT.load(Ordering::Acquire);
    if component.is_null() {
        return None;
    }
    let mut w: u64 = 0;
    let mut h: u64 = 0;
    let status = unsafe {
        OH_NativeXComponent_GetXComponentSize(
            component,
            window as *const c_void,
            &mut w as *mut u64,
            &mut h as *mut u64,
        )
    };
    if status == RESULT_SUCCESS && w > 0 && h > 0 {
        Some((w, h))
    } else {
        None
    }
}

/// Query the XComponent id string (useful for debugging).
pub fn component_id() -> Option<String> {
    let component = COMPONENT.load(Ordering::Acquire);
    if component.is_null() {
        return None;
    }
    let mut buf = [0 as c_char; 128];
    let mut size: u64 = buf.len() as u64;
    let status = unsafe {
        OH_NativeXComponent_GetXComponentId(component, buf.as_mut_ptr(), &mut size as *mut u64)
    };
    if status != RESULT_SUCCESS {
        return None;
    }
    let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    Some(cstr.to_string_lossy().into_owned())
}
