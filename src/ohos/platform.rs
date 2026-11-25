//! HarmonyOS platform implementation for GPUI.
//!
//! `OhosPlatform` ties together all HarmonyOS-specific sub-systems and
//! implements the platform-level contract that GPUI expects.  It mirrors the
//! role played by `AndroidPlatform` / `IosPlatform`.
//!
//! ## Architecture
//!
//! ```text
//! OhosPlatform
//!   ├── OhosDispatcher    — uv/vsync-driven main queue + Rust thread-pool
//!   ├── CosmicTextSystem  — cosmic-text + swash shaping / rasterisation
//!   ├── GpuContext        — shared wgpu device + queue (lazily initialised)
//!   ├── WindowList        — live OhosWindow instances
//!   └── DisplayList       — connected OhosDisplay instances
//! ```
//!
//! ## Lifecycle
//!
//! 1. `napi.rs` registers the native module and constructs the platform on the
//!    NAPI/UI thread.
//! 2. The example app calls `Application::with_platform(...).run(cb)`; on
//!    OHOS `Platform::run` **stores** `cb` and returns (it must not block the
//!    NAPI thread — same model as iOS).
//! 3. When the ArkUI `XComponent` reports `OnSurfaceCreated`, `napi.rs`
//!    creates an `OhosWindow` and invokes the stored `cb`, which opens the
//!    GPUI window via `Platform::open_window`.
//! 4. On surface destruction the renderer is unconfigured but kept alive.

use anyhow::Result;
use futures::channel::oneshot;
use gpui::{
    Action, ActivityGuard, AnyWindowHandle, AppLifecyclePhase, BackgroundExecutor, ClipboardItem,
    CursorStyle, Edges, ForegroundExecutor, KeybindingKeystroke, Keymap, Keystroke, Menu, MenuItem,
    PathPromptOptions, Platform, PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper,
    PlatformTextSystem, PlatformWindow, Task, ThermalState, WindowAppearance, WindowInsets,
    WindowParams,
};
use gpui_wgpu::CosmicTextSystem;
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use super::{
    dispatcher::OhosDispatcher,
    display::{DisplayList, OhosDisplay},
    window::{OhosNativeWindow, OhosWindow, WindowAppearance as OhosWindowAppearance, WindowList},
    OhosBackend,
};
use gpui_wgpu::GpuContext;

// ── stub: clipboard ───────────────────────────────────────────────────────────

/// HarmonyOS clipboard.
///
/// A full implementation would round-trip through ArkTS (`@ohos.pasteboard`).
/// Until then this is an in-process string store so the rest of the platform
/// compiles and the example works.
#[derive(Default)]
pub struct OhosClipboard {
    contents: Option<String>,
}

impl OhosClipboard {
    pub fn read(&self) -> Option<String> {
        self.contents.clone()
    }

    pub fn write(&mut self, text: String) {
        self.contents = Some(text);
    }

    pub fn clear(&mut self) {
        self.contents = None;
    }
}

// ── stub: credential store ────────────────────────────────────────────────────

/// HarmonyOS credential store.
///
/// Stubbed with an in-memory map; a real implementation would use the
/// HarmonyOS Distributed Data Object / HUKS keystore via ArkTS.
#[derive(Default)]
pub struct OhosCredentialStore {
    store: HashMap<(String, String), Vec<u8>>,
}

impl OhosCredentialStore {
    pub fn write(&mut self, service: &str, username: &str, password: &[u8]) -> Result<()> {
        self.store.insert(
            (service.to_string(), username.to_string()),
            password.to_vec(),
        );
        Ok(())
    }

    pub fn read(&self, service: &str, username: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .store
            .get(&(service.to_string(), username.to_string()))
            .cloned())
    }

    pub fn delete(&mut self, service: &str, username: &str) -> Result<()> {
        self.store
            .remove(&(service.to_string(), username.to_string()));
        Ok(())
    }
}

// ── software-keyboard bridge ──────────────────────────────────────────────────

/// Handler installed by `napi.rs` that forwards a software-keyboard request to
/// ArkTS (`Some(type)` = show, `None` = hide).
type KeyboardHandler = Box<dyn Fn(Option<crate::KeyboardType>) + Send + Sync + 'static>;

static KEYBOARD_HANDLER: Mutex<Option<KeyboardHandler>> = Mutex::new(None);

/// Handler installed by `napi.rs` that writes text to the ArkTS pasteboard.
type ClipboardWriteHandler = Box<dyn Fn(&str) + Send + Sync + 'static>;

static CLIPBOARD_WRITE_HANDLER: Mutex<Option<ClipboardWriteHandler>> = Mutex::new(None);

/// Handler installed by `napi.rs` that asks ArkTS to open a URL.
type OpenUrlHandler = Box<dyn Fn(&str) -> bool + Send + Sync + 'static>;

static OPEN_URL_HANDLER: Mutex<Option<OpenUrlHandler>> = Mutex::new(None);

/// Handler installed by `napi.rs` that asks ArkTS to open the native file
/// picker.  The three arguments are `(directories, save, suggested_name)`.
type PickPathsHandler = Box<dyn Fn(bool, bool, Option<&str>) + Send + Sync + 'static>;

static PICK_PATHS_HANDLER: Mutex<Option<PickPathsHandler>> = Mutex::new(None);

/// A credential-store operation forwarded to ArkTS.
#[derive(Clone, Copy, Debug)]
pub enum CredentialOp<'a> {
    Read,
    Write(&'a [u8]),
    Delete,
}

/// Handler for platform credential storage through ArkTS preferences/HUKS.
type CredentialHandler = Box<
    dyn for<'a> Fn(&str, &str, CredentialOp<'a>) -> Result<Option<Vec<u8>>> + Send + Sync + 'static,
>;

static CREDENTIAL_HANDLER: Mutex<Option<CredentialHandler>> = Mutex::new(None);

/// Result channel for a native path picker request.  GPUI's "existing paths"
/// prompt accepts multiple results, while "new path" returns exactly one.
enum PendingPicker {
    Paths(oneshot::Sender<Result<Option<Vec<PathBuf>>>>),
    NewPath(oneshot::Sender<Result<Option<PathBuf>>>),
}

