//! Soft-keyboard (IME) text input through the OpenHarmony NDK input method
//! controller (`libohinputmethod.so`).
//!
//! OpenHarmony exposes the input method client as a C API: attach an opaque
//! `InputMethod_TextEditorProxy` (callbacks installed through setters) to the
//! focused window with `OH_InputMethodController_Attach`, and the IME delivers
//! insert / preview / delete / selection / enter commands through those callbacks.
//! This is the same hand-rolled NDK style as [`super::xcomponent`] and
//! [`super::vsync`] — no ArkTS round-trip and no extra crate.
//!
//! ## Threading
//!
//! The IME may invoke callbacks on a binder thread rather than the thread that
//! attached. GPUI's `App` (and therefore [`PlatformInputHandler`], which stamps
//! every update with the spawning thread) lives on the UI thread, so the event
//! callbacks never touch the handler directly: they copy their arguments into an
//! owned value and hop through [`super::main_thread::post`], applying on the UI
//! thread. The synchronous query callbacks (`GetTextConfig`, cursor context) must
//! answer before returning, so they read a plain-data cache that the UI thread
//! keeps current — they never borrow the handler.
//!
//! ## Text model
//!
//! The NDK passes UTF-16 (`char16_t`) text and offsets, matching GPUI's
//! UTF-16 selection ranges. Deletion rounds endpoints out to character boundaries
//! so a surrogate pair is never split (the same rule as the Android arm).
//!
//! This module is compiled only for `target_env = "ohos"`.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

use gpui::{DispatchEventResult, PlatformInput, PlatformInputHandler};
use parking_lot::Mutex;

use super::main_thread;

// ── opaque NDK handles ─────────────────────────────────────────────────────────

#[repr(C)]
pub struct InputMethod_TextEditorProxy {
    _private: [u8; 0],
}
#[repr(C)]
pub struct InputMethod_AttachOptions {
    _private: [u8; 0],
}
#[repr(C)]
pub struct InputMethod_InputMethodProxy {
    _private: [u8; 0],
}
#[repr(C)]
pub struct InputMethod_TextConfig {
    _private: [u8; 0],
}
#[repr(C)]
pub struct InputMethod_CursorInfo {
    _private: [u8; 0],
}

/// `InputMethod_ErrorCode::IME_ERR_OK`.
const IME_ERR_OK: i32 = 0;
/// `InputMethod_KeyboardStatus`: hide / show.
const KEYBOARD_HIDE: i32 = 1;
/// `InputMethod_TextInputType::IME_TEXT_INPUT_TYPE_TEXT`.
const TEXT_INPUT_TYPE_TEXT: i32 = 0;
/// `InputMethod_EnterKeyType`: UNSPECIFIED / NONE insert a newline when unhandled;
/// SEARCH / SEND / GO / NEXT / DONE only dispatch the key.
const ENTER_UNSPECIFIED: i32 = 0;
const ENTER_NONE: i32 = 1;

// ── callback signatures (inputmethod_text_editor_proxy_capi.h) ─────────────────

type InsertTextFn = extern "C" fn(*mut InputMethod_TextEditorProxy, *const u16, usize);
type DeleteFn = extern "C" fn(*mut InputMethod_TextEditorProxy, i32);
type KeyboardStatusFn = extern "C" fn(*mut InputMethod_TextEditorProxy, i32);
type EnterKeyFn = extern "C" fn(*mut InputMethod_TextEditorProxy, i32);
type MoveCursorFn = extern "C" fn(*mut InputMethod_TextEditorProxy, i32);
type SetSelectionFn = extern "C" fn(*mut InputMethod_TextEditorProxy, i32, i32);
type SetPreviewTextFn =
    extern "C" fn(*mut InputMethod_TextEditorProxy, *const u16, usize, i32, i32) -> i32;
type FinishPreviewFn = extern "C" fn(*mut InputMethod_TextEditorProxy);
type GetTextConfigFn = extern "C" fn(*mut InputMethod_TextEditorProxy, *mut InputMethod_TextConfig);
type GetTextOfCursorFn =
    extern "C" fn(*mut InputMethod_TextEditorProxy, i32, *mut u16, *mut usize);
type GetTextIndexFn = extern "C" fn(*mut InputMethod_TextEditorProxy) -> i32;

