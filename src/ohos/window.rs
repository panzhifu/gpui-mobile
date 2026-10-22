//! HarmonyOS window implementation.
//!
//! `OhosWindow` wraps an `OHNativeWindow *` (handed to us by the ArkUI
//! `XComponent` via `OnSurfaceCreated`) and owns the `WgpuRenderer` that draws
//! into it.
//!
//! ## Rendering path
//!
//! The `OHNativeWindow` is exposed to wgpu as a `raw_window_handle`
//! `OhosNdkWindowHandle`.  wgpu's **GLES/EGL** backend has explicit OHOS
//! support (`wgpu-hal` matches `Rwh::OhosNdk` and `target_env = "ohos"`),
//! so rendering goes through OpenGL ES.  The Vulkan backend has no OHOS
//! surface support and is never selected.
//!
//! ## Lifecycle
//!
//! HarmonyOS surfaces can be destroyed and recreated at any time (background /
//! foreground, rotation):
//!
//! * `OnSurfaceCreated`  — create the wgpu surface and renderer.
//! * `OnSurfaceChanged`  — call `handle_resize`.
//! * `OnSurfaceDestroyed` — unconfigure the surface, keep the window struct.
//!
//! ## Input events
//!
//! Touch / mouse / key events arrive from `xcomponent.rs` as the
//! `TouchPoint` / `OhosKeyEvent` types defined in `mod.rs` before being
//! forwarded to registered callbacks.
//!
//! `OhosPlatformWindow` wraps an `Arc<OhosWindow>` and implements
//! `gpui::PlatformWindow`, `HasWindowHandle` and `HasDisplayHandle`.

#![allow(unsafe_code)]

use anyhow::{Context as _, Result};
use futures::channel::oneshot;
use gpui::{
    self, AtlasKey, AtlasTile, Capslock, DispatchEventResult, GpuSpecs, Modifiers, PlatformAtlas,
    PlatformDisplay, PlatformInputHandler, PlatformWindow, PromptButton, PromptLevel,
    RequestFrameOptions, WindowBackgroundAppearance, WindowBounds, WindowControlArea,
    WindowVisibility,
};
use gpui_wgpu::{wgpu, GpuContext, WgpuRenderer, WgpuSurfaceConfig};
use parking_lot::Mutex;
use raw_window_handle::{
    HasDisplayHandle, HasWindowHandle, OhosDisplayHandle, OhosNdkWindowHandle, RawDisplayHandle,
    RawWindowHandle,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;

use super::{OhosKeyEvent, Pixels, Size, TouchPoint};
use crate::momentum::{MomentumScroller, VelocityTracker};

/// The default preferred present mode for the OHOS GLES surface.
///
/// GLES presents through the EGL swap interval; `Fifo` maps to vsync'd
/// presentation and is universally supported, unlike `Mailbox` which the GL
/// backend does not implement.
const PREFERRED_PRESENT_MODE: wgpu::PresentMode = wgpu::PresentMode::Fifo;

// ── OhosNativeWindow ──────────────────────────────────────────────────────────

/// Owned wrapper around an `OHNativeWindow *` handed to us by the `XComponent`.
///
/// The pointer is owned by the ArkUI `XComponent`; we only borrow it while the
/// surface is alive (cleared on `OnSurfaceDestroyed`).  The wrapper implements
/// the `raw_window_handle` traits that `WgpuRenderer::new` requires.
#[derive(Clone, Copy)]
pub struct OhosNativeWindow {
    ptr: *mut c_void,
}

impl OhosNativeWindow {
    /// Wrap a raw `OHNativeWindow *`.  Returns `None` for a null pointer.
    pub fn from_ptr(ptr: *mut c_void) -> Option<Self> {
        NonNull::new(ptr).map(|p| Self { ptr: p.as_ptr() })
    }

    /// The raw `OHNativeWindow *`.
    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr
    }

    fn non_null(&self) -> Option<NonNull<c_void>> {
        NonNull::new(self.ptr)
    }
}

impl std::fmt::Debug for OhosNativeWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OhosNativeWindow")
            .field("ptr", &format_args!("{:p}", self.ptr))
            .finish()
    }
}

// SAFETY: the pointer is owned by the XComponent and only ever used on the
// main thread; `Send + Sync` are required so it can live inside the
// `Mutex<WindowState>`.
unsafe impl Send for OhosNativeWindow {}
unsafe impl Sync for OhosNativeWindow {}

impl HasWindowHandle for OhosNativeWindow {
    fn window_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
    {
        let ptr = self
            .non_null()
            .ok_or(raw_window_handle::HandleError::Unavailable)?;
        let handle = OhosNdkWindowHandle::new(ptr);
        Ok(
            unsafe {
                raw_window_handle::WindowHandle::borrow_raw(RawWindowHandle::OhosNdk(handle))
            },
        )
    }
}

impl HasDisplayHandle for OhosNativeWindow {
    fn display_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError>
    {
        let raw = RawDisplayHandle::Ohos(OhosDisplayHandle::new());
        // SAFETY: the OHOS display handle borrows no data.
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(raw) })
    }
}

// ── momentum state ────────────────────────────────────────────────────────────

/// Shared momentum scrolling state, accessible from both the touch callback
/// (which starts/cancels flings and records velocity samples) and the
/// request-frame callback (which pumps the decelerating animation).
///
/// As on Android, touch MOVE events **accumulate** a scroll delta which is
/// drained once per frame, avoiding a full layout+paint per move event.
struct MomentumState {
    velocity_tracker: VelocityTracker,
    scroller: MomentumScroller,
    pending_scroll_dx: f32,
    pending_scroll_dy: f32,
    pending_scroll_pos_x: f32,
    pending_scroll_pos_y: f32,
    has_pending_scroll: bool,
    pending_scroll_phase: gpui::TouchPhase,
}

// ── callback type aliases ─────────────────────────────────────────────────────

/// Called once per VSync tick to produce the next `Scene`.
pub type RequestFrameCallback = Box<dyn FnMut() + Send + 'static>;
/// Called when a touch event arrives.
pub type TouchCallback = Box<dyn FnMut(TouchPoint) + Send + 'static>;
/// Called when the window's active status changes.
pub type ActiveStatusCallback = Box<dyn FnMut(bool) + Send + 'static>;
/// Called when a key event arrives.
pub type KeyCallback = Box<dyn FnMut(OhosKeyEvent) + Send + 'static>;
/// Called when the window is resized.
pub type ResizeCallback = Box<dyn FnMut(Size<super::DevicePixels>, f32) + Send + 'static>;
/// Called when the window is destroyed (surface lost).
pub type CloseCallback = Box<dyn FnOnce() + Send + 'static>;
/// Called when the window appearance (light/dark) changes.
pub type AppearanceCallback = Box<dyn FnMut(WindowAppearance) + Send + 'static>;

