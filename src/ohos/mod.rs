//! HarmonyOS / OpenHarmony platform implementation for GPUI.
//!
//! This module wires together all HarmonyOS-specific sub-modules and exposes
//! the single public entry-point `current_platform()`, mirroring the structure
//! of `gpui_mobile::android` and `gpui_mobile::ios`.
//!
//! ## Architecture
//!
//! ```text
//! OhosPlatform                (platform.rs)
//!   ├── OhosDispatcher        (dispatcher.rs)  — uv_async main queue + thread pool
//!   ├── OhosWindow            (window.rs)
//!   │     └── gpui_wgpu::WgpuRenderer   (GLES/EGL via OHNativeWindow)
//!   ├── OhosDisplay           (display.rs)
//!   ├── gpui_wgpu::CosmicTextSystem     (fonts loaded from /system/fonts)
//!   └── napi / xcomponent     (napi.rs, xcomponent.rs) — entry points + surface
//! ```
//!
//! ## Rendering
//!
//! OpenHarmony apps embed native rendering surfaces through an ArkUI
//! `XComponent` (type `SURFACE`).  When the surface is created the native
//! `OnSurfaceCreated(component, window)` callback receives an `OHNativeWindow *`
//! which is handed to wgpu via `raw_window_handle`'s `OhosNdkWindowHandle`.
//! wgpu's GLES/EGL backend has explicit OHOS support (the Vulkan backend does
//! not), so rendering goes through OpenGL ES.
//!
//! ## cfg note
//!
//! The OHOS Rust target triples are `aarch64-unknown-linux-ohos` and
//! `x86_64-unknown-linux-ohos`（后者是 x86_64 宿主上那台 HarmonyOS 模拟器），两者都报
//! `target_os = "linux"` **和** `target_env = "ohos"`。Every branch in this crate keys
//! off `target_env = "ohos"`, never `target_os`.
//!
//! This module is only compiled when `target_env = "ohos"`.

// ── 系统库 ───────────────────────────────────────────────────────────────────
//
// 这个模块里那 29 个 `extern "C"` 块只声明符号、不带 `#[link]`，所以链出来的 cdylib
// 只有一条 `DT_NEEDED: libc.so`，运行时在第一个未解析符号上就 relocation 失败
// （模拟器实测：`Error relocating lib…_example.so: OH_LOG_Print: symbol not found`）。
// 写在这一层而不是各下游的 build.rs：漏一次就是装不上又查不到原因。
//
// 名字对着 NDK sysroot 里的 `lib<name>.so`：`libhilog_ndk.z.so`（OH_LOG_Print）、
// `libace_napi.z.so`（napi_*）、`libace_ndk.z.so`（OH_NativeXComponent_*）、
// `libnative_vsync.so`（OH_NativeVSync_*）。EGL / GLES 不在这里 —— wgpu-hal 的 gles
// 后端是运行时 dlopen 载的。
#[link(name = "hilog_ndk.z", kind = "dylib")]
#[link(name = "ace_napi.z", kind = "dylib")]
#[link(name = "ace_ndk.z", kind = "dylib")]
#[link(name = "native_vsync", kind = "dylib")]
extern "C" {}

// ── geometry types ───────────────────────────────────────────────────────────
//
// As in the Android module: the real `gpui::Pixels` has a `pub(crate)` inner
// field that is inaccessible outside the `gpui` crate, so platform code cannot
// use `.0` on it.  We define local geometry stubs with fully public inner
// fields.  Sub-modules import them via `use super::*`.

/// A logical pixel value (CSS px).  Public inner field unlike `gpui::Pixels`.
#[derive(Copy, Clone, Debug, Default, PartialEq, PartialOrd)]
pub struct Pixels(pub f32);

impl Pixels {
    pub const ZERO: Self = Self(0.0);
}

impl From<f32> for Pixels {
    fn from(v: f32) -> Self {
        Self(v)
    }
}

impl From<Pixels> for f32 {
    fn from(p: Pixels) -> Self {
        p.0
    }
}

impl std::ops::Mul<f32> for Pixels {
    type Output = Self;
    fn mul(self, rhs: f32) -> Self {
        Self(self.0 * rhs)
    }
}

impl std::ops::Div<f32> for Pixels {
    type Output = Self;
    fn div(self, rhs: f32) -> Self {
        Self(self.0 / rhs)
    }
}