#[link(name = "ohinputmethod", kind = "dylib")]
extern "C" {
    fn OH_TextEditorProxy_Create() -> *mut InputMethod_TextEditorProxy;
    fn OH_TextEditorProxy_Destroy(proxy: *mut InputMethod_TextEditorProxy);
    fn OH_TextEditorProxy_SetInsertTextFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<InsertTextFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetDeleteForwardFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<DeleteFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetDeleteBackwardFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<DeleteFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetSendKeyboardStatusFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<KeyboardStatusFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetSendEnterKeyFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<EnterKeyFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetMoveCursorFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<MoveCursorFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetHandleSetSelectionFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<SetSelectionFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetSetPreviewTextFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<SetPreviewTextFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetFinishTextPreviewFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<FinishPreviewFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetGetTextConfigFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<GetTextConfigFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetGetLeftTextOfCursorFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<GetTextOfCursorFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetGetRightTextOfCursorFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<GetTextOfCursorFn>,
    ) -> i32;
    fn OH_TextEditorProxy_SetGetTextIndexAtCursorFunc(
        proxy: *mut InputMethod_TextEditorProxy,
        func: Option<GetTextIndexFn>,
    ) -> i32;

    fn OH_AttachOptions_Create(show_keyboard: bool) -> *mut InputMethod_AttachOptions;
    fn OH_AttachOptions_Destroy(options: *mut InputMethod_AttachOptions);

    fn OH_InputMethodController_Attach(
        proxy: *mut InputMethod_TextEditorProxy,
        options: *mut InputMethod_AttachOptions,
        input_method_proxy: *mut *mut InputMethod_InputMethodProxy,
    ) -> i32;
    fn OH_InputMethodController_Detach(proxy: *mut InputMethod_InputMethodProxy) -> i32;

    fn OH_InputMethodProxy_ShowKeyboard(proxy: *mut InputMethod_InputMethodProxy) -> i32;
    fn OH_InputMethodProxy_HideKeyboard(proxy: *mut InputMethod_InputMethodProxy) -> i32;
    fn OH_InputMethodProxy_NotifyCursorUpdate(
        proxy: *mut InputMethod_InputMethodProxy,
        cursor_info: *mut InputMethod_CursorInfo,
    ) -> i32;

    fn OH_CursorInfo_Create(left: f64, top: f64, width: f64, height: f64)
        -> *mut InputMethod_CursorInfo;
    fn OH_CursorInfo_Destroy(cursor_info: *mut InputMethod_CursorInfo);

    fn OH_TextConfig_SetInputType(config: *mut InputMethod_TextConfig, input_type: i32);
    fn OH_TextConfig_SetPreviewTextSupport(config: *mut InputMethod_TextConfig, supported: bool);
    fn OH_TextConfig_SetSelection(config: *mut InputMethod_TextConfig, start: i32, end: i32);
}

// ── active session state ───────────────────────────────────────────────────────

/// The focused window's handler slot. The `Rc` is only ever borrowed on the UI
/// thread (attach / detach / the posted apply closures), so the wrapper is `Send`
/// for the static despite `Rc` itself not being `Send` — the same confinement
/// argument as `OhosPlatformState`.
struct HandlerSlot(Rc<RefCell<Option<PlatformInputHandler>>>);
// SAFETY: see the type doc — the Rc never leaves the UI thread.
unsafe impl Send for HandlerSlot {}

static HANDLER: Mutex<Option<HandlerSlot>> = Mutex::new(None);

type InputCb = Arc<Mutex<Box<dyn FnMut(PlatformInput) -> DispatchEventResult + Send>>>;
/// The window's input dispatch callback (shared `Arc`); the inner `Mutex` is what
/// the momentum pump already locks.
struct InputSlot(InputCb);

static INPUT_CB: Mutex<Option<InputSlot>> = Mutex::new(None);

static METHOD_PROXY: AtomicPtr<InputMethod_InputMethodProxy> = AtomicPtr::new(std::ptr::null_mut());
static EDITOR_PROXY: AtomicPtr<InputMethod_TextEditorProxy> = AtomicPtr::new(std::ptr::null_mut());

/// Plain-data selection cache for the synchronous `GetTextConfig` / cursor-index
/// queries, which may run on a binder thread and so cannot borrow the handler.
static CACHED_SELECTION: Mutex<(i32, i32)> = Mutex::new((0, 0));

// ── public API (called on the UI thread by OhosPlatformWindow) ─────────────────