/// Install the ArkTS clipboard write bridge (called from `napi.rs`).
pub fn set_clipboard_write_handler(handler: ClipboardWriteHandler) {
    *CLIPBOARD_WRITE_HANDLER.lock() = Some(handler);
}

fn request_clipboard_write(text: &str) {
    if let Some(handler) = CLIPBOARD_WRITE_HANDLER.lock().as_ref() {
        handler(text);
    } else {
        log::debug!("clipboard write: no ArkTS handler installed yet");
    }
}

/// Install the ArkTS URL-launch bridge (called from `napi.rs`).
pub fn set_open_url_handler(handler: OpenUrlHandler) {
    *OPEN_URL_HANDLER.lock() = Some(handler);
}

/// Ask ArkTS to open `url`; `false` means no handler is installed yet.
pub fn request_open_url(url: &str) -> bool {
    if let Some(handler) = OPEN_URL_HANDLER.lock().as_ref() {
        handler(url)
    } else {
        log::debug!("open_url: no ArkTS handler installed yet");
        false
    }
}

/// Install the ArkTS file-picker bridge (called from `napi.rs`).
pub fn set_pick_paths_handler(handler: PickPathsHandler) {
    *PICK_PATHS_HANDLER.lock() = Some(handler);
}

/// Ask ArkTS to show a file picker.  Results are returned through
/// `napi::native_picker_result`.
pub fn request_pick_paths(directories: bool, save: bool, suggested_name: Option<&str>) -> bool {
    if let Some(handler) = PICK_PATHS_HANDLER.lock().as_ref() {
        handler(directories, save, suggested_name);
        true
    } else {
        log::debug!("pick_paths: no ArkTS handler installed yet");
        false
    }
}

/// Install the ArkTS credential-store bridge (called from `napi.rs`).
pub fn set_credential_handler(handler: CredentialHandler) {
    *CREDENTIAL_HANDLER.lock() = Some(handler);
}

fn has_credential_handler() -> bool {
    CREDENTIAL_HANDLER.lock().is_some()
}

fn credential_op(service: &str, username: &str, op: CredentialOp<'_>) -> Result<Option<Vec<u8>>> {
    if let Some(handler) = CREDENTIAL_HANDLER.lock().as_ref() {
        handler(service, username, op)
    } else {
        Ok(None)
    }
}

/// Install the ArkTS keyboard bridge (called from `napi.rs`).
pub fn set_keyboard_handler(handler: KeyboardHandler) {
    *KEYBOARD_HANDLER.lock() = Some(handler);
}

/// Request that the software keyboard be shown.
pub fn request_show_keyboard(keyboard_type: crate::KeyboardType) {
    if let Some(handler) = KEYBOARD_HANDLER.lock().as_ref() {
        handler(Some(keyboard_type));
    } else {
        log::debug!("show_keyboard: no ArkTS keyboard handler installed yet");
    }
}

/// Request that the software keyboard be hidden.
pub fn request_hide_keyboard() {
    if let Some(handler) = KEYBOARD_HANDLER.lock().as_ref() {
        handler(None);
    }
}

fn appearance_from_mode(mode: u32) -> WindowAppearance {
    match mode {
        1 => WindowAppearance::Dark,
        2 => WindowAppearance::VibrantLight,
        3 => WindowAppearance::VibrantDark,
        _ => WindowAppearance::Light,
    }
}

fn appearance_to_window(appearance: WindowAppearance) -> OhosWindowAppearance {
    match appearance {
        WindowAppearance::Light => OhosWindowAppearance::Light,
        WindowAppearance::Dark => OhosWindowAppearance::Dark,
        WindowAppearance::VibrantLight => OhosWindowAppearance::HighContrastLight,
        WindowAppearance::VibrantDark => OhosWindowAppearance::HighContrastDark,
    }
}

// ── platform state ────────────────────────────────────────────────────────────

struct OhosPlatformState {
    dispatcher: Arc<OhosDispatcher>,
    gpu_context: GpuContext,
    windows: WindowList,
    displays: DisplayList,
    text_system: Arc<CosmicTextSystem>,
    clipboard: OhosClipboard,
    credentials: OhosCredentialStore,

    /// The `on_finish_launching` closure passed to `run()`.
    finish_launching: Option<Box<dyn FnOnce() + Send>>,

    // gpui-pre 0.3.9 的 `Platform::on_quit` 要的是 `FnMut() -> bool`（返回「可以退」），
    // 与 Android 臂同一条。
    quit_callback: Option<Box<dyn FnMut() -> bool + Send>>,
    reopen_callback: Option<Box<dyn FnMut() + Send>>,
    open_urls_callback: Option<Box<dyn FnMut(Vec<String>) + Send>>,
    keyboard_layout_callback: Option<Box<dyn FnMut() + Send>>,
    lifecycle_callback: Option<Box<dyn FnMut(AppLifecyclePhase) + Send>>,
    memory_warning_callback: Option<Box<dyn FnMut() + Send>>,
    appearance: WindowAppearance,
    insets: Option<WindowInsets>,

    /// Pending native file-picker request, completed by `napi::native_picker_result`.
    pending_picker: Option<PendingPicker>,

    is_active: bool,
    headless: bool,
    preferred_backend: OhosBackend,
}

// SAFETY: `OhosPlatformState` is only accessed while holding the
// `Mutex<OhosPlatformState>` lock, and all GPU work (including any use of
// `GpuContext = Rc<RefCell<Option<WgpuContext>>>`) happens exclusively on the
// OHOS main thread.  The `Rc` never escapes to another thread; we only need
// `Send` so that `Arc<OhosPlatform>` satisfies the `Sync` bound required by
// `static OnceLock<Arc<OhosPlatform>>`.
unsafe impl Send for OhosPlatformState {}

// ── OhosPlatform ──────────────────────────────────────────────────────────────

/// The GPUI platform implementation for HarmonyOS.
pub struct OhosPlatform {
    state: Mutex<OhosPlatformState>,
    should_quit: AtomicBool,
    /// GPUI 的 quit 回调在它借用着 `App` 的那一刻返回 false（从动作处理函数里退，
    /// 比如返回手势）。这一臂的 main loop 不会自己再回调一次，所以把请求停在这里，
    /// 由 [`OhosPlatform::should_quit`] 每轮重试 —— 与 Android 臂同一条。
    quit_pending: AtomicBool,
}