// ── appearance ────────────────────────────────────────────────────────────────

/// Light / dark / high-contrast appearance of the window.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum WindowAppearance {
    #[default]
    Light,
    Dark,
    HighContrastLight,
    HighContrastDark,
}

// ── safe area ─────────────────────────────────────────────────────────────────

/// Safe area insets in physical (device) pixels.
#[derive(Debug, Clone, Copy, Default)]
pub struct SafeAreaInsets {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

impl SafeAreaInsets {
    /// Convert physical-pixel insets to logical pixels using the given scale.
    pub fn to_logical(&self, scale_factor: f32) -> SafeAreaInsets {
        SafeAreaInsets {
            top: self.top / scale_factor,
            bottom: self.bottom / scale_factor,
            left: self.left / scale_factor,
            right: self.right / scale_factor,
        }
    }
}

// ── window state ──────────────────────────────────────────────────────────────

struct WindowState {
    /// The `OHNativeWindow` handle.  `None` while the surface is unavailable
    /// (between `OnSurfaceDestroyed` and the next `OnSurfaceCreated`).
    native_window: Option<OhosNativeWindow>,

    /// Shared GPU context (instance + adapter + device + queue).
    gpu_context: GpuContext,

    /// The wgpu renderer from `gpui_wgpu`.  `None` while the surface is unavailable.
    renderer: Option<WgpuRenderer>,

    width: i32,
    height: i32,
    scale_factor: f32,

    /// Safe area insets in physical (device) pixels.
    safe_area_insets: SafeAreaInsets,

    appearance: WindowAppearance,
    is_active: bool,
    transparent: bool,

    request_frame_callback: Option<RequestFrameCallback>,
    touch_callback: Option<TouchCallback>,
    key_callback: Option<KeyCallback>,
    resize_callback: Option<ResizeCallback>,
    close_callback: Option<CloseCallback>,
    appearance_callback: Option<AppearanceCallback>,
    active_status_callback: Option<ActiveStatusCallback>,
}

// SAFETY: `WindowState` is only accessed while holding the `Mutex<WindowState>`
// lock, and all GPU work happens exclusively on the OHOS main thread.  The
// `GpuContext` (`Rc<RefCell<..>>`) never escapes to another thread; `Send` is
// required so `Arc<OhosPlatform>` satisfies the `Sync` bound of a static.
unsafe impl Send for WindowState {}

// ── OhosWindow ────────────────────────────────────────────────────────────────

/// A GPUI window on HarmonyOS.
pub struct OhosWindow {
    state: Arc<Mutex<WindowState>>,
    id: u64,
    active: Arc<std::sync::atomic::AtomicBool>,
}

// SAFETY: see `WindowState`.
unsafe impl Send for OhosWindow {}
unsafe impl Sync for OhosWindow {}

impl OhosWindow {
    // ── constructors ─────────────────────────────────────────────────────────

    /// Create an `OhosWindow` from an `OHNativeWindow`.
    ///
    /// Initialises the wgpu surface and renderer immediately.  `gpu_context`
    /// should be the shared context from the platform (pass a fresh
    /// `Rc::new(RefCell::new(None))` for the first window).
    pub fn new(
        native_window: OhosNativeWindow,
        gpu_context: GpuContext,
        width: i32,
        height: i32,
        scale_factor: f32,
        transparent: bool,
    ) -> Result<Arc<Self>> {
        log::info!(
            "OhosWindow::new — {}×{} scale={:.1} ptr={:p}",
            width,
            height,
            scale_factor,
            native_window.as_ptr(),
        );

        let renderer = Self::create_renderer(
            &native_window,
            Rc::clone(&gpu_context),
            width,
            height,
            transparent,
        )
        .context("failed to create gpui_wgpu renderer")?;

        let id = native_window.as_ptr() as u64;

        let state = Arc::new(Mutex::new(WindowState {
            native_window: Some(native_window),
            gpu_context,
            renderer: Some(renderer),
            width,
            height,
            scale_factor,
            safe_area_insets: SafeAreaInsets::default(),
            appearance: WindowAppearance::Light,
            is_active: true,
            transparent,
            request_frame_callback: None,
            touch_callback: None,
            key_callback: None,
            resize_callback: None,
            close_callback: None,
            appearance_callback: None,
            active_status_callback: None,
        }));

        Ok(Arc::new(Self {
            state,
            id,
            active: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }))
    }