/// Attach the IME for the focused input. `slot` shares the window's handler;
/// `input_cb` dispatches confirm / arrow keys into GPUI's key bindings. The
/// keyboard is shown only when the handler actually accepts text.
pub fn attach(
    slot: Rc<RefCell<Option<PlatformInputHandler>>>,
    input_cb: InputCb,
) {
    detach();

    let accepts_text = slot
        .borrow_mut()
        .as_mut()
        .is_some_and(|h| h.query_accepts_text_input());

    *HANDLER.lock() = Some(HandlerSlot(slot));
    *INPUT_CB.lock() = Some(InputSlot(input_cb));
    *CACHED_SELECTION.lock() = (0, 0);

    unsafe {
        let proxy = OH_TextEditorProxy_Create();
        if proxy.is_null() {
            log::warn!("ime: OH_TextEditorProxy_Create returned null");
            return;
        }
        OH_TextEditorProxy_SetInsertTextFunc(proxy, Some(on_insert_text));
        OH_TextEditorProxy_SetDeleteBackwardFunc(proxy, Some(on_delete_backward));
        OH_TextEditorProxy_SetDeleteForwardFunc(proxy, Some(on_delete_forward));
        OH_TextEditorProxy_SetSendKeyboardStatusFunc(proxy, Some(on_keyboard_status));
        OH_TextEditorProxy_SetSendEnterKeyFunc(proxy, Some(on_enter_key));
        OH_TextEditorProxy_SetMoveCursorFunc(proxy, Some(on_move_cursor));
        OH_TextEditorProxy_SetHandleSetSelectionFunc(proxy, Some(on_set_selection));
        OH_TextEditorProxy_SetSetPreviewTextFunc(proxy, Some(on_preview_text));
        OH_TextEditorProxy_SetFinishTextPreviewFunc(proxy, Some(on_finish_preview));
        OH_TextEditorProxy_SetGetTextConfigFunc(proxy, Some(on_get_text_config));
        OH_TextEditorProxy_SetGetLeftTextOfCursorFunc(proxy, Some(on_get_left_text));
        OH_TextEditorProxy_SetGetRightTextOfCursorFunc(proxy, Some(on_get_right_text));
        OH_TextEditorProxy_SetGetTextIndexAtCursorFunc(proxy, Some(on_get_text_index));

        let options = OH_AttachOptions_Create(accepts_text);
        let mut method_proxy: *mut InputMethod_InputMethodProxy = std::ptr::null_mut();
        let err = OH_InputMethodController_Attach(proxy, options, &mut method_proxy);
        OH_AttachOptions_Destroy(options);
        if err != IME_ERR_OK || method_proxy.is_null() {
            log::warn!("ime: OH_InputMethodController_Attach failed: {err}");
            OH_TextEditorProxy_Destroy(proxy);
            return;
        }
        EDITOR_PROXY.store(proxy, Ordering::Release);
        METHOD_PROXY.store(method_proxy, Ordering::Release);
        log::info!("ime: attached (accepts_text={accepts_text})");
    }
}

/// Detach the IME and forget the handler slot. Called when the input loses focus.
pub fn detach() {
    let method_proxy = METHOD_PROXY.swap(std::ptr::null_mut(), Ordering::AcqRel);
    if !method_proxy.is_null() {
        unsafe { OH_InputMethodController_Detach(method_proxy) };
    }
    let editor_proxy = EDITOR_PROXY.swap(std::ptr::null_mut(), Ordering::AcqRel);
    if !editor_proxy.is_null() {
        unsafe { OH_TextEditorProxy_Destroy(editor_proxy) };
    }
    *HANDLER.lock() = None;
    *INPUT_CB.lock() = None;
}

/// Whether the IME is currently attached.
pub fn is_attached() -> bool {
    !METHOD_PROXY.load(Ordering::Acquire).is_null()
}

/// Show the soft keyboard (the input keeps focus but the IME was hidden).
pub fn show_keyboard() {
    let proxy = METHOD_PROXY.load(Ordering::Acquire);
    if !proxy.is_null() {
        unsafe { OH_InputMethodProxy_ShowKeyboard(proxy) };
    }
}

/// Hide the soft keyboard without detaching.
pub fn hide_keyboard() {
    let proxy = METHOD_PROXY.load(Ordering::Acquire);
    if !proxy.is_null() {
        unsafe { OH_InputMethodProxy_HideKeyboard(proxy) };
    }
}

/// Report the caret bounds (logical pixels) so the IME can position candidates
/// and avoid covering the edited line.
pub fn update_cursor(bounds: gpui::Bounds<gpui::Pixels>) {
    let proxy = METHOD_PROXY.load(Ordering::Acquire);
    if proxy.is_null() {
        return;
    }
    unsafe {
        let info = OH_CursorInfo_Create(
            bounds.origin.x.as_f32() as f64,
            bounds.origin.y.as_f32() as f64,
            bounds.size.width.as_f32() as f64,
            bounds.size.height.as_f32() as f64,
        );
        if !info.is_null() {
            OH_InputMethodProxy_NotifyCursorUpdate(proxy, info);
            OH_CursorInfo_Destroy(info);
        }
    }
}