/// System font paths probed at startup (ordered by preference).
const SYSTEM_FONT_PATHS: &[&str] = &[
    // HarmonyOS Sans (Latin + CJK)
    "/system/fonts/HarmonyOS_Sans_Regular.ttf",
    "/system/fonts/HarmonyOS_Sans_Medium.ttf",
    "/system/fonts/HarmonyOS_Sans_Bold.ttf",
    "/system/fonts/HarmonyOS_Sans_Light.ttf",
    "/system/fonts/HarmonyOS_Sans_Italic.ttf",
    "/system/fonts/HarmonyOS_Sans_SC_Regular.ttf",
    "/system/fonts/HarmonyOS_Sans_SC_Medium.ttf",
    "/system/fonts/HarmonyOS_Sans_SC_Bold.ttf",
    "/system/fonts/HarmonyOS_Sans_TC_Regular.ttf",
    // Non-Latin HarmonyOS Sans variants
    "/system/fonts/HarmonyOS_Sans_Naskh_Arabic_Regular.ttf",
    "/system/fonts/HarmonyOS_Sans_Devanagari_Regular.ttf",
    "/system/fonts/HarmonyOS_Sans_Thai_Regular.ttf",
    "/system/fonts/HarmonyOS_Sans_Hebrew_Regular.ttf",
    // Generic Noto fallbacks (also present on some OHOS builds)
    "/system/fonts/NotoSans-Regular.ttf",
    "/system/fonts/NotoSans-Bold.ttf",
    "/system/fonts/NotoSansMono-Regular.ttf",
    // Emoji (CBDT/COLR depending on build)
    "/system/fonts/NotoColorEmoji.ttf",
    "/system/fonts/HarmonyOS_Sans_Emoji.ttf",
];

impl OhosPlatform {
    // ── constructors ─────────────────────────────────────────────────────────

    /// Create a new `OhosPlatform`.
    ///
    /// `headless` — when `true`, no real surface is created.  Useful for
    /// off-screen rendering and unit tests.
    pub fn new(headless: bool) -> Self {
        crate::ohos::init_logger();

        let dispatcher = OhosDispatcher::new();
        let text_system = Arc::new(CosmicTextSystem::new_without_system_fonts("HarmonyOS Sans"));

        // Load system fonts explicitly: cosmic-text's fontdb is not populated
        // (we used `new_without_system_fonts`) because OHOS font paths are not
        // scanned by the upstream implementation.
        {
            let mut font_data: Vec<std::borrow::Cow<'static, [u8]>> = Vec::new();
            for path in SYSTEM_FONT_PATHS {
                match std::fs::read(path) {
                    Ok(bytes) => {
                        log::info!("loaded system font: {path} ({} bytes)", bytes.len());
                        font_data.push(std::borrow::Cow::Owned(bytes));
                    }
                    Err(e) => log::debug!("skipping system font {path}: {e}"),
                }
            }

            if font_data.is_empty() {
                log::warn!("no system fonts found under /system/fonts/ — text may not render");
            } else if let Err(e) = text_system.add_fonts(font_data) {
                log::warn!("failed to add system fonts: {e:#}");
            }
        }

        let displays = if headless {
            DisplayList::single(OhosDisplay::headless(1080, 1920))
        } else {
            DisplayList::single(OhosDisplay::headless(0, 0))
        };

        log::info!("OhosPlatform::new — headless={headless}");

        Self {
            state: Mutex::new(OhosPlatformState {
                dispatcher,
                gpu_context: Rc::new(RefCell::new(None)),
                windows: WindowList::default(),
                displays,
                text_system,
                clipboard: OhosClipboard::default(),
                credentials: OhosCredentialStore::default(),
                finish_launching: None,
                quit_callback: None,
                reopen_callback: None,
                open_urls_callback: None,
                keyboard_layout_callback: None,
                lifecycle_callback: None,
                memory_warning_callback: None,
                appearance: appearance_from_mode(super::appearance_mode()),
                insets: None,
                pending_picker: None,
                is_active: false,
                headless,
                preferred_backend: OhosBackend::Gles,
            }),
            should_quit: AtomicBool::new(false),
            quit_pending: AtomicBool::new(false),
        }
    }

    // ── executors ─────────────────────────────────────────────────────────────

    /// Returns the shared dispatcher.
    pub fn dispatcher(&self) -> Arc<OhosDispatcher> {
        self.state.lock().dispatcher.clone()
    }

    /// Returns the background-task executor.
    pub fn background_executor(&self) -> Arc<OhosDispatcher> {
        self.dispatcher()
    }

    /// Returns the foreground-task executor.
    pub fn foreground_executor(&self) -> Arc<OhosDispatcher> {
        self.dispatcher()
    }

    /// Returns the text system.
    pub fn text_system(&self) -> Arc<CosmicTextSystem> {
        self.state.lock().text_system.clone()
    }

    // ── lifecycle ─────────────────────────────────────────────────────────────

    /// Store the `on_finish_launching` callback.
    ///
    /// Unlike Android, `Platform::run` on HarmonyOS must **not** block the
    /// NAPI/UI thread — the ArkTS runtime needs that thread to deliver the
    /// `XComponent` surface callbacks.  The callback is therefore stored and
    /// invoked later from `napi.rs` once the surface is ready (the same model
    /// as iOS).
    pub fn run(&self, on_finish_launching: Box<dyn FnOnce() + Send + 'static>) {
        let mut state = self.state.lock();
        state.finish_launching = Some(on_finish_launching);
        drop(state);

        log::info!("OhosPlatform::run — callback stored (non-blocking)");

        // Headless / test mode: no surface will ever arrive, so invoke now.
        if self.state.lock().headless {
            if let Some(cb) = self.take_finish_launching_callback() {
                cb();
            }
        }
    }

    /// Take the stored `on_finish_launching` callback, if any.
    ///
    /// Called from `napi.rs` once the `XComponent` surface has been created
    /// and an `OhosWindow` exists, so the GPUI `Application` can open its
    /// first window.
    pub fn take_finish_launching_callback(&self) -> Option<Box<dyn FnOnce() + Send>> {
        self.state.lock().finish_launching.take()
    }