impl std::ops::Add for Pixels {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl std::ops::Sub for Pixels {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl std::ops::Neg for Pixels {
    type Output = Self;
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

/// Physical / device pixels.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DevicePixels(pub i32);

impl From<i32> for DevicePixels {
    fn from(v: i32) -> Self {
        Self(v)
    }
}

impl From<DevicePixels> for i32 {
    fn from(dp: DevicePixels) -> Self {
        dp.0
    }
}

/// A 2-D size.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Size<T> {
    pub width: T,
    pub height: T,
}

/// A 2-D point.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Point<T> {
    pub x: T,
    pub y: T,
}

/// An axis-aligned rectangle.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Bounds<T> {
    pub origin: Point<T>,
    pub size: Size<T>,
}

/// Convenience constructors (mirror `gpui::point` / `gpui::size`).
pub fn point<T>(x: T, y: T) -> Point<T> {
    Point { x, y }
}

pub fn size<T>(width: T, height: T) -> Size<T> {
    Size { width, height }
}

// ── sub-modules ──────────────────────────────────────────────────────────────

pub mod dispatcher;
pub mod display;
pub mod keyboard;
pub mod logging;
pub mod main_thread;
pub mod napi;
pub mod platform;
pub mod vsync;
pub mod window;
pub mod xcomponent;

// ── public re-exports ─────────────────────────────────────────────────────────

pub use dispatcher::OhosDispatcher;
pub use display::OhosDisplay;
pub use keyboard::*;
pub use logging::init_logger;
pub use platform::{OhosPlatform, SharedPlatform};
pub use window::{OhosNativeWindow, OhosPlatformWindow, OhosWindow, SafeAreaInsets};

// ── platform entry-point (mirrors gpui_linux::current_platform) ───────────────

use std::rc::Rc;

/// Returns the HarmonyOS platform implementation.
///
/// `headless` is accepted for API parity with the iOS / Android equivalents.
/// When `true` the platform is constructed without an active `OHNativeWindow`,
/// which is useful for off-screen rendering and testing.
pub fn current_platform(headless: bool) -> Rc<dyn gpui::Platform> {
    Rc::new(OhosPlatform::new(headless))
}

// ── helper: OHOS render backend ───────────────────────────────────────────────

/// Which GPU back-end to use for the wgpu surface.
///
/// On HarmonyOS only the OpenGL ES path is currently usable: wgpu's Vulkan
/// backend has no `OhosNdk` surface support, whereas its GLES/EGL backend has
/// explicit `target_env = "ohos"` handling.  The enum is kept open so a
/// future Vulkan path can be added without an API break.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum OhosBackend {
    /// OpenGL ES via EGL (the only supported backend today).
    #[default]
    Gles,
    /// Vulkan — reserved; not yet supported for OHOS surfaces.
    Vulkan,
}

impl std::fmt::Display for OhosBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gles => write!(f, "OpenGL ES"),
            Self::Vulkan => write!(f, "Vulkan"),
        }
    }
}

// ── helper: key / input event types ──────────────────────────────────────────

/// A minimal key event representation.
///
/// Maps the subset of the OHOS `OH_NativeXComponent_KeyEvent` fields that GPUI
/// needs.
#[derive(Clone, Debug)]
pub struct OhosKeyEvent {
    /// OHOS `KeyCode` value (see `native_key_event.h`, e.g. `KEY_A = 2017`).
    pub key_code: i32,
    /// `OH_NativeXComponent_KeyAction`: `DOWN = 0`, `UP = 1`, `CANCEL = 2`.
    pub action: i32,
    /// Modifier bitmask (shift / ctrl / alt / meta).
    pub meta_state: u32,
    /// Unicode character produced (0 if none).
    pub unicode_char: u32,
}

/// A single touch point from an `OH_NativeXComponent_TouchEvent`.
#[derive(Clone, Debug)]
pub struct TouchPoint {
    pub id: i32,
    /// X in surface (physical) pixels.
    pub x: f32,
    /// Y in surface (physical) pixels.
    pub y: f32,
    /// `OH_NativeXComponent_TouchEventType`:
    /// `DOWN = 0`, `UP = 1`, `MOVE = 2`, `CANCEL = 3`.
    pub action: u32,
}

// ── global display / chrome state ─────────────────────────────────────────────
//
// HarmonyOS density and safe-area insets are owned by the ArkTS layer and
// pushed into native code via the NAPI exports in `napi.rs`.  They are stored
// here so the platform/window code can read them synchronously.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Display density as a scale factor relative to 160 dpi, stored as `f32` bits.
static DENSITY_BITS: AtomicU32 = AtomicU32::new(0); // 0 == "unset"

/// Safe-area insets in logical pixels, stored as `f32` bits: (top, bottom, left, right).
static SAFE_AREA_BITS: [AtomicU32; 4] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

