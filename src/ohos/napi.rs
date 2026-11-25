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
use std::path::PathBuf;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, OnceLock};

use gpui::{App, AppLifecyclePhase, Application, WindowAppearance};

use super::platform::{self, OhosPlatform, SharedPlatform};
use super::window::OhosNativeWindow;
use super::{vsync, xcomponent};

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
    fn napi_get_value_string_utf8(
        env: NapiEnv,
        value: NapiValue,
        buf: *mut c_char,
        buflen: usize,
        result: *mut usize,
    ) -> NapiStatus;
    fn napi_create_int32(env: NapiEnv, value: c_int, result: *mut NapiValue) -> NapiStatus;
    fn napi_create_double(env: NapiEnv, value: c_double, result: *mut NapiValue) -> NapiStatus;
    fn napi_get_value_bool(env: NapiEnv, value: NapiValue, result: *mut bool) -> NapiStatus;
    fn napi_get_array_length(env: NapiEnv, value: NapiValue, result: *mut u32) -> NapiStatus;
    fn napi_get_element(
        env: NapiEnv,
        object: NapiValue,
        index: u32,
        result: *mut NapiValue,
    ) -> NapiStatus;
    fn napi_get_boolean(env: NapiEnv, value: bool, result: *mut NapiValue) -> NapiStatus;
    fn napi_get_null(env: NapiEnv, result: *mut NapiValue) -> NapiStatus;
    fn napi_create_array_with_length(
        env: NapiEnv,
        length: usize,
        result: *mut NapiValue,
    ) -> NapiStatus;
    fn napi_set_element(
        env: NapiEnv,
        object: NapiValue,
        index: u32,
        value: NapiValue,
    ) -> NapiStatus;
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
/// `napi_valuetype::napi_null`.
const NAPI_NULL: c_int = 1;
/// `napi_valuetype::napi_undefined`.
const NAPI_UNDEFINED: c_int = 0;

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
static NAPI_ENV: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static KEYBOARD_REF: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static CLIPBOARD_WRITE_REF: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static OPEN_URL_REF: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static PICK_PATHS_REF: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static CREDENTIAL_REF: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

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

    // 1. Register the plain NAPI functions ArkTS calls directly.
    register_function(env, exports, c"nativeInit", native_init);
    register_function(env, exports, c"nativeSetDensity", native_set_density);
    register_function(env, exports, c"nativeLifecycle", native_lifecycle);
    register_function(env, exports, c"nativeMemoryWarning", native_memory_warning);
    register_function(env, exports, c"nativeSetAppearance", native_set_appearance);
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
    register_function(env, exports, c"nativeSubmitText", native_submit_text);
    register_function(env, exports, c"nativeKeyEvent", native_key_event);
    register_function(
        env,
        exports,
        c"nativeSetClipboardCallback",
        native_set_clipboard_callback,
    );
    register_function(
        env,
        exports,
        c"nativeClipboardChanged",
        native_clipboard_changed,
    );
    register_function(
        env,
        exports,
        c"nativeTakeSystemChrome",
        native_take_system_chrome,
    );
    register_function(
        env,
        exports,
        c"nativeSystemChromeGeneration",
        native_system_chrome_generation,
    );
    register_function(
        env,
        exports,
        c"nativeSetKeyboardHeight",
        native_set_keyboard_height,
    );
    register_function(
        env,
        exports,
        c"nativeSetOpenUrlCallback",
        native_set_open_url_callback,
    );
    register_function(
        env,
        exports,
        c"nativeSetPickPathsCallback",
        native_set_pick_paths_callback,
    );
    register_function(env, exports, c"nativePickerResult", native_picker_result);
    register_function(
        env,
        exports,
        c"nativeSetCredentialCallback",
        native_set_credential_callback,
    );
    register_function(env, exports, c"nativeOpenUrls", native_open_urls);

    // 2. Create the platform singleton and start the (non-blocking) GPUI app.
    let platform = PLATFORM
        .get_or_init(|| Arc::new(OhosPlatform::new(false)))
        .clone();

    let run_platform = SharedPlatform::new(platform.clone()).into_rc();
    Application::with_platform(run_platform).run(|cx: &mut App| {
        if let Some(cb) = APP_CB.lock().take() {
            log::info!("napi: invoking app callback");
            cb(cx);
        }
    });

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