    /// Request a graceful quit.
    pub fn quit(&self) {
        log::info!("OhosPlatform::quit");
        if self.try_quit_callback() {
            self.should_quit.store(true, Ordering::SeqCst);
            self.quit_pending.store(false, Ordering::SeqCst);
        } else {
            self.quit_pending.store(true, Ordering::SeqCst);
        }
    }

    /// 跑一次 quit 回调，回报「现在能不能退」。回调要能再调回平台，所以先把它从锁里
    /// 取出来、跑完再放回去。
    fn try_quit_callback(&self) -> bool {
        let mut callback = self.state.lock().quit_callback.take();
        let can_quit = callback.as_mut().map_or(true, |callback| callback());
        if let Some(callback) = callback {
            self.state.lock().quit_callback.get_or_insert(callback);
        }
        can_quit
    }

    /// Returns `true` if `quit()` has been called. While a quit is deferred
    /// (GPUI 借用了 `App` 的那一轮), retry the callback so it still completes.
    pub fn should_quit(&self) -> bool {
        if !self.should_quit.load(Ordering::Relaxed)
            && self.quit_pending.load(Ordering::Relaxed)
            && self.try_quit_callback()
        {
            self.should_quit.store(true, Ordering::SeqCst);
            self.quit_pending.store(false, Ordering::SeqCst);
        }
        self.should_quit.load(Ordering::Relaxed)
    }

    /// Called when the app moves to the foreground.
    pub fn did_become_active(&self) {
        self.state.lock().is_active = true;
    }

    /// Called when the app moves to the background.
    pub fn did_enter_background(&self) {
        self.state.lock().is_active = false;
    }

    /// Deliver URLs from a deeplink.
    pub fn deliver_open_urls(&self, urls: Vec<String>) {
        if let Some(cb) = self.state.lock().open_urls_callback.as_mut() {
            cb(urls);
        }
    }

    /// Deliver a "reopen" event.
    pub fn deliver_reopen(&self) {
        if let Some(cb) = self.state.lock().reopen_callback.as_mut() {
            cb();
        }
    }

    /// Notify that the keyboard layout changed.
    pub fn notify_keyboard_layout_change(&self) {
        if let Some(cb) = self.state.lock().keyboard_layout_callback.as_mut() {
            cb();
        }
    }

    // ── window management ─────────────────────────────────────────────────────

    /// Create an `OhosWindow` from a raw `OHNativeWindow *`.
    ///
    /// Called from `napi.rs` when `OnSurfaceCreated` fires.
    pub fn open_native_window(
        &self,
        native_window: OhosNativeWindow,
        width: i32,
        height: i32,
        scale_factor: f32,
        transparent: bool,
    ) -> Result<Arc<OhosWindow>> {
        let mut state = self.state.lock();
        let window = OhosWindow::new(
            native_window,
            Rc::clone(&state.gpu_context),
            width,
            height,
            scale_factor,
            transparent,
        )?;
        let appearance = appearance_to_window(state.appearance);
        window.set_appearance(appearance);
        state.windows.push(Arc::clone(&window));
        log::info!(
            "OhosPlatform::open_native_window — id={:#x} {}×{} scale={:.1}",
            window.id(),
            width,
            height,
            scale_factor
        );
        Ok(window)
    }

    /// Remove and return the window identified by `id`.
    pub fn close_window(&self, id: u64) -> Option<Arc<OhosWindow>> {
        self.state.lock().windows.remove(id)
    }

    /// Returns the primary (first) window, if any.
    pub fn primary_window(&self) -> Option<Arc<OhosWindow>> {
        self.state.lock().windows.primary().cloned()
    }

    /// Returns the number of live windows.
    pub fn window_count(&self) -> usize {
        self.state.lock().windows.len()
    }

    /// Get a clone of the shared GPU context handle.
    pub fn gpu_context(&self) -> GpuContext {
        Rc::clone(&self.state.lock().gpu_context)
    }

    // ── display management ────────────────────────────────────────────────────

    /// Returns all connected displays.
    pub fn displays_list(&self) -> Vec<OhosDisplay> {
        self.state.lock().displays.all().to_vec()
    }

    /// Returns the primary display.
    pub fn primary_display_impl(&self) -> Option<OhosDisplay> {
        self.state.lock().displays.primary().cloned()
    }

    /// Update the primary display geometry (from `OnSurfaceChanged` or ArkTS).
    pub fn update_primary_display(&self, width: i32, height: i32, density_dpi: i32) {
        let display = OhosDisplay::new(width, height, density_dpi);
        self.state.lock().displays = DisplayList::single(display);
    }

    // ── clipboard / credentials ───────────────────────────────────────────────

    pub fn write_to_clipboard_impl(&self, text: String) {
        // Keep the local cache so reads on the same frame see the write, and
        // mirror it to the real OHOS pasteboard through ArkTS.
        self.state.lock().clipboard.write(text.clone());
        request_clipboard_write(&text);
    }

    /// Called from `napi.rs` when ArkTS reports the current pasteboard content.
    pub fn clipboard_changed(&self, text: String) {
        self.state.lock().clipboard.write(text);
    }

    /// Complete the pending picker request from the ArkTS bridge.  A "new
    /// path" request only consumes the first returned path.
    pub fn finish_picker(&self, paths: Result<Option<Vec<PathBuf>>>) {
        match self.state.lock().pending_picker.take() {
            Some(PendingPicker::Paths(tx)) => {
                let _ = tx.send(paths);
            }
            Some(PendingPicker::NewPath(tx)) => {
                let first = paths.map(|paths| {
                    paths.and_then(|mut p| {
                        if p.is_empty() {
                            None
                        } else {
                            Some(p.remove(0))
                        }
                    })
                });
                let _ = tx.send(first);
            }
            None => {}
        }
    }

    /// Apply a density change to the primary window.
    pub fn apply_density(&self, scale_factor: f32) {
        if let Some(win) = self.primary_window() {
            win.set_scale_factor(scale_factor);
        }
    }

