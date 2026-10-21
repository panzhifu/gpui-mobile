//! NAPI entry point and ArkTS bridge for GPUI on HarmonyOS.
//!
//! This is the OHOS analogue of `android::jni` — the first Rust code that runs
//! and the glue between the ArkTS runtime and the GPUI platform.
//!
//! ## Module registration
//!
//! OpenHarmony loads a native module through the Node-API ABI.  A module
//! registers itself with `napi_module_register` from a shared-library
//! constructor (`.init_array`).  Because the module name must match the
//! `XComponent`'s `libraryname` (and therefore the consumer crate's `.so`
//! name), the registration lives in the **consumer** crate:
//!
//! ```ignore
//! #[cfg(target_env = "ohos")]
//! extern "C" fn ohos_init(env: gpui_mobile::ohos::napi::NapiEnv,
//!                         exports: gpui_mobile::ohos::napi::NapiValue)
//!                         -> gpui_mobile::ohos::napi::NapiValue {
//!     gpui_mobile::ohos::napi::install(env, exports, "gpui_mobile_example",
//!         Box::new(|cx| open_main_window(cx)));
//!     exports
//! }
//!
//! gpui_mobile::gpui_ohos_napi_module!("gpui_mobile_example", ohos_init);
//! ```
//!
//! ## XComponent binding
//!
//! When an `XComponent` names our module, the runtime exposes the
//! `OH_NativeXComponent` as the `__NATIVE_XCOMPONENT_OBJ__` property of the
//! module's `exports` object.  [`install`] unwraps it and registers the
//! surface / touch callbacks from [`xcomponent`](super::xcomponent), then
//! starts the GPUI `Application`.
//!
//! ## Data flow
//!
//! * Surface created  → `OhosPlatform::open_native_window` + finish-launching.
//! * vsync            → drain the dispatcher + `OhosWindow::request_frame`.
//! * ArkTS functions  → density / safe-area / lifecycle / keyboard bridge.

#![allow(unsafe_code)]

use parking_lot::Mutex;
use std::ffi::{c_char, c_double, c_int, c_void, CStr};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, OnceLock};

use gpui::{App, Application, ApplicationHandle};

use super::platform::{self, OhosPlatform, SharedPlatform};
use super::window::OhosNativeWindow;
use super::{main_thread, vsync, xcomponent};

// ── NAPI ABI ──────────────────────────────────────────────────────────────────

/// Opaque `napi_env`.
pub type NapiEnv = *mut c_void;
/// Opaque `napi_value`.
pub type NapiValue = *mut c_void;
/// Opaque `napi_callback_info`.
pub type NapiCallbackInfo = *mut c_void;
/// Opaque `napi_ref`.
pub type NapiRef = *mut c_void;

/// `napi_status`.
type NapiStatus = c_int;
const NAPI_OK: NapiStatus = 0;

/// `NAPI_MODULE_VERSION`.
const NAPI_MODULE_VERSION: c_int = 1;