    /// Create a headless `OhosWindow` for testing.  No wgpu surface is created.
    pub fn headless(width: i32, height: i32, scale_factor: f32) -> Arc<Self> {
        let state = Arc::new(Mutex::new(WindowState {
            native_window: None,
            gpu_context: Rc::new(RefCell::new(None)),
            renderer: None,
            width,
            height,
            scale_factor,
            safe_area_insets: SafeAreaInsets::default(),
            appearance: WindowAppearance::Light,
            is_active: false,
            transparent: false,
            request_frame_callback: None,
            touch_callback: None,
            key_callback: None,
            resize_callback: None,
            close_callback: None,
            appearance_callback: None,
            active_status_callback: None,
        }));

        Arc::new(Self {
            state,
            id: ((width as u64) << 32) | (height as u64),
            active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    // ── lifecycle ─────────────────────────────────────────────────────────────

    /// Called when `OnSurfaceCreated` / `OnSurfaceChanged` delivers a surface.
    pub fn init_window(
        &self,
        native_window: OhosNativeWindow,
        gpu_context: GpuContext,
        width: i32,
        height: i32,
    ) -> Result<()> {
        let mut state = self.state.lock();
        let transparent = state.transparent;

        // If a renderer already exists (kept alive across surface destruction),
        // just replace its surface.  This preserves the atlas and cached
        // texture IDs so GPUI's scene cache stays valid.
        if state.renderer.is_some() {
            let config = WgpuSurfaceConfig {
                size: gpui::size(gpui::DevicePixels(width), gpui::DevicePixels(height)),
                transparent,
                preferred_present_mode: Some(PREFERRED_PRESENT_MODE),
            };
            let instance = state
                .gpu_context
                .borrow()
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("gpu_context missing during surface replacement"))?
                .instance
                .clone();
            state
                .renderer
                .as_mut()
                .unwrap()
                .replace_surface(&native_window, config, &instance)?;
            log::info!(
                "OhosWindow::init_window — replaced surface {}×{}",
                width,
                height
            );
        } else {
            let ctx = if state.gpu_context.borrow().is_some() {
                Rc::clone(&state.gpu_context)
            } else {
                state.gpu_context = Rc::clone(&gpu_context);
                gpu_context
            };
            let renderer = Self::create_renderer(&native_window, ctx, width, height, transparent)?;
            state.renderer = Some(renderer);
            log::info!(
                "OhosWindow::init_window — created renderer {}×{}",
                width,
                height
            );
        }

        state.native_window = Some(native_window);
        state.width = width;
        state.height = height;
        state.is_active = true;
        self.active
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Called when `OnSurfaceDestroyed` fires.
    ///
    /// Unconfigures the surface but keeps the renderer (and its atlas) alive so
    /// rendering resumes seamlessly when a new surface is created.
    pub fn term_window(&self) {
        let mut state = self.state.lock();
        if let Some(ref mut renderer) = state.renderer {
            renderer.unconfigure_surface();
            log::info!("OhosWindow::term_window — surface unconfigured (renderer kept alive)");
        }
        state.native_window = None;
        state.is_active = false;
        self.active
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Called when `OnSurfaceChanged` reports new dimensions.
    pub fn handle_resize(&self, width: i32, height: i32) {
        let (changed, scale) = {
            let mut state = self.state.lock();
            if width == state.width && height == state.height {
                return;
            }
            log::info!(
                "OhosWindow resize: {}×{} → {}×{}",
                state.width,
                state.height,
                width,
                height
            );
            state.width = width;
            state.height = height;
            let scale = state.scale_factor;

            // `update_drawable_size` may block on the GPU; take the renderer out
            // so we don't hold the state lock during the call.
            if let Some(mut renderer) = state.renderer.take() {
                renderer.update_drawable_size(gpui::size(
                    gpui::DevicePixels(width),
                    gpui::DevicePixels(height),
                ));
                state.renderer = Some(renderer);
            }
            (true, scale)
        };

        if changed {
            let cb = {
                let mut state = self.state.lock();
                state.resize_callback.take()
            };
            if let Some(mut cb) = cb {
                cb(
                    Size {
                        width: super::DevicePixels(width),
                        height: super::DevicePixels(height),
                    },
                    scale,
                );
                let mut state = self.state.lock();
                if state.resize_callback.is_none() {
                    state.resize_callback = Some(cb);
                }
            }
        }
    }

    // ── drawing ───────────────────────────────────────────────────────────────

    fn scene_is_empty(scene: &gpui::Scene) -> bool {
        scene.shadows.is_empty()
            && scene.quads.is_empty()
            && scene.paths.is_empty()
            && scene.underlines.is_empty()
            && scene.monochrome_sprites.is_empty()
            && scene.subpixel_sprites.is_empty()
            && scene.polychrome_sprites.is_empty()
            && scene.surfaces.is_empty()
    }

    /// Draw `scene` into the window's next frame.  No-ops if there is no surface.
    pub fn draw(&self, scene: &gpui::Scene) {
        if Self::scene_is_empty(scene) {
            log::trace!("OhosWindow::draw — skipping empty scene");
            return;
        }

        // Take the renderer out so `renderer.draw()` (which can block on the
        // GL driver) runs without holding the state lock.
        let mut renderer = {
            let mut state = self.state.lock();
            match state.renderer.take() {
                Some(r) => r,
                None => return,
            }
        };

        renderer.draw(scene);

        let mut state = self.state.lock();
        state.renderer = Some(renderer);
    }

    /// Invoke the `request_frame_callback` if registered.
    ///
    /// Called from the vsync frame callback (`vsync.rs`).  The callback is
    /// taken out of the lock before invocation to avoid deadlocking with the
    /// `draw()` call it triggers.
    pub fn request_frame(&self) {
        let cb = {
            let mut state = self.state.lock();
            state.request_frame_callback.take()
        };

        if let Some(mut cb) = cb {
            cb();
            let mut state = self.state.lock();
            if state.request_frame_callback.is_none() {
                state.request_frame_callback = Some(cb);
            }
        }
    }

    // ── input delivery ────────────────────────────────────────────────────────

    /// Deliver a touch point to the registered touch callback.
    pub fn handle_touch(&self, point: TouchPoint) {
        let cb = {
            let mut state = self.state.lock();
            state.touch_callback.take()
        };
        if let Some(mut cb) = cb {
            cb(point);
            let mut state = self.state.lock();
            if state.touch_callback.is_none() {
                state.touch_callback = Some(cb);
            }
        } else {
            log::trace!(
                "handle_touch: no touch_callback registered (id={} action={})",
                point.id,
                point.action
            );
        }
    }

    /// Deliver a key event to the registered key callback.
    pub fn handle_key_event(&self, event: OhosKeyEvent) {
        let cb = {
            let mut state = self.state.lock();
            state.key_callback.take()
        };
        if let Some(mut cb) = cb {
            cb(event);
            let mut state = self.state.lock();
            if state.key_callback.is_none() {
                state.key_callback = Some(cb);
            }
        }
    }

    // ── appearance / active ───────────────────────────────────────────────────

    /// Update the window's appearance (e.g. after a dark-mode change).
    pub fn set_appearance(&self, appearance: WindowAppearance) {
        {
            let mut state = self.state.lock();
            if state.appearance == appearance {
                return;
            }
            state.appearance = appearance;
        }
        let cb = {
            let mut state = self.state.lock();
            state.appearance_callback.take()
        };
        if let Some(mut cb) = cb {
            cb(appearance);
            let mut state = self.state.lock();
            if state.appearance_callback.is_none() {
                state.appearance_callback = Some(cb);
            }
        }
    }

    /// Returns the current appearance.
    pub fn appearance(&self) -> WindowAppearance {
        self.state.lock().appearance
    }

    /// Update the active (foreground/background) status of the window.
    pub fn set_active(&self, active: bool) {
        use std::sync::atomic::Ordering;
        let prev = self.active.swap(active, Ordering::Relaxed);
        if prev == active {
            return;
        }
        log::info!("OhosWindow::set_active({}) — changed from {}", active, prev);

        let mut taken_cb: Option<Box<dyn FnMut(bool) + Send>> = None;
        if let Some(mut state) = self.state.try_lock() {
            state.is_active = active;
            taken_cb = state.active_status_callback.take();
        }
        if let Some(mut cb) = taken_cb {
            cb(active);
            if let Some(mut state) = self.state.try_lock() {
                state.active_status_callback = Some(cb);
            }
        }
    }

    // ── geometry ──────────────────────────────────────────────────────────────

    /// Physical size of the window in device pixels.
    pub fn physical_size(&self) -> Size<super::DevicePixels> {
        let state = self.state.lock();
        Size {
            width: super::DevicePixels(state.width),
            height: super::DevicePixels(state.height),
        }
    }

    /// Logical size of the window in density-independent pixels.
    pub fn logical_size(&self) -> Size<Pixels> {
        let state = self.state.lock();
        Size {
            width: Pixels(state.width as f32 / state.scale_factor),
            height: Pixels(state.height as f32 / state.scale_factor),
        }
    }

    /// Display scale factor (device pixels per logical pixel).
    pub fn scale_factor(&self) -> f32 {
        self.state.lock().scale_factor
    }

    /// Returns the current safe area insets in physical (device) pixels.
    pub fn safe_area_insets(&self) -> SafeAreaInsets {
        self.state.lock().safe_area_insets
    }

    /// Returns the current safe area insets in logical pixels.
    pub fn safe_area_insets_logical(&self) -> SafeAreaInsets {
        let state = self.state.lock();
        state.safe_area_insets.to_logical(state.scale_factor)
    }

    /// Override the safe area insets (physical pixels).
    ///
    /// Called from `napi.rs` when ArkTS forwards the window's avoid area.
    pub fn set_safe_area_insets(&self, insets: SafeAreaInsets) {
        log::info!(
            "OhosWindow safe_area_insets: top={:.0} bottom={:.0} left={:.0} right={:.0} (physical px)",
            insets.top,
            insets.bottom,
            insets.left,
            insets.right,
        );
        self.state.lock().safe_area_insets = insets;
    }

    // ── state queries ─────────────────────────────────────────────────────────

    /// Whether the window currently has an active wgpu surface.
    pub fn has_surface(&self) -> bool {
        self.state.lock().renderer.is_some()
    }

    /// Returns the sprite atlas from the renderer, if available.
    pub fn sprite_atlas(&self) -> Option<Arc<dyn PlatformAtlas>> {
        let state = self.state.lock();
        state
            .renderer
            .as_ref()
            .map(|r| r.sprite_atlas().clone() as Arc<dyn PlatformAtlas>)
    }

    /// Returns GPU specs from the renderer, if available.
    pub fn gpu_specs(&self) -> Option<GpuSpecs> {
        let state = self.state.lock();
        // gpui-pre 0.3.9 里 `WgpuRenderer::gpu_specs()` 自己就返回 `Option`
        // （渲染器还没建起来时没有 GPU 可报），所以这里要 `and_then` 而不是 `map`。
        state.renderer.as_ref().and_then(|r| r.gpu_specs())
    }

    /// Whether the window is currently active / visible.
    pub fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// A stable numeric identifier for this window.
    pub fn id(&self) -> u64 {
        self.id
    }

    // ── callback registration ─────────────────────────────────────────────────

    /// Register a callback invoked once per VSync tick.
    pub fn on_request_frame<F>(&self, cb: F)
    where
        F: FnMut() + Send + 'static,
    {
        self.state.lock().request_frame_callback = Some(Box::new(cb));
    }

    /// Register a callback invoked for each touch point.
    pub fn on_touch<F>(&self, cb: F)
    where
        F: FnMut(TouchPoint) + Send + 'static,
    {
        self.state.lock().touch_callback = Some(Box::new(cb));
    }

    /// Register a callback invoked for each key event.
    pub fn on_key_event<F>(&self, cb: F)
    where
        F: FnMut(OhosKeyEvent) + Send + 'static,
    {
        self.state.lock().key_callback = Some(Box::new(cb));
    }

    /// Register a callback invoked when the window is resized.
    pub fn on_resize<F>(&self, cb: F)
    where
        F: FnMut(Size<super::DevicePixels>, f32) + Send + 'static,
    {
        self.state.lock().resize_callback = Some(Box::new(cb));
    }

    /// Register a callback invoked once when the window surface is lost.
    pub fn on_close<F>(&self, cb: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.state.lock().close_callback = Some(Box::new(cb));
    }

    /// Register a callback invoked when the appearance changes.
    pub fn on_appearance_changed<F>(&self, cb: F)
    where
        F: FnMut(WindowAppearance) + Send + 'static,
    {
        self.state.lock().appearance_callback = Some(Box::new(cb));
    }

    /// Register a callback invoked when the active status changes.
    pub fn on_active_status_change<F>(&self, cb: F)
    where
        F: FnMut(bool) + Send + 'static,
    {
        self.state.lock().active_status_callback = Some(Box::new(cb));
    }

    // ── GPU introspection ─────────────────────────────────────────────────────

    /// Whether the GPU supports dual-source blending (subpixel text AA).
    pub fn supports_subpixel_aa(&self) -> bool {
        self.state
            .lock()
            .renderer
            .as_ref()
            .map(|r| r.supports_dual_source_blending())
            .unwrap_or(false)
    }

    // ── private helpers ───────────────────────────────────────────────────────

    /// Create a `WgpuRenderer` for the given `OHNativeWindow`.
    fn create_renderer(
        native_window: &OhosNativeWindow,
        gpu_context: GpuContext,
        width: i32,
        height: i32,
        transparent: bool,
    ) -> Result<WgpuRenderer> {
        let config = WgpuSurfaceConfig {
            size: gpui::size(gpui::DevicePixels(width), gpui::DevicePixels(height)),
            transparent,
            preferred_present_mode: Some(PREFERRED_PRESENT_MODE),
        };

        WgpuRenderer::new(gpu_context, native_window, config, None)
    }
}

impl Drop for OhosWindow {
    fn drop(&mut self) {
        let mut state = self.state.lock();
        if let Some(mut renderer) = state.renderer.take() {
            renderer.destroy();
        }
        state.native_window = None;
    }
}

// ── OhosPlatformWindow (PlatformWindow impl) ──────────────────────────────────

/// A wrapper around `Arc<OhosWindow>` implementing `gpui::PlatformWindow`.
#[allow(clippy::type_complexity)]
pub struct OhosPlatformWindow {
    window: Arc<OhosWindow>,
    display: Option<Rc<dyn PlatformDisplay>>,
    input_handler: Option<PlatformInputHandler>,
    title: String,
    momentum: Arc<Mutex<MomentumState>>,
    momentum_input_cb:
        Arc<Mutex<Box<dyn FnMut(gpui::PlatformInput) -> DispatchEventResult + Send>>>,
}

impl OhosPlatformWindow {
    /// Create an `OhosPlatformWindow` wrapping an existing `OhosWindow`.
    pub fn new(window: Arc<OhosWindow>, display: Option<Rc<dyn PlatformDisplay>>) -> Self {
        let noop_input_cb: Box<dyn FnMut(gpui::PlatformInput) -> DispatchEventResult + Send> =
            Box::new(|_| DispatchEventResult::default());
        Self {
            window,
            display,
            input_handler: None,
            title: String::new(),
            momentum: Arc::new(Mutex::new(MomentumState {
                velocity_tracker: VelocityTracker::new(),
                scroller: MomentumScroller::new(),
                pending_scroll_dx: 0.0,
                pending_scroll_dy: 0.0,
                pending_scroll_pos_x: 0.0,
                pending_scroll_pos_y: 0.0,
                has_pending_scroll: false,
                pending_scroll_phase: gpui::TouchPhase::Moved,
            })),
            momentum_input_cb: Arc::new(Mutex::new(noop_input_cb)),
        }
    }

    /// Access the underlying `OhosWindow`.
    pub fn inner(&self) -> &Arc<OhosWindow> {
        &self.window
    }
}

impl HasWindowHandle for OhosPlatformWindow {
    fn window_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
    {
        let state = self.window.state.lock();
        let nw = state
            .native_window
            .as_ref()
            .ok_or(raw_window_handle::HandleError::Unavailable)?;
        let ptr = NonNull::new(nw.as_ptr()).ok_or(raw_window_handle::HandleError::Unavailable)?;
        let handle = OhosNdkWindowHandle::new(ptr);
        Ok(
            unsafe {
                raw_window_handle::WindowHandle::borrow_raw(RawWindowHandle::OhosNdk(handle))
            },
        )
    }
}

impl HasDisplayHandle for OhosPlatformWindow {
    fn display_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError>
    {
        let raw = RawDisplayHandle::Ohos(OhosDisplayHandle::new());
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(raw) })
    }
}

impl PlatformWindow for OhosPlatformWindow {
    fn bounds(&self) -> gpui::Bounds<gpui::Pixels> {
        let state = self.window.state.lock();
        let w = state.width as f32 / state.scale_factor;
        let h = state.height as f32 / state.scale_factor;
        gpui::Bounds {
            origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
            size: gpui::size(gpui::px(w), gpui::px(h)),
        }
    }

    fn is_maximized(&self) -> bool {
        true
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Fullscreen(self.bounds())
    }

    fn content_size(&self) -> gpui::Size<gpui::Pixels> {
        self.bounds().size
    }

    fn resize(&mut self, _size: gpui::Size<gpui::Pixels>) {
        // HarmonyOS windows are sized by the system / ArkUI layout.
    }

    fn scale_factor(&self) -> f32 {
        self.window.scale_factor()
    }

    fn appearance(&self) -> gpui::WindowAppearance {
        match self.window.appearance() {
            WindowAppearance::Dark => gpui::WindowAppearance::Dark,
            WindowAppearance::HighContrastDark => gpui::WindowAppearance::VibrantDark,
            WindowAppearance::Light | WindowAppearance::HighContrastLight => {
                gpui::WindowAppearance::Light
            }
        }
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.display.clone()
    }

    fn mouse_position(&self) -> gpui::Point<gpui::Pixels> {
        gpui::Point::default()
    }

    fn modifiers(&self) -> Modifiers {
        Modifiers::default()
    }

    fn capslock(&self) -> Capslock {
        Capslock::default()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.input_handler = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.input_handler.take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<oneshot::Receiver<usize>> {
        // Native dialogs would require an ArkTS round-trip; not supported yet.
        None
    }

    fn activate(&self) {}

    fn is_active(&self) -> bool {
        self.window.is_active()
    }

    fn is_hovered(&self) -> bool {
        self.window.is_active()
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }

    fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
        log::debug!("OhosPlatformWindow::set_title({title})");
    }

    fn set_background_appearance(&self, _background: WindowBackgroundAppearance) {}

    fn minimize(&self) {
        log::debug!("OhosPlatformWindow::minimize — no-op on HarmonyOS");
    }

    fn zoom(&self) {}

    fn toggle_fullscreen(&self) {}

    fn is_fullscreen(&self) -> bool {
        true
    }

    fn frame_waker(&self) -> Option<Rc<dyn Fn()>> {
        // GPUI calls this when the window turns dirty; the vsync loop draws at the
        // next callback (`vsync.rs`). Without it there is no demand to arm on, and
        // an idle-but-armed loop is what wedges the main thread in
        // `eglSwapBuffers` once another window covers this one.
        Some(Rc::new(super::vsync::schedule))
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        // PlatformWindow gives Box<dyn FnMut(..)> (not Send); the callback is
        // always invoked on the OHOS main thread, so the transmute is safe.
        let send_callback: Box<dyn FnMut(RequestFrameOptions) + Send> =
            unsafe { std::mem::transmute(callback) };
        let send_callback = Mutex::new(send_callback);

        let momentum = Arc::clone(&self.momentum);
        let input_cb = Arc::clone(&self.momentum_input_cb);

        self.window.on_request_frame(move || {
            // 1. Drain coalesced touch-scroll deltas (at most one ScrollWheel/frame).
            {
                let mut ms = momentum.lock();

                if ms.has_pending_scroll {
                    let dx = ms.pending_scroll_dx;
                    let dy = ms.pending_scroll_dy;
                    let pos_x = ms.pending_scroll_pos_x;
                    let pos_y = ms.pending_scroll_pos_y;
                    let phase = ms.pending_scroll_phase;

                    ms.pending_scroll_dx = 0.0;
                    ms.pending_scroll_dy = 0.0;
                    ms.has_pending_scroll = false;
                    drop(ms);

                    let position = gpui::point(gpui::px(pos_x), gpui::px(pos_y));
                    if let Some(mut guard) = input_cb.try_lock() {
                        let _ = guard(gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                            position,
                            delta: gpui::ScrollDelta::Pixels(gpui::point(
                                gpui::px(dx),
                                gpui::px(dy),
                            )),
                            modifiers: gpui::Modifiers::default(),
                            touch_phase: phase,
                        }));
                    }
                } else if ms.scroller.is_active() {
                    // 2. Momentum scrolling pump.
                    if let Some(delta) = ms.scroller.step() {
                        let position =
                            gpui::point(gpui::px(delta.position_x), gpui::px(delta.position_y));
                        let fling_ended = !ms.scroller.is_active();
                        drop(ms);

                        if let Some(mut guard) = input_cb.try_lock() {
                            let _ =
                                guard(gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                                    position,
                                    delta: gpui::ScrollDelta::Pixels(gpui::point(
                                        gpui::px(delta.dx),
                                        gpui::px(delta.dy),
                                    )),
                                    modifiers: gpui::Modifiers::default(),
                                    touch_phase: gpui::TouchPhase::Moved,
                                }));
                            if fling_ended {
                                let _ = guard(gpui::PlatformInput::ScrollWheel(
                                    gpui::ScrollWheelEvent {
                                        position,
                                        delta: gpui::ScrollDelta::Pixels(gpui::point(
                                            gpui::px(0.0),
                                            gpui::px(0.0),
                                        )),
                                        modifiers: gpui::Modifiers::default(),
                                        touch_phase: gpui::TouchPhase::Ended,
                                    },
                                ));
                            }
                        }
                    } else {
                        let pos = gpui::point(
                            gpui::px(ms.scroller.position_x()),
                            gpui::px(ms.scroller.position_y()),
                        );
                        drop(ms);
                        if let Some(mut guard) = input_cb.try_lock() {
                            let _ =
                                guard(gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                                    position: pos,
                                    delta: gpui::ScrollDelta::Pixels(gpui::point(
                                        gpui::px(0.0),
                                        gpui::px(0.0),
                                    )),
                                    modifiers: gpui::Modifiers::default(),
                                    touch_phase: gpui::TouchPhase::Ended,
                                }));
                        }
                    }
                }
            }