/// Read a JS string argument.  `None` for non-string values.
fn arg_string(env: NapiEnv, value: NapiValue) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // First query the byte length, then read into a buffer of that size.
    let mut len: usize = 0;
    if unsafe { napi_get_value_string_utf8(env, value, std::ptr::null_mut(), 0, &mut len) }
        != NAPI_OK
    {
        return None;
    }
    let mut buf = vec![0u8; len + 1];
    let mut written: usize = 0;
    if unsafe {
        napi_get_value_string_utf8(
            env,
            value,
            buf.as_mut_ptr() as *mut c_char,
            buf.len(),
            &mut written,
        )
    } != NAPI_OK
    {
        return None;
    }
    buf.truncate(written);
    String::from_utf8(buf).ok()
}

fn arg_bool(env: NapiEnv, value: NapiValue) -> Option<bool> {
    if value.is_null() {
        return None;
    }
    let mut v = false;
    (unsafe { napi_get_value_bool(env, value, &mut v as *mut _) } == NAPI_OK).then_some(v)
}

fn arg_string_array(env: NapiEnv, value: NapiValue) -> Option<Vec<String>> {
    if value.is_null() {
        return None;
    }
    let mut len = 0u32;
    if unsafe { napi_get_array_length(env, value, &mut len as *mut _) } != NAPI_OK {
        return None;
    }
    let mut result = Vec::with_capacity(len as usize);
    for i in 0..len {
        let mut item = std::ptr::null_mut();
        if unsafe { napi_get_element(env, value, i, &mut item as *mut _) } != NAPI_OK {
            return None;
        }
        result.push(arg_string(env, item)?);
    }
    Some(result)
}

fn store_function_ref(env: NapiEnv, value: NapiValue) -> Option<NapiRef> {
    let mut ty: c_int = 0;
    if unsafe { napi_typeof(env, value, &mut ty as *mut _) } != NAPI_OK || ty != NAPI_FUNCTION {
        log::warn!("callback argument is not a function");
        return None;
    }
    let mut reference: NapiRef = std::ptr::null_mut();
    if unsafe { napi_create_reference(env, value, 1, &mut reference as *mut _) } != NAPI_OK {
        log::warn!("napi_create_reference failed");
        return None;
    }
    Some(reference)
}

fn call_function(env: NapiEnv, reference: NapiRef, args: &[NapiValue]) -> Option<NapiValue> {
    if env.is_null() || reference.is_null() {
        return None;
    }
    let mut func: NapiValue = std::ptr::null_mut();
    if unsafe { napi_get_reference_value(env, reference, &mut func as *mut _) } != NAPI_OK
        || func.is_null()
    {
        return None;
    }
    let mut result: NapiValue = std::ptr::null_mut();
    let status = unsafe {
        napi_call_function(
            env,
            undefined(env),
            func,
            args.len(),
            args.as_ptr(),
            &mut result as *mut _,
        )
    };
    (status == NAPI_OK).then_some(result)
}

fn string_value(env: NapiEnv, value: &str) -> Option<NapiValue> {
    let c_value = std::ffi::CString::new(value).ok()?;
    let mut result: NapiValue = std::ptr::null_mut();
    (unsafe { napi_create_string_utf8(env, c_value.as_ptr(), value.len(), &mut result as *mut _) }
        == NAPI_OK)
        .then_some(result)
}

fn bool_value(env: NapiEnv, value: bool) -> NapiValue {
    let mut v: NapiValue = std::ptr::null_mut();
    unsafe { napi_get_boolean(env, value, &mut v as *mut _) };
    v
}

fn null_value(env: NapiEnv) -> NapiValue {
    let mut v: NapiValue = std::ptr::null_mut();
    unsafe { napi_get_null(env, &mut v as *mut _) };
    v
}

fn int32(env: NapiEnv, value: c_int) -> NapiValue {
    let mut v: NapiValue = std::ptr::null_mut();
    unsafe { napi_create_int32(env, value, &mut v as *mut _) };
    v
}

fn double(env: NapiEnv, value: f64) -> NapiValue {
    let mut v: NapiValue = std::ptr::null_mut();
    unsafe { napi_create_double(env, value, &mut v as *mut _) };
    v
}

fn undefined(env: NapiEnv) -> NapiValue {
    let mut v: NapiValue = std::ptr::null_mut();
    unsafe { napi_get_undefined(env, &mut v as *mut _) };
    v
}

// ── exported JS functions ─────────────────────────────────────────────────────