/// Desired status-bar light/dark content style (`false` = dark content).
static STATUS_BAR_LIGHT: AtomicBool = AtomicBool::new(false);

/// Desired status-bar background colour (0xRRGGBB, `u32::MAX` == unset).
static STATUS_BAR_COLOR: AtomicU32 = AtomicU32::new(u32::MAX);

/// Desired navigation-bar background colour (`u32::MAX` == unset).
static NAV_BAR_COLOR: AtomicU32 = AtomicU32::new(u32::MAX);

fn f32_bits(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

fn store_f32(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Release);
}

/// Set the display density (scale factor relative to 160 dpi).
///
/// Called by `napi.rs` from ArkTS using the value reported by
/// `display.getDefaultDisplaySync().densityDPI / 160`.
pub fn set_density(scale_factor: f32) {
    store_f32(&DENSITY_BITS, scale_factor);
}

/// The current display density, defaulting to `1.0` until ArkTS reports it.
pub fn density() -> f32 {
    let v = f32_bits(&DENSITY_BITS);
    if v > 0.0 {
        v
    } else {
        1.0
    }
}

/// Set the window safe-area insets (logical pixels).
pub fn set_safe_area_insets(top: f32, bottom: f32, left: f32, right: f32) {
    store_f32(&SAFE_AREA_BITS[0], top);
    store_f32(&SAFE_AREA_BITS[1], bottom);
    store_f32(&SAFE_AREA_BITS[2], left);
    store_f32(&SAFE_AREA_BITS[3], right);
}

/// Query the safe-area insets (logical pixels) as `(top, bottom, left, right)`.
pub fn safe_area_insets() -> (f32, f32, f32, f32) {
    (
        f32_bits(&SAFE_AREA_BITS[0]),
        f32_bits(&SAFE_AREA_BITS[1]),
        f32_bits(&SAFE_AREA_BITS[2]),
        f32_bits(&SAFE_AREA_BITS[3]),
    )
}

/// Apply system chrome (status bar / navigation bar) styling.
///
/// The actual styling is performed by the ArkTS layer — native code records
/// the request and `napi.rs` exposes it to the ArkTS `Index.ets` page, which
/// calls the `@ohos.window` APIs.
pub fn apply_system_chrome(style: &crate::SystemChromeStyle) {
    STATUS_BAR_LIGHT.store(
        matches!(style.status_bar_style, crate::StatusBarContentStyle::Light),
        Ordering::Release,
    );
    STATUS_BAR_COLOR.store(
        style.status_bar_color.unwrap_or(u32::MAX),
        Ordering::Release,
    );
    NAV_BAR_COLOR.store(
        style.navigation_bar_color.unwrap_or(u32::MAX),
        Ordering::Release,
    );
    log::debug!(
        "ohos::apply_system_chrome: status_light={} status_color={:?} nav_color={:?}",
        style.status_bar_style == crate::StatusBarContentStyle::Light,
        style.status_bar_color,
        style.navigation_bar_color,
    );
}

/// Whether the status-bar content should be light (white icons).
pub fn status_bar_is_light() -> bool {
    STATUS_BAR_LIGHT.load(Ordering::Acquire)
}

/// Requested status-bar background colour (`None` if unset).
pub fn status_bar_color() -> Option<u32> {
    match STATUS_BAR_COLOR.load(Ordering::Acquire) {
        u32::MAX => None,
        v => Some(v),
    }
}

/// Requested navigation-bar background colour (`None` if unset).
pub fn navigation_bar_color() -> Option<u32> {
    match NAV_BAR_COLOR.load(Ordering::Acquire) {
        u32::MAX => None,
        v => Some(v),
    }
}

/// Show the software keyboard.
///
/// The OHOS software keyboard is driven from ArkTS (`@ohos.inputMethod`).
/// Native code records the request; a NAPI callback registered by the ArkTS
/// page is invoked to actually present the IME.  See `napi.rs`.
pub fn show_keyboard(keyboard_type: crate::KeyboardType) {
    log::debug!("ohos::show_keyboard({keyboard_type:?})");
    platform::request_show_keyboard(keyboard_type);
}

/// Hide the software keyboard.
pub fn hide_keyboard() {
    log::debug!("ohos::hide_keyboard()");
    platform::request_hide_keyboard();
}

// ── shared logging helper ─────────────────────────────────────────────────────

/// Initialise the hilog logger (idempotent).
///
/// Retained as a named helper for parity with `android::init_logger`.
pub fn init() {
    init_logger();
}