            // 3. Force a render if text input arrived since the last frame.
            let text_dirty =
                crate::TEXT_INPUT_DIRTY.swap(false, std::sync::atomic::Ordering::AcqRel);

            let mut cb = send_callback.lock();
            cb(RequestFrameOptions {
                require_presentation: true,
                force_render: text_dirty,
                // gpui-pre 0.3.9 多了 `signal_at` / `signal_source` 两格（帧请求的
                // 计时与来源）。这一臂没有那份时间戳采集，留 `Default` 报「未知」，
                // 不编一个假的请求时间进去。
                ..Default::default()
            });
        });
    }

    fn on_input(&self, callback: Box<dyn FnMut(gpui::PlatformInput) -> DispatchEventResult>) {
        // PlatformWindow gives Box<dyn FnMut(..)> (not Send); the input
        // callbacks are always invoked on the OHOS main thread.
        let send_callback: Box<dyn FnMut(gpui::PlatformInput) -> DispatchEventResult + Send> =
            unsafe { std::mem::transmute(callback) };
        let input_cb = Arc::new(Mutex::new(send_callback));

        *self.momentum_input_cb.lock() = {
            let cb = Arc::clone(&input_cb);
            Box::new(move |input: gpui::PlatformInput| -> DispatchEventResult { cb.lock()(input) })
        };

        // ── Touch events → PlatformInput ─────────────────────────────────
        {
            let cb = Arc::clone(&input_cb);
            let scale_factor = self.window.scale_factor();
            let momentum = Arc::clone(&self.momentum);

            const SCROLL_SLOP: f32 = 8.0;

            #[derive(Clone, Copy, Debug)]
            enum TouchState {
                Idle,
                Pending { start_x: f32, start_y: f32 },
                Scrolling { prev_x: f32, prev_y: f32 },
            }

            let state = Mutex::new(TouchState::Idle);

            self.window.on_touch(move |touch| {
                // OHOS delivers touch coordinates in surface (device) pixels;
                // GPUI lays out in logical pixels.
                let logical_x = touch.x / scale_factor;
                let logical_y = touch.y / scale_factor;
                let modifiers = gpui::Modifiers::default();

                let mut ts = state.lock();

                match touch.action {
                    // DOWN
                    0 => {
                        {
                            let mut ms = momentum.lock();
                            ms.scroller.cancel();
                            ms.velocity_tracker.reset();
                            ms.pending_scroll_dx = 0.0;
                            ms.pending_scroll_dy = 0.0;
                            ms.has_pending_scroll = false;
                        }
                        *ts = TouchState::Pending {
                            start_x: logical_x,
                            start_y: logical_y,
                        };
                    }

                    // MOVE
                    2 => {
                        let mut ms = momentum.lock();
                        ms.velocity_tracker.record(logical_x, logical_y);

                        match *ts {
                            TouchState::Pending { start_x, start_y } => {
                                let dx = logical_x - start_x;
                                let dy = logical_y - start_y;
                                let distance = (dx * dx + dy * dy).sqrt();
                                if distance > SCROLL_SLOP {
                                    *ts = TouchState::Scrolling {
                                        prev_x: logical_x,
                                        prev_y: logical_y,
                                    };
                                    ms.pending_scroll_dx += dx;
                                    ms.pending_scroll_dy += dy;
                                    ms.pending_scroll_pos_x = logical_x;
                                    ms.pending_scroll_pos_y = logical_y;
                                    if !ms.has_pending_scroll {
                                        ms.pending_scroll_phase = gpui::TouchPhase::Started;
                                    }
                                    ms.has_pending_scroll = true;
                                }
                            }
                            TouchState::Scrolling { prev_x, prev_y } => {
                                let dx = logical_x - prev_x;
                                let dy = logical_y - prev_y;
                                *ts = TouchState::Scrolling {
                                    prev_x: logical_x,
                                    prev_y: logical_y,
                                };
                                ms.pending_scroll_dx += dx;
                                ms.pending_scroll_dy += dy;
                                ms.pending_scroll_pos_x = logical_x;
                                ms.pending_scroll_pos_y = logical_y;
                                if !ms.has_pending_scroll {
                                    ms.pending_scroll_phase = gpui::TouchPhase::Moved;
                                }
                                ms.has_pending_scroll = true;
                            }
                            TouchState::Idle => {}
                        }
                        drop(ms);

                        let position = gpui::point(gpui::px(logical_x), gpui::px(logical_y));
                        let mut guard = cb.lock();
                        let _ = guard(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                            position,
                            modifiers,
                            pressed_button: Some(gpui::MouseButton::Left),
                        }));
                    }

                    // UP / CANCEL
                    1 | 3 => {
                        let position = gpui::point(gpui::px(logical_x), gpui::px(logical_y));

                        match *ts {
                            TouchState::Pending { start_x, start_y } => {
                                {
                                    let mut ms = momentum.lock();
                                    ms.velocity_tracker.reset();
                                    ms.has_pending_scroll = false;
                                }
                                let tap_pos = gpui::point(gpui::px(start_x), gpui::px(start_y));
                                let mut guard = cb.lock();
                                let _ =
                                    guard(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                                        button: gpui::MouseButton::Left,
                                        position: tap_pos,
                                        modifiers,
                                        click_count: 1,
                                        first_mouse: false,
                                    }));
                                let _ = guard(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                                    button: gpui::MouseButton::Left,
                                    position: tap_pos,
                                    modifiers,
                                    click_count: 1,
                                }));
                            }
                            TouchState::Scrolling { prev_x, prev_y } => {
                                let dx = logical_x - prev_x;
                                let dy = logical_y - prev_y;
                                let mut ms = momentum.lock();

                                let total_dx = ms.pending_scroll_dx + dx;
                                let total_dy = ms.pending_scroll_dy + dy;
                                ms.pending_scroll_dx = 0.0;
                                ms.pending_scroll_dy = 0.0;
                                ms.has_pending_scroll = false;

                                let (vx, vy) = ms.velocity_tracker.velocity();
                                ms.velocity_tracker.reset();
                                ms.scroller.fling(vx, vy, logical_x, logical_y);
                                drop(ms);

                                let mut guard = cb.lock();
                                let _ = guard(gpui::PlatformInput::ScrollWheel(
                                    gpui::ScrollWheelEvent {
                                        position,
                                        delta: gpui::ScrollDelta::Pixels(gpui::point(
                                            gpui::px(total_dx),
                                            gpui::px(total_dy),
                                        )),
                                        modifiers,
                                        touch_phase: gpui::TouchPhase::Ended,
                                    },
                                ));
                                let _ = guard(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                                    button: gpui::MouseButton::Left,
                                    position,
                                    modifiers,
                                    click_count: 1,
                                }));
                            }
                            TouchState::Idle => {}
                        }
                        *ts = TouchState::Idle;
                    }

                    _ => {}
                }
            });
        }

        // ── Key events → PlatformInput ───────────────────────────────────
        {
            let cb = Arc::clone(&input_cb);
            self.window.on_key_event(move |key_event| {
                use super::keyboard::{
                    ohos_key_to_keystroke, KEY_ACTION_DOWN, KEY_ACTION_UP, KEY_DEL, KEY_DPAD_LEFT,
                    KEY_DPAD_RIGHT, KEY_MOVE_END, KEY_MOVE_HOME,
                };

                if key_event.action == KEY_ACTION_DOWN {
                    match key_event.key_code {
                        KEY_DEL => {
                            crate::dispatch_text_input("\x08");
                        }
                        KEY_DPAD_LEFT => {
                            crate::dispatch_text_input("\x1b[D");
                        }
                        KEY_DPAD_RIGHT => {
                            crate::dispatch_text_input("\x1b[C");
                        }
                        KEY_MOVE_HOME => {
                            crate::dispatch_text_input("\x1b[H");
                        }
                        KEY_MOVE_END => {
                            crate::dispatch_text_input("\x1b[F");
                        }
                        _ => {
                            if key_event.unicode_char != 0 {
                                if let Some(c) = char::from_u32(key_event.unicode_char) {
                                    let s = c.to_string();
                                    crate::dispatch_text_input(&s);
                                }
                            }
                        }
                    }
                }

                let keystroke = match ohos_key_to_keystroke(
                    key_event.key_code,
                    key_event.meta_state,
                    key_event.unicode_char,
                ) {
                    Some(ks) => ks,
                    None => return,
                };

                let event = if key_event.action == KEY_ACTION_DOWN {
                    gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                        keystroke,
                        is_held: false,
                        prefer_character_input: key_event.unicode_char != 0,
                    })
                } else if key_event.action == KEY_ACTION_UP {
                    gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke })
                } else {
                    return;
                };

                let mut guard = cb.lock();
                let _ = guard(event);
            });
        }
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        let send_callback: Box<dyn FnMut(bool) + Send> = unsafe { std::mem::transmute(callback) };
        let send_callback = Mutex::new(send_callback);
        self.window.on_active_status_change(move |active| {
            let mut cb = send_callback.lock();
            cb(active);
        });
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        let _callback = Mutex::new(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(gpui::Size<gpui::Pixels>, f32)>) {
        let send_callback: Box<dyn FnMut(gpui::Size<gpui::Pixels>, f32) + Send> =
            unsafe { std::mem::transmute(callback) };
        let send_callback = Arc::new(Mutex::new(send_callback));
        self.window.on_resize(move |device_size, scale| {
            let mut cb = send_callback.lock();
            cb(
                gpui::size(
                    gpui::px(device_size.width.0 as f32 / scale),
                    gpui::px(device_size.height.0 as f32 / scale),
                ),
                scale,
            );
        });
    }

    fn on_moved(&self, _callback: Box<dyn FnMut()>) {}

    fn on_should_close(&self, _callback: Box<dyn FnMut() -> bool>) {}

    fn on_hit_test_window_control(&self, _callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        let send_callback: Box<dyn FnOnce() + Send + 'static> =
            unsafe { std::mem::transmute::<Box<dyn FnOnce()>, Box<dyn FnOnce() + Send>>(callback) };
        self.window.on_close(send_callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        let send_callback: Box<dyn FnMut() + Send> = unsafe { std::mem::transmute(callback) };
        let send_callback = Mutex::new(send_callback);
        self.window.on_appearance_changed(move |_appearance| {
            let mut cb = send_callback.lock();
            cb();
        });
    }

    fn draw(&self, scene: &gpui::Scene) {
        self.window.draw(scene);
    }

    fn schedule_frame(&self) {
        // GPUI's own retry path: a frame that ended with the window still dirty
        // (an animation, a fling) or with next-frame callbacks pending raises
        // demand here instead of assuming the platform ticks regardless.
        super::vsync::schedule();
    }

    // gpui-pre 0.3.9 的 `PlatformWindow` 没有 `completed_frame` 这一格（Android 臂
    // 也没有），所以不实现。

    fn visibility(&self) -> WindowVisibility {
        // XComponent 没有「最小化」这一态：窗口在不在前台就是可不可见。
        if self.window.is_active() {
            WindowVisibility::Visible
        } else {
            WindowVisibility::Hidden
        }
    }

    fn on_visibility_change(&self, _callback: Box<dyn FnMut(WindowVisibility)>) {
        // 前后台切换由 ArkTS 的 onForeground / onBackground 递进来（走
        // `OhosWindow::set_active`），这一臂没有单独的可见性回调通道。
        // 接了不用等于没接，所以这里明确不存这个回调。
    }

    fn on_display_changed(&self, _callback: Box<dyn FnMut()>) {
        // 一个 XComponent 绑死一块屏；换屏由宿主重建 surface。
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.window
            .sprite_atlas()
            .unwrap_or_else(|| Arc::new(FallbackAtlas::new()))
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        self.window.supports_subpixel_aa()
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.window.gpu_specs()
    }

    fn update_ime_position(&self, _bounds: gpui::Bounds<gpui::Pixels>) {
        // The IME candidate window position is set from ArkTS (see `napi.rs`).
    }
}