// ── event callbacks (may run on a binder thread) ───────────────────────────────
//
// Each copies its arguments into owned data and hops to the UI thread before
// touching the handler. The text pointer is only valid for the call's duration,
// so the UTF-16 → String conversion happens here, before the post.

#[derive(Clone, Copy, Debug)]
enum Kind {
    Insert,
    Preview,
    FinishPreview,
    DeleteBackward,
    DeleteForward,
    Selection,
    Enter,
    MoveCursor,
    KeyboardStatus,
}

unsafe fn utf16_to_string(ptr: *const u16, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf16_lossy(slice)
}

fn post_event(kind: Kind, text: String, a: i32, b: i32) {
    if !main_thread::post(Box::new(move || apply_event(kind, text, a, b))) {
        log::warn!("ime: no UI-thread hop; dropping {kind:?} event");
    }
}

extern "C" fn on_insert_text(_proxy: *mut InputMethod_TextEditorProxy, text: *const u16, len: usize) {
    let text = unsafe { utf16_to_string(text, len) };
    post_event(Kind::Insert, text, 0, 0);
}

extern "C" fn on_delete_backward(_proxy: *mut InputMethod_TextEditorProxy, len: i32) {
    post_event(Kind::DeleteBackward, String::new(), len, 0);
}

extern "C" fn on_delete_forward(_proxy: *mut InputMethod_TextEditorProxy, len: i32) {
    post_event(Kind::DeleteForward, String::new(), len, 0);
}

extern "C" fn on_keyboard_status(_proxy: *mut InputMethod_TextEditorProxy, status: i32) {
    post_event(Kind::KeyboardStatus, String::new(), status, 0);
}

extern "C" fn on_enter_key(_proxy: *mut InputMethod_TextEditorProxy, key: i32) {
    post_event(Kind::Enter, String::new(), key, 0);
}

extern "C" fn on_move_cursor(_proxy: *mut InputMethod_TextEditorProxy, direction: i32) {
    post_event(Kind::MoveCursor, String::new(), direction, 0);
}

extern "C" fn on_set_selection(_proxy: *mut InputMethod_TextEditorProxy, start: i32, end: i32) {
    post_event(Kind::Selection, String::new(), start, end);
}

extern "C" fn on_preview_text(
    _proxy: *mut InputMethod_TextEditorProxy,
    text: *const u16,
    len: usize,
    start: i32,
    end: i32,
) -> i32 {
    let text = unsafe { utf16_to_string(text, len) };
    post_event(Kind::Preview, text, start, end);
    IME_ERR_OK
}

extern "C" fn on_finish_preview(_proxy: *mut InputMethod_TextEditorProxy) {
    post_event(Kind::FinishPreview, String::new(), 0, 0);
}

// Synchronous queries: answer from the plain-data cache, never borrow the handler
// (these may run on a binder thread).

extern "C" fn on_get_text_config(
    _proxy: *mut InputMethod_TextEditorProxy,
    config: *mut InputMethod_TextConfig,
) {
    if config.is_null() {
        return;
    }
    let (start, end) = *CACHED_SELECTION.lock();
    unsafe {
        OH_TextConfig_SetInputType(config, TEXT_INPUT_TYPE_TEXT);
        OH_TextConfig_SetPreviewTextSupport(config, true);
        OH_TextConfig_SetSelection(config, start, end);
    }
}

extern "C" fn on_get_left_text(
    _proxy: *mut InputMethod_TextEditorProxy,
    _number: i32,
    _text: *mut u16,
    length: *mut usize,
) {
    // Context-aware prediction is not wired yet; report no surrounding text.
    if !length.is_null() {
        unsafe { *length = 0 };
    }
}

extern "C" fn on_get_right_text(
    _proxy: *mut InputMethod_TextEditorProxy,
    _number: i32,
    _text: *mut u16,
    length: *mut usize,
) {
    if !length.is_null() {
        unsafe { *length = 0 };
    }
}

extern "C" fn on_get_text_index(_proxy: *mut InputMethod_TextEditorProxy) -> i32 {
    CACHED_SELECTION.lock().0
}

// ── applying events on the UI thread ───────────────────────────────────────────

/// Run `f` with the handler taken out of its slot, so no `RefCell` borrow spans a
/// GPUI update (which may replace the handler) — the same discipline as the
/// Android arm's `text_input::drain`.
fn with_handler(f: impl FnOnce(&mut PlatformInputHandler)) {
    let slot = HANDLER.lock().as_ref().map(|s| s.0.clone());
    let Some(slot) = slot else { return };
    let Some(mut handler) = slot.borrow_mut().take() else {
        return;
    };
    f(&mut handler);
    let mut current = slot.borrow_mut();
    if current.is_none() {
        *current = Some(handler);
    }
}