    /// Refresh logical window insets from the values ArkTS publishes.
    pub fn refresh_insets(&self) {
        let (top, bottom, left, right) = super::safe_area_insets();
        let ime_bottom = crate::keyboard_height();
        let insets = WindowInsets {
            safe_area: Edges {
                top: gpui::px(top),
                right: gpui::px(right),
                bottom: gpui::px(bottom),
                left: gpui::px(left),
            },
            ime: Edges {
                top: gpui::px(0.0),
                right: gpui::px(0.0),
                bottom: gpui::px(ime_bottom),
                left: gpui::px(0.0),
            },
        };
        {
            let mut state = self.state.lock();
            if state.insets.as_ref() == Some(&insets) {
                return;
            }
            state.insets = Some(insets.clone());
        }
        if let Some(win) = self.primary_window() {
            win.set_insets(insets);
        }
    }

    /// Report an application lifecycle phase change from ArkTS.
    pub fn deliver_lifecycle(&self, phase: AppLifecyclePhase) {
        let mut callback = self.state.lock().lifecycle_callback.take();
        if let Some(cb) = callback.as_mut() {
            cb(phase);
        }
        if let Some(cb) = callback {
            self.state.lock().lifecycle_callback.get_or_insert(cb);
        }
    }

    /// Report system memory pressure from ArkTS.
    pub fn deliver_memory_warning(&self) {
        let mut callback = self.state.lock().memory_warning_callback.take();
        if let Some(cb) = callback.as_mut() {
            cb();
        }
        if let Some(cb) = callback {
            self.state.lock().memory_warning_callback.get_or_insert(cb);
        }
    }

    /// Update the platform/window appearance from ArkTS colour-mode changes.
    pub fn set_appearance(&self, appearance: WindowAppearance) {
        self.state.lock().appearance = appearance;
        let window_appearance = appearance_to_window(appearance);
        if let Some(win) = self.primary_window() {
            win.set_appearance(window_appearance);
        }
    }

    pub fn read_from_clipboard_impl(&self) -> Option<String> {
        self.state.lock().clipboard.read()
    }

    pub fn write_credentials_impl(
        &self,
        service: &str,
        username: &str,
        password: &[u8],
    ) -> Result<()> {
        if has_credential_handler() {
            credential_op(service, username, CredentialOp::Write(password))?;
            return Ok(());
        }
        self.state
            .lock()
            .credentials
            .write(service, username, password)
    }

    pub fn read_credentials_impl(&self, service: &str, username: &str) -> Result<Option<Vec<u8>>> {
        if has_credential_handler() {
            return credential_op(service, username, CredentialOp::Read);
        }
        self.state.lock().credentials.read(service, username)
    }

    pub fn delete_credentials_impl(&self, service: &str, username: &str) -> Result<()> {
        if has_credential_handler() {
            credential_op(service, username, CredentialOp::Delete)?;
            return Ok(());
        }
        self.state.lock().credentials.delete(service, username)
    }

    // ── misc ──────────────────────────────────────────────────────────────────

    /// Whether the platform is running in headless mode.
    pub fn is_headless(&self) -> bool {
        self.state.lock().headless
    }

    /// Whether the app is currently in the foreground.
    pub fn is_active(&self) -> bool {
        self.state.lock().is_active
    }

    /// Preferred wgpu backend.
    pub fn preferred_backend(&self) -> OhosBackend {
        self.state.lock().preferred_backend
    }

    /// Override the preferred wgpu backend.
    pub fn set_preferred_backend(&self, backend: OhosBackend) {
        self.state.lock().preferred_backend = backend;
    }

    // ── callback registration ─────────────────────────────────────────────────

    pub fn on_quit<F>(&self, mut cb: F)
    where
        F: FnMut() + Send + 'static,
    {
        // 这个内在口是「平台真要退了通知你一声」，不是 GPUI 那条要返回 bool 的
        // `Platform::on_quit`，所以恒答 true —— 与 Android 臂同一条。
        self.state.lock().quit_callback = Some(Box::new(move || {
            cb();
            true
        }));
    }

    pub fn on_reopen<F>(&self, cb: F)
    where
        F: FnMut() + Send + 'static,
    {
        self.state.lock().reopen_callback = Some(Box::new(cb));
    }

    pub fn on_open_urls<F>(&self, cb: F)
    where
        F: FnMut(Vec<String>) + Send + 'static,
    {
        self.state.lock().open_urls_callback = Some(Box::new(cb));
    }

    pub fn on_keyboard_layout_change<F>(&self, cb: F)
    where
        F: FnMut() + Send + 'static,
    {
        self.state.lock().keyboard_layout_callback = Some(Box::new(cb));
    }

    // ── frame loop ────────────────────────────────────────────────────────────

    /// Advance the dispatcher by one tick (promote delayed tasks + drain the
    /// main-thread queue).  Called from the vsync frame loop.
    pub fn tick(&self) {
        // Clone the dispatcher and drop the state lock before running tasks —
        // drained tasks may call back into the platform (which needs the lock).
        let dispatcher = self.state.lock().dispatcher.clone();
        dispatcher.tick();
    }

    /// Drain all pending main-thread tasks synchronously.
    pub fn flush_main_thread_tasks(&self) {
        let dispatcher = self.state.lock().dispatcher.clone();
        dispatcher.flush_main_thread_tasks();
    }
}

// ── impl Platform ─────────────────────────────────────────────────────────────