// ── Fallback atlas ────────────────────────────────────────────────────────────

/// A minimal fallback `PlatformAtlas` used only when the renderer is not
/// available (e.g. before the wgpu surface is created).
struct FallbackAtlas {
    state: Mutex<FallbackAtlasState>,
}

struct FallbackAtlasState {
    next_id: u32,
    tiles: HashMap<AtlasKey, AtlasTile>,
}

impl FallbackAtlas {
    fn new() -> Self {
        Self {
            state: Mutex::new(FallbackAtlasState {
                next_id: 1,
                tiles: HashMap::new(),
            }),
        }
    }
}

impl PlatformAtlas for FallbackAtlas {
    fn get_or_insert_with<'a>(
        &self,
        // gpui-pre 0.3.9 的 `PlatformAtlas` 这里收的是值而不是引用。
        key: AtlasKey,
        build: &mut dyn FnMut() -> anyhow::Result<
            Option<(gpui::Size<gpui::DevicePixels>, std::borrow::Cow<'a, [u8]>)>,
        >,
    ) -> anyhow::Result<Option<AtlasTile>> {
        let mut state = self.state.lock();
        if let Some(tile) = state.tiles.get(&key) {
            return Ok(Some(tile.clone()));
        }

        let data = build()?;
        if let Some((size, _pixels)) = data {
            let id = state.next_id;
            state.next_id += 1;
            let tile = AtlasTile {
                texture_id: gpui::AtlasTextureId {
                    index: 0,
                    kind: gpui::AtlasTextureKind::Monochrome,
                },
                tile_id: gpui::TileId(id),
                padding: 0,
                bounds: gpui::Bounds {
                    origin: gpui::point(gpui::DevicePixels(0), gpui::DevicePixels(0)),
                    size,
                },
            };
            state.tiles.insert(key.clone(), tile.clone());
            Ok(Some(tile))
        } else {
            Ok(None)
        }
    }