/// A NAPI callback function.
pub type NapiRegisterFn = extern "C" fn(env: NapiEnv, exports: NapiValue) -> NapiValue;
/// A NAPI method callback.
pub type NapiCallback = extern "C" fn(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue;

extern "C" {
    fn napi_module_register(module: *mut NapiModule);

    fn napi_get_cb_info(
        env: NapiEnv,
        info: NapiCallbackInfo,
        argc: *mut usize,
        argv: *mut NapiValue,
        this_arg: *mut NapiValue,
        data: *mut *mut c_void,
    ) -> NapiStatus;
    fn napi_create_function(
        env: NapiEnv,
        utf8name: *const c_char,
        length: usize,
        cb: Option<NapiCallback>,
        data: *mut c_void,
        result: *mut NapiValue,
    ) -> NapiStatus;
    fn napi_set_named_property(
        env: NapiEnv,
        object: NapiValue,
        utf8name: *const c_char,
        value: NapiValue,
    ) -> NapiStatus;
    fn napi_get_named_property(
        env: NapiEnv,
        object: NapiValue,
        utf8name: *const c_char,
        result: *mut NapiValue,
    ) -> NapiStatus;
    fn napi_unwrap(env: NapiEnv, js_object: NapiValue, result: *mut *mut c_void) -> NapiStatus;
    fn napi_get_value_double(env: NapiEnv, value: NapiValue, result: *mut c_double) -> NapiStatus;
    fn napi_get_undefined(env: NapiEnv, result: *mut NapiValue) -> NapiStatus;
    fn napi_create_string_utf8(
        env: NapiEnv,
        str_: *const c_char,
        length: usize,
        result: *mut NapiValue,
    ) -> NapiStatus;
    fn napi_create_reference(
        env: NapiEnv,
        value: NapiValue,
        initial_refcount: u32,
        result: *mut NapiRef,
    ) -> NapiStatus;
    fn napi_get_reference_value(
        env: NapiEnv,
        reference: NapiRef,
        result: *mut NapiValue,
    ) -> NapiStatus;
    fn napi_call_function(
        env: NapiEnv,
        recv: NapiValue,
        func: NapiValue,
        argc: usize,
        argv: *const NapiValue,
        result: *mut NapiValue,
    ) -> NapiStatus;
    fn napi_typeof(env: NapiEnv, value: NapiValue, result: *mut c_int) -> NapiStatus;
}

/// `napi_valuetype::napi_function`.
const NAPI_FUNCTION: c_int = 6;

// ── NapiModule ────────────────────────────────────────────────────────────────

/// The `napi_module` descriptor registered with the runtime.
#[repr(C)]
pub struct NapiModule {
    pub nm_version: c_int,
    pub nm_flags: u32,
    pub nm_filename: *const c_char,
    pub nm_register_func: Option<NapiRegisterFn>,
    pub nm_modname: *const c_char,
    pub nm_priv: *mut c_void,
    pub reserved: [*mut c_void; 4],
}

impl NapiModule {
    /// Build a module descriptor.
    ///
    /// `name` must be a null-terminated C string matching the consumer crate's
    /// `.so` module name (e.g. `c"gpui_mobile_example"`).
    pub const fn new(name: &'static CStr, register: NapiRegisterFn) -> Self {
        Self {
            nm_version: NAPI_MODULE_VERSION,
            nm_flags: 0,
            nm_filename: c"entry.ets".as_ptr(),
            nm_register_func: Some(register),
            nm_modname: name.as_ptr(),
            nm_priv: std::ptr::null_mut(),
            reserved: [std::ptr::null_mut(); 4],
        }
    }
}

/// Register a module with the OHOS NAPI runtime.
///
/// # Safety
/// `module` must point to a `'static` `NapiModule` that outlives the process.
pub unsafe fn register_module(module: *mut NapiModule) {
    unsafe { napi_module_register(module) };
}

/// Declare the OHOS NAPI module (shared-library constructor + descriptor).
///
/// ```ignore
/// gpui_mobile::gpui_ohos_napi_module!("gpui_mobile_example", crate::ohos_init);
/// ```
#[macro_export]
macro_rules! gpui_ohos_napi_module {
    ($name:literal, $init:path) => {
        #[cfg(target_env = "ohos")]
        #[doc(hidden)]
        mod __gpui_ohos_napi_module {
            use $crate::ohos::napi::NapiModule;

            static mut MODULE: NapiModule = NapiModule::new(
                unsafe {
                    ::core::ffi::CStr::from_bytes_with_nul_unchecked(
                        concat!($name, "\0").as_bytes(),
                    )
                },
                $init,
            );

            extern "C" fn __gpui_napi_ctor() {
                unsafe {
                    $crate::ohos::napi::register_module(::core::ptr::addr_of_mut!(MODULE));
                }
            }

            #[used]
            #[unsafe(link_section = ".init_array")]
            static CTOR: extern "C" fn() = __gpui_napi_ctor;
        }
    };
}

// ── globals ───────────────────────────────────────────────────────────────────

static PLATFORM: OnceLock<Arc<OhosPlatform>> = OnceLock::new();
static APP_CB: Mutex<Option<Box<dyn FnOnce(&mut App) + Send>>> = Mutex::new(None);

/// GPUI's `ApplicationHandle`, kept alive for the process so the `App` outlives the
/// non-blocking `Platform::run` that created it.
///
/// It travels as a raw `Box` pointer because the handle is an `Rc` and therefore
/// neither `Send` nor `Sync`. Only the UI thread writes it, and the only read is the
/// null check that keeps the runtime's second module-init pass from starting a
/// second app.
static APP_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

static NAPI_ENV: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static KEYBOARD_REF: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Returns the global platform singleton.
///
/// # Panics
/// Panics if called before [`install`].
pub fn platform() -> Arc<OhosPlatform> {
    PLATFORM
        .get()
        .cloned()
        .expect("gpui-mobile OHOS platform not initialised (install() not called)")
}

/// Returns the platform singleton if it has been initialised.
pub fn try_platform() -> Option<Arc<OhosPlatform>> {
    PLATFORM.get().cloned()
}

// ── install ───────────────────────────────────────────────────────────────────

const XCOMPONENT_OBJ: &CStr = c"__NATIVE_XCOMPONENT_OBJ__";

/// Initialise the OHOS platform and register the NAPI exports.
///
/// Called from the consumer crate's NAPI init function.  `app_cb` is the
/// closure that opens the GPUI window (typically `open_main_window`); it is
/// invoked once the `XComponent` surface is ready.
///
/// # Safety
/// `env` / `exports` must be the values handed to the NAPI init function.
pub unsafe fn install(
    env: NapiEnv,
    exports: NapiValue,
    _module_name: &'static str,
    app_cb: Box<dyn FnOnce(&mut App) + Send>,
) {
    super::init_logger();
    NAPI_ENV.store(env, Ordering::Release);
    *APP_CB.lock() = Some(app_cb);

    // The UI-thread hop is deliberately **not** created here. This callback runs
    // while the module is loading, and a `napi_threadsafe_function` built from
    // that env is tied to an app context that is gone by the time the first frame
    // arrives: measured on the emulator, the ring reported `app was released`
    // ~95 times a second and nothing was ever drained. The hop is therefore
    // created from the first live ArkTS → native call (see
    // [`ensure_main_thread_hop`]).

    // 1. Register the plain NAPI functions ArkTS calls directly.
    register_function(env, exports, c"nativeInit", native_init);
    register_function(env, exports, c"nativeSetDensity", native_set_density);
    register_function(
        env,
        exports,
        c"nativeSetSafeAreaInsets",
        native_set_safe_area_insets,
    );
    register_function(env, exports, c"nativeOnForeground", native_on_foreground);
    register_function(env, exports, c"nativeOnBackground", native_on_background);
    register_function(env, exports, c"nativeOnDestroy", native_on_destroy);
    register_function(
        env,
        exports,
        c"nativeSetKeyboardCallback",
        native_set_keyboard_callback,
    );

    // 2. Create the platform singleton and start the (non-blocking) GPUI app —
    //    exactly once, and keep the handle.
    //
    //    Two things go wrong without this guard. The runtime calls the module init
    //    a second time when it binds the XComponent (`__NATIVE_XCOMPONENT_OBJ__`
    //    only exists on that pass), and a second `Application::run` overwrites the
    //    launch callback the first one is still waiting on — which drops the first
    //    `Rc<AppCell>`. And `Platform::run` returns immediately on OHOS, so the
    //    handle *is* the app's last strong reference: dropping it releases the App
    //    the moment the statement ends, and every later frame dies in
    //    `AsyncWindowContext::update_window` with "app was released". Android's
    //    host stores the same handle for the same reason.
    let platform = PLATFORM
        .get_or_init(|| Arc::new(OhosPlatform::new(false)))
        .clone();

    if APP_HANDLE.load(Ordering::Acquire).is_null() {
        let run_platform = SharedPlatform::new(platform.clone()).into_rc();
        let handle = Application::with_platform(run_platform).run_embedded(|cx: &mut App| {
            if let Some(cb) = APP_CB.lock().take() {
                log::info!("napi: invoking app callback");
                cb(cx);
            }
        });
        // The `Box` is deliberately never freed: the App lives for the process, and
        // OHOS has no ordered teardown to hand it back to.
        let boxed: Box<ApplicationHandle> = Box::new(handle);
        APP_HANDLE.store(Box::into_raw(boxed).cast(), Ordering::Release);
    } else {
        log::debug!("napi: the app is already running; not starting a second one");
    }

    // 3. Bind the XComponent, if the runtime provided one.
    if let Err(e) = bind_xcomponent(env, exports, platform) {
        log::warn!("napi: XComponent not bound in install(): {e}");
    }
}

fn bind_xcomponent(
    env: NapiEnv,
    exports: NapiValue,
    platform: Arc<OhosPlatform>,
) -> Result<(), String> {
    let mut obj: NapiValue = std::ptr::null_mut();
    let status = unsafe {
        napi_get_named_property(env, exports, XCOMPONENT_OBJ.as_ptr(), &mut obj as *mut _)
    };
    if status != NAPI_OK || obj.is_null() {
        return Err(format!(
            "napi_get_named_property(__NATIVE_XCOMPONENT_OBJ__) status={status}"
        ));
    }

    let mut raw: *mut c_void = std::ptr::null_mut();
    let status = unsafe { napi_unwrap(env, obj, &mut raw as *mut _) };
    if status != NAPI_OK || raw.is_null() {
        return Err(format!("napi_unwrap status={status}"));
    }

    // Install our handler *before* registering callbacks so no event is lost.
    xcomponent::set_handler(Box::new(GpuiXComponentHandler {
        platform: platform.clone(),
    }));
    let component = raw as *mut xcomponent::OhNativeXComponent;
    let status = xcomponent::register(component);
    if status != xcomponent::RESULT_SUCCESS {
        return Err(format!(
            "OH_NativeXComponent_RegisterCallback status={status}"
        ));
    }
    log::info!(
        "napi: XComponent bound (id={:?})",
        xcomponent::component_id()
    );
    Ok(())
}

// ── XComponent handler ────────────────────────────────────────────────────────

/// Drives the platform from XComponent / touch callbacks.
struct GpuiXComponentHandler {
    platform: Arc<OhosPlatform>,
}

impl xcomponent::XComponentHandler for GpuiXComponentHandler {
    fn on_surface_created(
        &self,
        _component: *mut xcomponent::OhNativeXComponent,
        window: *mut c_void,
    ) {
        let platform = &self.platform;

        let Some(native) = OhosNativeWindow::from_ptr(window) else {
            log::error!("on_surface_created: null OHNativeWindow");
            return;
        };

        let (w, h) = xcomponent::surface_size(window).unwrap_or((0, 0));
        let scale = super::density();
        let (w, h) = (w as i32, h as i32);

        if platform.window_count() == 0 {
            match platform.open_native_window(native, w, h, scale, false) {
                Ok(_window) => {
                    platform.update_primary_display(w, h, (scale * 160.0).round() as i32);

                    // Install the dispatcher waker and start the vsync loop.
                    platform
                        .dispatcher()
                        .set_main_thread_waker(|| vsync::request_frame());

                    let p = platform.clone();
                    vsync::start(Box::new(move || {
                        p.tick();
                        if let Some(win) = p.primary_window() {
                            win.request_frame();
                        }
                    }));
                }
                Err(e) => {
                    log::error!("on_surface_created: open_native_window failed: {e:#}");
                    return;
                }
            }
        } else if let Some(win) = platform.primary_window() {
            let _ = win.init_window(native, platform.gpu_context(), w, h);
        }

        // Window exists — let GPUI open its first window now.
        if let Some(cb) = platform.take_finish_launching_callback() {
            log::info!("on_surface_created: invoking finish-launching callback");
            cb();
        }
    }

    fn on_surface_changed(
        &self,
        _component: *mut xcomponent::OhNativeXComponent,
        _window: *mut c_void,
        width: u64,
        height: u64,
    ) {
        let platform = &self.platform;
        platform.update_primary_display(
            width as i32,
            height as i32,
            (super::density() * 160.0).round() as i32,
        );
        if let Some(win) = platform.primary_window() {
            win.handle_resize(width as i32, height as i32);
        }
    }

    fn on_surface_destroyed(
        &self,
        _component: *mut xcomponent::OhNativeXComponent,
        _window: *mut c_void,
    ) {
        if let Some(win) = self.platform.primary_window() {
            win.term_window();
        }
    }

    fn on_touch(
        &self,
        _component: *mut xcomponent::OhNativeXComponent,
        _window: *mut c_void,
        event: xcomponent::OhosTouchEvent,
    ) {
        let Some(win) = self.platform.primary_window() else {
            return;
        };
        let point = super::TouchPoint {
            id: event.id,
            x: event.x,
            y: event.y,
            action: event.event_type,
        };
        win.handle_touch(point);
    }

    fn on_focus_changed(&self, focused: bool) {
        if focused {
            self.platform.did_become_active();
        } else {
            self.platform.did_enter_background();
        }
        if let Some(win) = self.platform.primary_window() {
            win.set_active(focused);
        }
    }
}

// ── NAPI helpers ──────────────────────────────────────────────────────────────

fn register_function(env: NapiEnv, exports: NapiValue, name: &CStr, cb: NapiCallback) {
    let mut func: NapiValue = std::ptr::null_mut();
    let status = unsafe {
        napi_create_function(
            env,
            name.as_ptr(),
            name.to_bytes().len(),
            Some(cb),
            std::ptr::null_mut(),
            &mut func as *mut _,
        )
    };
    if status != NAPI_OK {
        log::warn!("napi_create_function({name:?}) status={status}");
        return;
    }
    let status = unsafe { napi_set_named_property(env, exports, name.as_ptr(), func) };
    if status != NAPI_OK {
        log::warn!("napi_set_named_property({name:?}) status={status}");
    }
}

/// Read up to `N` arguments from a callback info.
fn read_args<const N: usize>(env: NapiEnv, info: NapiCallbackInfo) -> (usize, [NapiValue; N]) {
    let mut argc: usize = N;
    let mut argv: [NapiValue; N] = [std::ptr::null_mut(); N];
    let status = unsafe {
        napi_get_cb_info(
            env,
            info,
            &mut argc as *mut usize,
            argv.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if status != NAPI_OK {
        return (0, argv);
    }
    (argc, argv)
}

fn arg_f64(env: NapiEnv, value: NapiValue) -> Option<f64> {
    if value.is_null() {
        return None;
    }
    let mut v: c_double = 0.0;
    (unsafe { napi_get_value_double(env, value, &mut v as *mut _) } == NAPI_OK).then_some(v)
}

fn undefined(env: NapiEnv) -> NapiValue {
    let mut v: NapiValue = std::ptr::null_mut();
    unsafe { napi_get_undefined(env, &mut v as *mut _) };
    v
}

// ── exported JS functions ─────────────────────────────────────────────────────

/// Create the UI-thread hop from a live ArkTS callback, once.
///
/// Every early entry point gets a shot because the order across hosts is not
/// guaranteed (`nativeInit` can land after `OnSurfaceCreated`), and the cost after
/// the first success is one atomic load.
fn ensure_main_thread_hop(env: NapiEnv) {
    if !main_thread::is_ready() {
        unsafe { main_thread::init(env) };
    }
}

extern "C" fn native_init(env: NapiEnv, _info: NapiCallbackInfo) -> NapiValue {
    ensure_main_thread_hop(env);
    log::info!("napi: nativeInit()");
    undefined(env)
}

/// `nativeSetDensity(scale)` — display density relative to 160 dpi
/// (= `display.getDefaultDisplaySync().densityDPI / 160`).
extern "C" fn native_set_density(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    ensure_main_thread_hop(env);
    let (argc, argv) = read_args::<1>(env, info);
    if argc >= 1 {
        if let Some(scale) = arg_f64(env, argv[0]) {
            super::set_density(scale as f32);
            log::info!("napi: density set to {scale:.2}");
        }
    }
    undefined(env)
}

/// `nativeSetSafeAreaInsets(top, bottom, left, right)` — logical pixels.
extern "C" fn native_set_safe_area_insets(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    ensure_main_thread_hop(env);
    let (argc, argv) = read_args::<4>(env, info);
    if argc >= 4 {
        let top = arg_f64(env, argv[0]).unwrap_or(0.0) as f32;
        let bottom = arg_f64(env, argv[1]).unwrap_or(0.0) as f32;
        let left = arg_f64(env, argv[2]).unwrap_or(0.0) as f32;
        let right = arg_f64(env, argv[3]).unwrap_or(0.0) as f32;
        super::set_safe_area_insets(top, bottom, left, right);
        log::info!("napi: safe area insets {top}/{bottom}/{left}/{right}");
    }
    undefined(env)
}

extern "C" fn native_on_foreground(env: NapiEnv, _info: NapiCallbackInfo) -> NapiValue {
    ensure_main_thread_hop(env);
    log::info!("napi: onForeground");
    if let Some(p) = try_platform() {
        p.did_become_active();
        if let Some(w) = p.primary_window() {
            w.set_active(true);
        }
    }
    undefined(env)
}

extern "C" fn native_on_background(env: NapiEnv, _info: NapiCallbackInfo) -> NapiValue {
    log::info!("napi: onBackground");
    if let Some(p) = try_platform() {
        p.did_enter_background();
        if let Some(w) = p.primary_window() {
            w.set_active(false);
        }
    }
    undefined(env)
}

extern "C" fn native_on_destroy(env: NapiEnv, _info: NapiCallbackInfo) -> NapiValue {
    log::info!("napi: onDestroy");
    vsync::stop();
    if let Some(p) = try_platform() {
        p.quit();
    }
    undefined(env)
}

/// `nativeSetKeyboardCallback(fn)` — ArkTS passes a JS function that shows /
/// hides the software keyboard.  It is invoked with an optional keyboard-type
/// string (absent ⇒ hide).
extern "C" fn native_set_keyboard_callback(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc < 1 {
        return undefined(env);
    }
    let value = argv[0];
    let mut ty: c_int = 0;
    if unsafe { napi_typeof(env, value, &mut ty as *mut _) } != NAPI_OK || ty != NAPI_FUNCTION {
        log::warn!("nativeSetKeyboardCallback: argument is not a function");
        return undefined(env);
    }

    let mut reference: NapiRef = std::ptr::null_mut();
    if unsafe { napi_create_reference(env, value, 1, &mut reference as *mut _) } != NAPI_OK {
        log::warn!("nativeSetKeyboardCallback: napi_create_reference failed");
        return undefined(env);
    }
    KEYBOARD_REF.store(reference, Ordering::Release);

    platform::set_keyboard_handler(Box::new(|kind| {
        call_keyboard_callback(kind);
    }));
    log::info!("napi: keyboard callback registered");
    undefined(env)
}

/// Invoke the ArkTS keyboard callback stored in `KEYBOARD_REF`.
fn call_keyboard_callback(kind: Option<crate::KeyboardType>) {
    let env = NAPI_ENV.load(Ordering::Acquire);
    if env.is_null() {
        return;
    }
    let reference = KEYBOARD_REF.load(Ordering::Acquire);
    if reference.is_null() {
        return;
    }

    let mut func: NapiValue = std::ptr::null_mut();
    if unsafe { napi_get_reference_value(env, reference, &mut func as *mut _) } != NAPI_OK
        || func.is_null()
    {
        return;
    }

    let this = undefined(env);
    let mut result: NapiValue = std::ptr::null_mut();

    match kind {
        Some(k) => {
            let name = keyboard_type_name(k);
            let c_name = std::ffi::CString::new(name).unwrap_or_default();
            let mut arg: NapiValue = std::ptr::null_mut();
            unsafe {
                napi_create_string_utf8(env, c_name.as_ptr(), name.len(), &mut arg as *mut _)
            };
            let argv = [arg];
            unsafe { napi_call_function(env, this, func, 1, argv.as_ptr(), &mut result as *mut _) };
        }
        None => {
            unsafe {
                napi_call_function(env, this, func, 0, std::ptr::null(), &mut result as *mut _)
            };
        }
    }
}

fn keyboard_type_name(k: crate::KeyboardType) -> &'static str {
    match k {
        crate::KeyboardType::Default => "default",
        crate::KeyboardType::EmailAddress => "email",
        crate::KeyboardType::Phone => "phone",
        crate::KeyboardType::NumberPad => "number",
        crate::KeyboardType::URL => "url",
        crate::KeyboardType::Decimal => "decimal",
    }
}