impl Platform for OhosPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        let dispatcher: Arc<dyn gpui::PlatformDispatcher> = self.state.lock().dispatcher.clone();
        BackgroundExecutor::new(dispatcher)
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        let dispatcher: Arc<dyn gpui::PlatformDispatcher> = self.state.lock().dispatcher.clone();
        ForegroundExecutor::new(dispatcher)
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.state.lock().text_system.clone()
    }

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        // `Platform::run` gives us `Box<dyn FnOnce()>` (not Send).  On OHOS
        // this is always called on the main/NAPI thread.
        let send_callback: Box<dyn FnOnce() + Send> =
            unsafe { std::mem::transmute(on_finish_launching) };
        OhosPlatform::run(self, send_callback);
    }

    fn quit(&self) {
        OhosPlatform::quit(self);
    }

    fn restart(&self, _binary_path: Option<PathBuf>, _arguments: Vec<std::ffi::OsString>) {
        log::warn!("OhosPlatform::restart — not supported on HarmonyOS");
    }

    fn activate(&self, _ignoring_other_apps: bool) {
        self.state.lock().is_active = true;
    }

    fn hide(&self) {}

    fn hide_other_apps(&self) {}

    fn unhide_other_apps(&self) {}

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        let state = self.state.lock();
        state
            .displays
            .all()
            .iter()
            .map(|d| Rc::new(d.clone()) as Rc<dyn PlatformDisplay>)
            .collect()
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.state
            .lock()
            .displays
            .primary()
            .map(|d| Rc::new(d.clone()) as Rc<dyn PlatformDisplay>)
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        None
    }

    fn open_window(
        &self,
        _handle: AnyWindowHandle,
        _options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        // The native window is created by the ArkUI XComponent, not by the
        // application.  Wrap the existing primary window for GPUI.
        let window = self.primary_window().ok_or_else(|| {
            anyhow::anyhow!(
                "OhosPlatform::open_window — no native surface available yet. \
                 Call this from the on-surface-created callback."
            )
        })?;

        let display = self
            .state
            .lock()
            .displays
            .primary()
            .map(|d| Rc::new(d.clone()) as Rc<dyn PlatformDisplay>);

        Ok(Box::new(super::window::OhosPlatformWindow::new(
            window, display,
        )))
    }

    fn window_appearance(&self) -> WindowAppearance {
        self.state.lock().appearance
    }

    fn open_url(&self, url: &str) {
        if !request_open_url(url) {
            log::warn!("OhosPlatform::open_url({url}) — ArkTS launch bridge unavailable");
        }
    }

    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        self.state.lock().open_urls_callback = Some(unsafe {
            std::mem::transmute::<Box<dyn FnMut(Vec<String>)>, Box<dyn FnMut(Vec<String>) + Send>>(
                callback,
            )
        });
    }

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn prompt_for_paths(
        &self,
        options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        let (tx, rx) = oneshot::channel();
        let suggested = options.prompt.as_ref().map(|s| s.as_ref());
        if !request_pick_paths(options.directories, false, suggested) {
            let _ = tx.send(Ok(None));
            return rx;
        }
        self.state.lock().pending_picker = Some(PendingPicker::Paths(tx));
        rx
    }

    fn prompt_for_new_path(
        &self,
        directory: &Path,
        suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        let suggested = suggested_name.map(ToOwned::to_owned).unwrap_or_else(|| {
            directory
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        if !request_pick_paths(false, true, Some(&suggested)) {
            let _ = tx.send(Ok(None));
            return rx;
        }
        self.state.lock().pending_picker = Some(PendingPicker::NewPath(tx));
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }

    fn reveal_path(&self, _path: &Path) {
        log::info!("OhosPlatform::reveal_path — not supported on HarmonyOS");
    }

    fn open_with_system(&self, _path: &Path) {
        log::info!("OhosPlatform::open_with_system — ArkTS launch not yet wired");
    }

    fn on_quit(&self, callback: Box<dyn FnMut() -> bool>) {
        self.state.lock().quit_callback = Some(unsafe {
            std::mem::transmute::<Box<dyn FnMut() -> bool>, Box<dyn FnMut() -> bool + Send>>(
                callback,
            )
        });
    }

    fn on_displays_changed(&self, _callback: Box<dyn FnMut()>) {
        // 一台 OHOS 设备上窗口挂在那块固定的屏上，运行期不会换显示器。
    }

    fn on_system_sleep(&self, _callback: Box<dyn FnMut()>) {
        // 移动端的后台切换由 ArkTS 生命周期走（onBackground），不是系统睡眠事件。
    }

    fn on_system_wake(&self, _callback: Box<dyn FnMut()>) {
        // 同上（onForeground）。
    }

    fn hide_cursor_until_mouse_moves(&self) {
        // 触屏这一侧没有指针可见性这件事。
    }

    fn is_cursor_visible(&self) -> bool {
        false
    }

    fn prevent_idle_sleep(&self, reason: &str) -> Task<Result<ActivityGuard>> {
        // 没接 OHOS 的 `PowerManager` 后台运行申请。返回 Err 而不是假装成功：
        // 调用方据此知道屏幕该自己管。
        Task::ready(Err(anyhow::anyhow!(
            "Idle sleep prevention for {reason:?} is not supported on HarmonyOS"
        )))
    }

    fn on_reopen(&self, callback: Box<dyn FnMut()>) {
        self.state.lock().reopen_callback = Some(unsafe {
            std::mem::transmute::<Box<dyn FnMut()>, Box<dyn FnMut() + Send>>(callback)
        });
    }

    fn on_app_lifecycle(&self, callback: Box<dyn FnMut(AppLifecyclePhase)>) {
        self.state.lock().lifecycle_callback = Some(unsafe {
            std::mem::transmute::<
                Box<dyn FnMut(AppLifecyclePhase)>,
                Box<dyn FnMut(AppLifecyclePhase) + Send>,
            >(callback)
        });
    }

    fn on_memory_warning(&self, callback: Box<dyn FnMut()>) {
        self.state.lock().memory_warning_callback = Some(unsafe {
            std::mem::transmute::<Box<dyn FnMut()>, Box<dyn FnMut() + Send>>(callback)
        });
    }

    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {}

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}

    fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn Action)>) {}

    fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {}

    fn on_validate_app_menu_command(&self, _callback: Box<dyn FnMut(&dyn Action) -> bool>) {}

    fn thermal_state(&self) -> ThermalState {
        ThermalState::Nominal
    }

    fn on_thermal_state_change(&self, _callback: Box<dyn FnMut()>) {}

    fn app_path(&self) -> Result<PathBuf> {
        let exe = std::fs::read_link("/proc/self/exe").unwrap_or_else(|_| PathBuf::from(""));
        Ok(exe)
    }

    fn path_for_auxiliary_executable(&self, _name: &str) -> Result<PathBuf> {
        anyhow::bail!("auxiliary executables are not supported on HarmonyOS")
    }

    fn set_cursor_style(&self, _style: CursorStyle) {}

    fn should_auto_hide_scrollbars(&self) -> bool {
        true
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        self.state
            .lock()
            .clipboard
            .read()
            .map(ClipboardItem::new_string)
    }

    fn write_to_clipboard(&self, item: ClipboardItem) {
        self.write_to_clipboard_impl(item.text().unwrap_or_default());
    }

    // The OHOS triple reports `target_os = "linux"`, so the trait additionally
    // exposes the X11/Wayland "primary selection" clipboard methods.  OHOS has
    // no such concept; alias them to the regular clipboard.
    fn read_from_primary(&self) -> Option<ClipboardItem> {
        self.read_from_clipboard()
    }

    fn write_to_primary(&self, item: ClipboardItem) {
        self.write_to_clipboard(item);
    }

    fn write_credentials(&self, url: &str, username: &str, password: &[u8]) -> Task<Result<()>> {
        Task::ready(self.write_credentials_impl(url, username, password))
    }

    fn read_credentials(&self, url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        let result = self
            .read_credentials_impl(url, "default")
            .map(|opt| opt.map(|pw| ("default".to_string(), pw)));
        Task::ready(result)
    }

    fn delete_credentials(&self, url: &str) -> Task<Result<()>> {
        Task::ready(self.delete_credentials_impl(url, "default"))
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(crate::ohos::keyboard::OhosKeyboardLayout::new("en-US"))
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        Rc::new(OhosKeyboardMapper)
    }

    fn on_keyboard_layout_change(&self, callback: Box<dyn FnMut()>) {
        self.state.lock().keyboard_layout_callback = Some(unsafe {
            std::mem::transmute::<Box<dyn FnMut()>, Box<dyn FnMut() + Send>>(callback)
        });
    }
}