fn dispatch_key(key: &str) -> DispatchEventResult {
    let cb = INPUT_CB.lock().as_ref().map(|s| s.0.clone());
    let Some(cb) = cb else {
        return DispatchEventResult::default();
    };
    let mut cb = cb.lock();
    cb(PlatformInput::KeyDown(gpui::KeyDownEvent {
        keystroke: gpui::Keystroke {
            key: key.into(),
            key_char: None,
            modifiers: gpui::Modifiers::default(),
        },
        is_held: false,
        prefer_character_input: false,
    }))
}

fn apply_event(kind: Kind, text: String, a: i32, b: i32) {
    match kind {
        Kind::Insert => with_handler(|h| {
            h.replace_text_in_range(None, &text);
            h.unmark_text();
        }),
        Kind::Preview => {
            let range = (a >= 0 && b >= a).then_some(a as usize..b as usize);
            with_handler(|h| {
                h.replace_and_mark_text_in_range(range, &text, None);
            });
        }
        Kind::FinishPreview => with_handler(|h| h.unmark_text()),
        Kind::DeleteBackward => apply_delete(false, a.max(0) as usize),
        Kind::DeleteForward => apply_delete(true, a.max(0) as usize),
        Kind::Selection => {
            if a >= 0 && b >= 0 {
                let start = a.min(b);
                let end = a.max(b);
                *CACHED_SELECTION.lock() = (start, end);
                with_handler(|h| {
                    h.unmark_text();
                    h.set_selected_text_range(start as usize..end as usize);
                });
            }
        }
        Kind::Enter => {
            with_handler(|h| h.unmark_text());
            let result = dispatch_key("enter");
            // Search / Send / Next must not silently insert a newline; only an
            // unhandled plain enter does.
            if result.propagate && matches!(a, ENTER_UNSPECIFIED | ENTER_NONE) {
                with_handler(|h| {
                    h.replace_text_in_range(None, "\n");
                });
            }
        }
        Kind::MoveCursor => {
            let key = match a {
                1 => "up",
                2 => "down",
                3 => "left",
                4 => "right",
                _ => return,
            };
            dispatch_key(key);
        }
        Kind::KeyboardStatus => {
            if a == KEYBOARD_HIDE {
                with_handler(|h| h.unmark_text());
            }
        }
    }
}

fn apply_delete(forward: bool, len: usize) {
    if len == 0 {
        return;
    }
    let slot = HANDLER.lock().as_ref().map(|s| s.0.clone());
    let Some(slot) = slot else { return };
    let Some(mut handler) = slot.borrow_mut().take() else {
        return;
    };
    if let Some(selection) = handler.selected_text_range(false) {
        let mut adjusted = None;
        if let Some(text) = handler.text_for_range(0..usize::MAX, &mut adjusted) {
            let (before, after) = if forward { (0, len) } else { (len, 0) };
            let range = deletion_range(&text, selection.range, before, after);
            handler.replace_text_in_range(Some(range), "");
        }
    }
    let mut current = slot.borrow_mut();
    if current.is_none() {
        *current = Some(handler);
    }
}

/// Round UTF-16 deletion endpoints out to character boundaries so a surrogate
/// pair is never split. Lifted from the Android arm's `text_input::deletion_range`
/// (UTF-16 mode), which carries the surrogate-pair tests.
fn deletion_range(
    text: &str,
    selection: std::ops::Range<usize>,
    before: usize,
    after: usize,
) -> std::ops::Range<usize> {
    let mut boundaries = vec![0];
    for ch in text.chars() {
        boundaries.push(boundaries.last().unwrap() + ch.len_utf16());
    }
    let length = *boundaries.last().unwrap();
    let start = selection.start.saturating_sub(before);
    let end = selection.end.saturating_add(after).min(length);
    let start_index = boundaries
        .partition_point(|offset| *offset <= start)
        .saturating_sub(1);
    let end_index = boundaries.partition_point(|offset| *offset < end);
    boundaries[start_index]..boundaries[end_index]
}

#[cfg(test)]
mod tests {
    use super::deletion_range;

    #[test]
    fn deletion_preserves_surrogate_pairs() {
        assert_eq!(deletion_range("你😀好", 3..3, 1, 0), 1..3);
        assert_eq!(deletion_range("你😀好", 1..1, 0, 1), 1..3);
        assert_eq!(deletion_range("你😀好", 0..0, 99, 99), 0..4);
    }
}