    fn remove(&self, key: &AtlasKey) {
        self.state.lock().tiles.remove(key);
    }
}

// ── WindowList helper ─────────────────────────────────────────────────────────

/// Tracks all live `OhosWindow` instances in the process.
#[derive(Default)]
pub struct WindowList {
    windows: Vec<Arc<OhosWindow>>,
}

impl WindowList {
    /// Add a window to the list.
    pub fn push(&mut self, window: Arc<OhosWindow>) {
        self.windows.push(window);
    }

    /// Remove and return the window with the given `id`, if present.
    pub fn remove(&mut self, id: u64) -> Option<Arc<OhosWindow>> {
        if let Some(pos) = self.windows.iter().position(|w| w.id() == id) {
            Some(self.windows.remove(pos))
        } else {
            None
        }
    }

    /// Find a window by id (shared reference).
    pub fn get(&self, id: u64) -> Option<&Arc<OhosWindow>> {
        self.windows.iter().find(|w| w.id() == id)
    }

    /// Returns the first (primary) window, if any.
    pub fn primary(&self) -> Option<&Arc<OhosWindow>> {
        self.windows.first()
    }

    /// Iterate over all windows.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<OhosWindow>> {
        self.windows.iter()
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_window_geometry() {
        let w = OhosWindow::headless(1080, 1920, 3.0);
        assert_eq!(w.physical_size().width, super::super::DevicePixels(1080));
        assert_eq!(w.physical_size().height, super::super::DevicePixels(1920));
        assert!((w.scale_factor() - 3.0).abs() < f32::EPSILON);
        assert!(!w.has_surface());
        assert!(!w.is_active());
    }

    #[test]
    fn headless_window_logical_size() {
        let w = OhosWindow::headless(1080, 1920, 3.0);
        let ls = w.logical_size();
        assert!((ls.width.0 - 360.0).abs() < f32::EPSILON);
        assert!((ls.height.0 - 640.0).abs() < f32::EPSILON);
    }

    #[test]
    fn safe_area_to_logical() {
        let insets = SafeAreaInsets {
            top: 60.0,
            bottom: 30.0,
            left: 0.0,
            right: 0.0,
        };
        let logical = insets.to_logical(3.0);
        assert!((logical.top - 20.0).abs() < f32::EPSILON);
        assert!((logical.bottom - 10.0).abs() < f32::EPSILON);
    }

    #[test]
    fn window_list_primary() {
        let mut list = WindowList::default();
        list.push(OhosWindow::headless(1080, 1920, 2.0));
        assert_eq!(list.len(), 1);
        assert!(list.primary().is_some());
    }

    #[test]
    fn null_native_window_is_none() {
        assert!(OhosNativeWindow::from_ptr(std::ptr::null_mut()).is_none());
    }
}