// ── keyboard mapper ───────────────────────────────────────────────────────────

/// Minimal `PlatformKeyboardMapper` for HarmonyOS.
struct OhosKeyboardMapper;

impl PlatformKeyboardMapper for OhosKeyboardMapper {
    fn map_key_equivalent(
        &self,
        keystroke: Keystroke,
        _use_key_equivalents: bool,
    ) -> KeybindingKeystroke {
        KeybindingKeystroke::from_keystroke(keystroke)
    }

    fn get_key_equivalents(&self) -> Option<&HashMap<char, char, rustc_hash::FxBuildHasher>> {
        None
    }
}

// ── SharedPlatform — Rc-friendly wrapper around Arc<OhosPlatform> ────────────

/// A thin wrapper so that `Arc<OhosPlatform>` can be used as `Rc<dyn Platform>`
/// by GPUI's `Application::with_platform`.
pub struct SharedPlatform(pub Arc<OhosPlatform>);

impl SharedPlatform {
    pub fn new(inner: Arc<OhosPlatform>) -> Self {
        Self(inner)
    }

    /// Convenience: wrap in `Rc<dyn Platform>` for GPUI.
    pub fn into_rc(self) -> Rc<dyn Platform> {
        Rc::new(self)
    }
}

/// Route every `Platform` trait call through to `self.0`.
impl Platform for SharedPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        <OhosPlatform as Platform>::background_executor(&self.0)
    }
    fn foreground_executor(&self) -> ForegroundExecutor {
        <OhosPlatform as Platform>::foreground_executor(&self.0)
    }
    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        <OhosPlatform as Platform>::text_system(&self.0)
    }
    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        <OhosPlatform as Platform>::run(&self.0, on_finish_launching)
    }
    fn quit(&self) {
        <OhosPlatform as Platform>::quit(&self.0)
    }
    fn restart(&self, binary_path: Option<PathBuf>, arguments: Vec<std::ffi::OsString>) {
        <OhosPlatform as Platform>::restart(&self.0, binary_path, arguments)
    }
    fn activate(&self, ignoring_other_apps: bool) {
        <OhosPlatform as Platform>::activate(&self.0, ignoring_other_apps)
    }
    fn hide(&self) {
        <OhosPlatform as Platform>::hide(&self.0)
    }
    fn hide_other_apps(&self) {
        <OhosPlatform as Platform>::hide_other_apps(&self.0)
    }
    fn unhide_other_apps(&self) {
        <OhosPlatform as Platform>::unhide_other_apps(&self.0)
    }
    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        <OhosPlatform as Platform>::displays(&self.0)
    }
    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        <OhosPlatform as Platform>::primary_display(&self.0)
    }
    fn active_window(&self) -> Option<AnyWindowHandle> {
        <OhosPlatform as Platform>::active_window(&self.0)
    }
    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        <OhosPlatform as Platform>::open_window(&self.0, handle, options)
    }
    fn window_appearance(&self) -> WindowAppearance {
        <OhosPlatform as Platform>::window_appearance(&self.0)
    }
    fn open_url(&self, url: &str) {
        <OhosPlatform as Platform>::open_url(&self.0, url)
    }
    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        <OhosPlatform as Platform>::on_open_urls(&self.0, callback)
    }
    fn register_url_scheme(&self, url: &str) -> Task<Result<()>> {
        <OhosPlatform as Platform>::register_url_scheme(&self.0, url)
    }
    fn prompt_for_paths(
        &self,
        options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        <OhosPlatform as Platform>::prompt_for_paths(&self.0, options)
    }
    fn prompt_for_new_path(
        &self,
        directory: &Path,
        suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        <OhosPlatform as Platform>::prompt_for_new_path(&self.0, directory, suggested_name)
    }
    fn can_select_mixed_files_and_dirs(&self) -> bool {
        <OhosPlatform as Platform>::can_select_mixed_files_and_dirs(&self.0)
    }
    fn reveal_path(&self, path: &Path) {
        <OhosPlatform as Platform>::reveal_path(&self.0, path)
    }
    fn open_with_system(&self, path: &Path) {
        <OhosPlatform as Platform>::open_with_system(&self.0, path)
    }
    fn on_quit(&self, callback: Box<dyn FnMut() -> bool>) {
        <OhosPlatform as Platform>::on_quit(&self.0, callback)
    }
    fn on_displays_changed(&self, callback: Box<dyn FnMut()>) {
        <OhosPlatform as Platform>::on_displays_changed(&self.0, callback)
    }
    fn on_system_sleep(&self, callback: Box<dyn FnMut()>) {
        <OhosPlatform as Platform>::on_system_sleep(&self.0, callback)
    }
    fn on_system_wake(&self, callback: Box<dyn FnMut()>) {
        <OhosPlatform as Platform>::on_system_wake(&self.0, callback)
    }
    fn prevent_idle_sleep(&self, reason: &str) -> Task<Result<ActivityGuard>> {
        <OhosPlatform as Platform>::prevent_idle_sleep(&self.0, reason)
    }
    fn hide_cursor_until_mouse_moves(&self) {
        <OhosPlatform as Platform>::hide_cursor_until_mouse_moves(&self.0)
    }
    fn is_cursor_visible(&self) -> bool {
        <OhosPlatform as Platform>::is_cursor_visible(&self.0)
    }
    fn on_reopen(&self, callback: Box<dyn FnMut()>) {
        <OhosPlatform as Platform>::on_reopen(&self.0, callback)
    }
    fn on_app_lifecycle(&self, callback: Box<dyn FnMut(AppLifecyclePhase)>) {
        <OhosPlatform as Platform>::on_app_lifecycle(&self.0, callback)
    }
    fn on_memory_warning(&self, callback: Box<dyn FnMut()>) {
        <OhosPlatform as Platform>::on_memory_warning(&self.0, callback)
    }
    fn set_menus(&self, menus: Vec<Menu>, keymap: &Keymap) {
        <OhosPlatform as Platform>::set_menus(&self.0, menus, keymap)
    }
    fn set_dock_menu(&self, menu: Vec<MenuItem>, keymap: &Keymap) {
        <OhosPlatform as Platform>::set_dock_menu(&self.0, menu, keymap)
    }
    fn on_app_menu_action(&self, callback: Box<dyn FnMut(&dyn Action)>) {
        <OhosPlatform as Platform>::on_app_menu_action(&self.0, callback)
    }
    fn on_will_open_app_menu(&self, callback: Box<dyn FnMut()>) {
        <OhosPlatform as Platform>::on_will_open_app_menu(&self.0, callback)
    }
    fn on_validate_app_menu_command(&self, callback: Box<dyn FnMut(&dyn Action) -> bool>) {
        <OhosPlatform as Platform>::on_validate_app_menu_command(&self.0, callback)
    }
    fn thermal_state(&self) -> ThermalState {
        <OhosPlatform as Platform>::thermal_state(&self.0)
    }
    fn on_thermal_state_change(&self, callback: Box<dyn FnMut()>) {
        <OhosPlatform as Platform>::on_thermal_state_change(&self.0, callback)
    }
    fn app_path(&self) -> Result<PathBuf> {
        <OhosPlatform as Platform>::app_path(&self.0)
    }
    fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        <OhosPlatform as Platform>::path_for_auxiliary_executable(&self.0, name)
    }
    fn set_cursor_style(&self, style: CursorStyle) {
        <OhosPlatform as Platform>::set_cursor_style(&self.0, style)
    }
    fn should_auto_hide_scrollbars(&self) -> bool {
        <OhosPlatform as Platform>::should_auto_hide_scrollbars(&self.0)
    }
    fn write_to_clipboard(&self, item: ClipboardItem) {
        <OhosPlatform as Platform>::write_to_clipboard(&self.0, item)
    }
    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        <OhosPlatform as Platform>::read_from_clipboard(&self.0)
    }
    fn read_from_primary(&self) -> Option<ClipboardItem> {
        <OhosPlatform as Platform>::read_from_primary(&self.0)
    }
    fn write_to_primary(&self, item: ClipboardItem) {
        <OhosPlatform as Platform>::write_to_primary(&self.0, item)
    }
    fn write_credentials(&self, url: &str, username: &str, password: &[u8]) -> Task<Result<()>> {
        <OhosPlatform as Platform>::write_credentials(&self.0, url, username, password)
    }
    fn read_credentials(&self, url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        <OhosPlatform as Platform>::read_credentials(&self.0, url)
    }
    fn delete_credentials(&self, url: &str) -> Task<Result<()>> {
        <OhosPlatform as Platform>::delete_credentials(&self.0, url)
    }
    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        <OhosPlatform as Platform>::keyboard_layout(&self.0)
    }
    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        <OhosPlatform as Platform>::keyboard_mapper(&self.0)
    }
    fn on_keyboard_layout_change(&self, callback: Box<dyn FnMut()>) {
        <OhosPlatform as Platform>::on_keyboard_layout_change(&self.0, callback)
    }
}