extern "C" fn native_init(env: NapiEnv, _info: NapiCallbackInfo) -> NapiValue {
    log::info!("napi: nativeInit()");
    undefined(env)
}

/// `nativeSetDensity(scale)` — display density relative to 160 dpi
/// (= `display.getDefaultDisplaySync().densityDPI / 160`).
extern "C" fn native_set_density(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc >= 1 {
        if let Some(scale) = arg_f64(env, argv[0]) {
            super::set_density(scale as f32);
            if let Some(p) = try_platform() {
                p.apply_density(scale as f32);
                p.refresh_insets();
            }
            log::info!("napi: density set to {scale:.2}");
        }
    }
    undefined(env)
}

/// `nativeLifecycle(phase)` — 0=Active, 1=Inactive, 2=Background, 3=Foreground.
extern "C" fn native_lifecycle(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc >= 1 {
        let phase = match arg_f64(env, argv[0]).unwrap_or(2.0) as i32 {
            0 => AppLifecyclePhase::Active,
            1 => AppLifecyclePhase::Inactive,
            3 => AppLifecyclePhase::Foreground,
            _ => AppLifecyclePhase::Background,
        };
        if let Some(p) = try_platform() {
            p.deliver_lifecycle(phase);
        }
    }
    undefined(env)
}

/// `nativeMemoryWarning()` — ArkTS `onMemoryLevel` observed pressure.
extern "C" fn native_memory_warning(env: NapiEnv, _info: NapiCallbackInfo) -> NapiValue {
    if let Some(p) = try_platform() {
        p.deliver_memory_warning();
    }
    undefined(env)
}

/// `nativeSetAppearance(mode)` — 0=light, 1=dark, 2=vibrant light, 3=vibrant dark.
extern "C" fn native_set_appearance(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc >= 1 {
        let mode = arg_f64(env, argv[0]).unwrap_or(0.0) as i32;
        let appearance = match mode {
            1 => WindowAppearance::Dark,
            2 => WindowAppearance::VibrantLight,
            3 => WindowAppearance::VibrantDark,
            _ => WindowAppearance::Light,
        };
        super::set_appearance_mode(mode.clamp(0, 3) as u32);
        if let Some(p) = try_platform() {
            p.set_appearance(appearance);
        }
    }
    undefined(env)
}

/// `nativeSetSafeAreaInsets(top, bottom, left, right)` — logical pixels.
extern "C" fn native_set_safe_area_insets(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<4>(env, info);
    if argc >= 4 {
        let top = arg_f64(env, argv[0]).unwrap_or(0.0) as f32;
        let bottom = arg_f64(env, argv[1]).unwrap_or(0.0) as f32;
        let left = arg_f64(env, argv[2]).unwrap_or(0.0) as f32;
        let right = arg_f64(env, argv[3]).unwrap_or(0.0) as f32;
        super::set_safe_area_insets(top, bottom, left, right);
        if let Some(p) = try_platform() {
            if let Some(win) = p.primary_window() {
                let scale = super::density().max(0.001);
                win.set_safe_area_insets(super::window::SafeAreaInsets {
                    top: top * scale,
                    bottom: bottom * scale,
                    left: left * scale,
                    right: right * scale,
                });
            }
            p.refresh_insets();
        }
        log::info!("napi: safe area insets {top}/{bottom}/{left}/{right}");
    }
    undefined(env)
}

extern "C" fn native_on_foreground(env: NapiEnv, _info: NapiCallbackInfo) -> NapiValue {
    log::info!("napi: onForeground");
    if let Some(p) = try_platform() {
        p.did_become_active();
        p.deliver_lifecycle(AppLifecyclePhase::Foreground);
        p.deliver_lifecycle(AppLifecyclePhase::Active);
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
        p.deliver_lifecycle(AppLifecyclePhase::Inactive);
        p.deliver_lifecycle(AppLifecyclePhase::Background);
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
        if let Some(win) = p.primary_window() {
            win.notify_close();
        }
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

// ── ArkTS → native clipboard ──────────────────────────────────────────────────

/// `nativeSetClipboardCallback(fn)` — ArkTS provides a function that writes a
/// string to the system pasteboard when GPUI calls `write_to_clipboard`.
extern "C" fn native_set_clipboard_callback(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc < 1 {
        return undefined(env);
    }
    let value = argv[0];
    let mut ty: c_int = 0;
    if unsafe { napi_typeof(env, value, &mut ty as *mut _) } != NAPI_OK || ty != NAPI_FUNCTION {
        log::warn!("nativeSetClipboardCallback: argument is not a function");
        return undefined(env);
    }
    let mut reference: NapiRef = std::ptr::null_mut();
    if unsafe { napi_create_reference(env, value, 1, &mut reference as *mut _) } != NAPI_OK {
        log::warn!("nativeSetClipboardCallback: napi_create_reference failed");
        return undefined(env);
    }
    CLIPBOARD_WRITE_REF.store(reference, Ordering::Release);
    platform::set_clipboard_write_handler(Box::new(call_clipboard_write_callback));
    log::info!("napi: clipboard callback registered");
    undefined(env)
}

fn call_clipboard_write_callback(text: &str) {
    let env = NAPI_ENV.load(Ordering::Acquire);
    let reference = CLIPBOARD_WRITE_REF.load(Ordering::Acquire);
    if env.is_null() || reference.is_null() {
        return;
    }
    let mut func: NapiValue = std::ptr::null_mut();
    if unsafe { napi_get_reference_value(env, reference, &mut func as *mut _) } != NAPI_OK
        || func.is_null()
    {
        return;
    }
    let c_text = match std::ffi::CString::new(text) {
        Ok(v) => v,
        Err(_) => return,
    };
    let mut arg: NapiValue = std::ptr::null_mut();
    if unsafe { napi_create_string_utf8(env, c_text.as_ptr(), text.len(), &mut arg as *mut _) }
        != NAPI_OK
    {
        return;
    }
    let this = undefined(env);
    let mut result: NapiValue = std::ptr::null_mut();
    let argv = [arg];
    unsafe { napi_call_function(env, this, func, 1, argv.as_ptr(), &mut result as *mut _) };
}

/// `nativeClipboardChanged(text)` — ArkTS reports the current system pasteboard
/// text (best-effort, called when the app regains focus).
extern "C" fn native_clipboard_changed(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc >= 1 {
        if let Some(text) = arg_string(env, argv[0]) {
            if let Some(p) = try_platform() {
                p.clipboard_changed(text);
            }
        }
    }
    undefined(env)
}

// ── ArkTS → native text input / system chrome ────────────────────────────────

/// `nativeSubmitText(text)` — ArkTS IME delivered committed text or an editing
/// key sequence (`"\x08"`, `"\x1b[D"`, ...).  Routed through the same
/// `dispatch_text_input` path Android and iOS use.
extern "C" fn native_submit_text(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc >= 1 {
        if let Some(text) = arg_string(env, argv[0]) {
            if !text.is_empty() {
                crate::dispatch_text_input(&text);
            }
        }
    }
    undefined(env)
}

/// `nativeKeyEvent(key, modifiers)` — ArkTS delivered a hardware/IME editing
/// key that must reach GPUI as a `KeyDown`/`KeyUp` event.
///
/// `key` uses GPUI key names (`"enter"`, `"backspace"`, `"escape"`, `"tab"`,
/// `"up"`, ...).  `modifiers` is a bitmask: 1 = shift, 2 = control, 4 = alt,
/// 8 = platform/meta, 16 = function.
extern "C" fn native_key_event(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<2>(env, info);
    if argc < 1 {
        return undefined(env);
    }
    let Some(key) = arg_string(env, argv[0]) else {
        return undefined(env);
    };
    let Some(key_code) = gpui_key_name_to_ohos(&key) else {
        log::warn!("nativeKeyEvent: unsupported key name {key:?}");
        return undefined(env);
    };
    let bits = if argc >= 2 {
        arg_f64(env, argv[1]).unwrap_or(0.0) as u32
    } else {
        0
    };
    let meta_state = (if bits & 1 != 0 {
        super::keyboard::OHOS_META_SHIFT_ON
    } else {
        0
    }) | (if bits & 2 != 0 {
        super::keyboard::OHOS_META_CTRL_ON
    } else {
        0
    }) | (if bits & 4 != 0 {
        super::keyboard::OHOS_META_ALT_ON
    } else {
        0
    }) | (if bits & 8 != 0 {
        super::keyboard::OHOS_META_META_ON
    } else {
        0
    }) | (if bits & 16 != 0 {
        super::keyboard::OHOS_META_FUNCTION_ON
    } else {
        0
    });
    let Some(win) = try_platform().and_then(|p| p.primary_window()) else {
        return undefined(env);
    };
    win.handle_key_event(super::OhosKeyEvent {
        key_code,
        action: super::keyboard::KEY_ACTION_DOWN,
        meta_state,
        unicode_char: 0,
    });
    undefined(env)
}

/// Map a GPUI key name (as sent from ArkTS) to an OHOS `KeyCode`.
fn gpui_key_name_to_ohos(key: &str) -> Option<i32> {
    use super::keyboard::*;
    Some(match key {
        "enter" => KEY_ENTER,
        "backspace" => KEY_DEL,
        "delete" => KEY_FORWARD_DEL,
        "escape" => KEY_ESCAPE,
        "tab" => KEY_TAB,
        "space" => KEY_SPACE,
        "up" => KEY_DPAD_UP,
        "down" => KEY_DPAD_DOWN,
        "left" => KEY_DPAD_LEFT,
        "right" => KEY_DPAD_RIGHT,
        "home" => KEY_MOVE_HOME,
        "end" => KEY_MOVE_END,
        "pageup" => KEY_PAGE_UP,
        "pagedown" => KEY_PAGE_DOWN,
        "insert" => KEY_INSERT,
        _ => return None,
    })
}

/// `nativeTakeSystemChrome()` — ArkTS polls this after each `nativeInit` /
/// foreground transition.  Returns `null` while no style has been requested,
/// otherwise `[statusColor, lightIcons, navColor]`.  `null` colour slots mean
/// "leave the system default"; using an `i32` here would truncate 24-bit
/// colours with high bits set into a negative sentinel.
extern "C" fn native_take_system_chrome(env: NapiEnv, _info: NapiCallbackInfo) -> NapiValue {
    let status = super::status_bar_color();
    let nav = super::navigation_bar_color();
    if status.is_none() && nav.is_none() && !super::status_bar_is_light() {
        return null_value(env);
    }
    let light = super::status_bar_is_light();
    let values = [
        status.map_or_else(|| null_value(env), |c| double(env, c as f64)),
        bool_value(env, light),
        nav.map_or_else(|| null_value(env), |c| double(env, c as f64)),
    ];
    let mut array: NapiValue = std::ptr::null_mut();
    if unsafe { napi_create_array_with_length(env, values.len(), &mut array as *mut _) } != NAPI_OK
    {
        return undefined(env);
    }
    for (i, value) in values.into_iter().enumerate() {
        if unsafe { napi_set_element(env, array, i as u32, value) } != NAPI_OK {
            return undefined(env);
        }
    }
    array
}

/// `nativeSystemChromeGeneration()` — generation counter for the style above.
extern "C" fn native_system_chrome_generation(env: NapiEnv, _info: NapiCallbackInfo) -> NapiValue {
    double(env, super::system_chrome_generation() as f64)
}

/// `nativeSetKeyboardHeight(height)` — ArkTS reports the on-screen IME height
/// in logical pixels through `keyboard.getKeyboardHeight()` / the input event.
extern "C" fn native_set_keyboard_height(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc >= 1 {
        if let Some(height) = arg_f64(env, argv[0]) {
            crate::set_keyboard_height(height.max(0.0) as f32);
            if let Some(p) = try_platform() {
                p.refresh_insets();
            }
        }
    }
    undefined(env)
}

/// `nativeSetOpenUrlCallback(fn)` — the callback returns `true` when ArkTS
/// accepts the URL.  Its result is forwarded to GPUI's `open_url`.
extern "C" fn native_set_open_url_callback(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc < 1 {
        return undefined(env);
    }
    let Some(reference) = store_function_ref(env, argv[0]) else {
        return undefined(env);
    };
    OPEN_URL_REF.store(reference, Ordering::Release);
    platform::set_open_url_handler(Box::new(|url| {
        let env = NAPI_ENV.load(Ordering::Acquire);
        let Some(text) = string_value(env, url) else {
            return false;
        };
        call_function(env, OPEN_URL_REF.load(Ordering::Acquire), &[text])
            .and_then(|v| arg_bool(env, v))
            .unwrap_or(false)
    }));
    log::info!("napi: open URL callback registered");
    undefined(env)
}

/// `nativeSetPickPathsCallback(fn)` — ArkTS picks paths and calls back through
/// `nativePickerResult`.
extern "C" fn native_set_pick_paths_callback(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc < 1 {
        return undefined(env);
    }
    let Some(reference) = store_function_ref(env, argv[0]) else {
        return undefined(env);
    };
    PICK_PATHS_REF.store(reference, Ordering::Release);
    platform::set_pick_paths_handler(Box::new(|directories, save, suggested_name| {
        let env = NAPI_ENV.load(Ordering::Acquire);
        let args: Vec<NapiValue> = [
            Some(bool_value(env, directories)),
            Some(bool_value(env, save)),
            suggested_name.and_then(|name| string_value(env, name)),
        ]
        .into_iter()
        .flatten()
        .collect();
        call_function(env, PICK_PATHS_REF.load(Ordering::Acquire), &args);
    }));
    log::info!("napi: path picker callback registered");
    undefined(env)
}

/// `nativePickerResult(ok, paths)` — ArkTS returns a path picker result.
extern "C" fn native_picker_result(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<2>(env, info);
    if argc < 2 {
        return undefined(env);
    }
    let ok = arg_bool(env, argv[0]).unwrap_or(false);
    let result = if ok {
        arg_string_array(env, argv[1])
            .map(|s| Some(s.into_iter().map(PathBuf::from).collect()))
            .ok_or_else(|| anyhow::anyhow!("invalid picker result"))
    } else {
        Ok(None)
    };
    if let Some(p) = try_platform() {
        p.finish_picker(result);
    }
    undefined(env)
}

/// `nativeSetCredentialCallback(fn)` — ArkTS implements credential storage.
///
/// The callback receives `(service, username, operation, password?)` where
/// operation is `0` = read, `1` = write, `2` = delete.  Write includes the
/// password as a UTF-8 string; read returns the password string or `null`, and
/// write/delete return nothing.  Failures should be thrown by ArkTS.
extern "C" fn native_set_credential_callback(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc < 1 {
        return undefined(env);
    }
    let Some(reference) = store_function_ref(env, argv[0]) else {
        return undefined(env);
    };
    CREDENTIAL_REF.store(reference, Ordering::Release);
    platform::set_credential_handler(Box::new(
        |service: &str, username: &str, op: platform::CredentialOp<'_>| {
            let env = NAPI_ENV.load(Ordering::Acquire);
            let Some(service_value) = string_value(env, service) else {
                return Err(anyhow::anyhow!("failed to create credential service value"));
            };
            let Some(username_value) = string_value(env, username) else {
                return Err(anyhow::anyhow!(
                    "failed to create credential username value"
                ));
            };
            let (code, password): (i32, Option<&[u8]>) = match op {
                platform::CredentialOp::Read => (0, None),
                platform::CredentialOp::Write(bytes) => (1, Some(bytes)),
                platform::CredentialOp::Delete => (2, None),
            };
            let mut args = vec![service_value, username_value, int32(env, code)];
            if let Some(bytes) = password {
                let text = String::from_utf8_lossy(bytes);
                let Some(value) = string_value(env, &text) else {
                    return Err(anyhow::anyhow!(
                        "failed to create credential password value"
                    ));
                };
                args.push(value);
            }
            let result = call_function(env, CREDENTIAL_REF.load(Ordering::Acquire), &args)
                .ok_or_else(|| anyhow::anyhow!("credential bridge callback failed"))?;
            match op {
                platform::CredentialOp::Read => {
                    if result.is_null() {
                        return Ok(None);
                    }
                    let mut ty: c_int = 0;
                    if unsafe { napi_typeof(env, result, &mut ty as *mut _) } != NAPI_OK {
                        return Err(anyhow::anyhow!("failed to inspect credential result"));
                    }
                    if ty == NAPI_NULL || ty == NAPI_UNDEFINED {
                        return Ok(None);
                    }
                    Ok(arg_string(env, result).map(String::into_bytes))
                }
                platform::CredentialOp::Write(_) | platform::CredentialOp::Delete => Ok(None),
            }
        },
    ));
    log::info!("napi: credential callback registered");
    undefined(env)
}

/// `nativeOpenUrls(urls)` — ArkTS reports a deeplink / `want` URI list.
extern "C" fn native_open_urls(env: NapiEnv, info: NapiCallbackInfo) -> NapiValue {
    let (argc, argv) = read_args::<1>(env, info);
    if argc >= 1 {
        if let Some(urls) = arg_string_array(env, argv[0]) {
            if let Some(p) = try_platform() {
                p.deliver_open_urls(urls.clone());
            }
            #[cfg(feature = "deeplink")]
            for url in &urls {
                crate::packages::deeplink::ohos_notify_deep_link(url);
            }
        }
    }
    undefined(env)
}