impl Default for OhosPlatform {
    fn default() -> Self {
        Self::new(false)
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn headless() -> OhosPlatform {
        OhosPlatform::new(true)
    }

    #[test]
    fn platform_constructs_headless() {
        let p = headless();
        assert!(p.is_headless());
        assert!(!p.is_active());
        assert!(!p.should_quit());
    }

    #[test]
    fn quit_sets_should_quit() {
        let p = headless();
        assert!(!p.should_quit());
        p.quit();
        assert!(p.should_quit());
    }

    #[test]
    fn clipboard_round_trip() {
        let p = headless();
        assert!(p.read_from_clipboard_impl().is_none());
        p.write_to_clipboard_impl("hello ohos".to_string());
        assert_eq!(p.read_from_clipboard_impl().as_deref(), Some("hello ohos"));
    }

    #[test]
    fn credentials_round_trip() {
        let p = headless();
        p.write_credentials_impl("svc", "user", b"pass123").unwrap();
        assert_eq!(
            p.read_credentials_impl("svc", "user").unwrap().as_deref(),
            Some(b"pass123".as_slice())
        );
    }

    #[test]
    fn headless_has_one_display() {
        let p = headless();
        assert_eq!(p.displays().len(), 1);
        assert!(p.primary_display().is_some());
    }

    #[test]
    fn default_backend_is_gles() {
        assert_eq!(headless().preferred_backend(), OhosBackend::Gles);
    }

    #[test]
    fn initial_window_count_is_zero() {
        assert_eq!(headless().window_count(), 0);
        assert!(headless().primary_window().is_none());
    }

    #[test]
    fn run_invokes_callback_headless() {
        let p = headless();
        let ran = Arc::new(AtomicBool::new(false));
        let r2 = ran.clone();
        p.run(Box::new(move || r2.store(true, Ordering::Relaxed)));
        assert!(ran.load(Ordering::Relaxed));
    }
}
