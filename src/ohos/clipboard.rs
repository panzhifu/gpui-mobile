//! System clipboard, through the ArkTS pasteboard.
//!
//! OpenHarmony's pasteboard lives in `@ohos.pasteboard` (ArkTS); the NDK exposes
//! no `OH_*Pasteboard*` entry point. GPUI's `Platform::read_from_clipboard` is
//! synchronous, but an ArkTS read is a Promise, so the two cannot be wired
//! directly. Instead the ArkTS host keeps a native cache fresh:
//!
//! * **write** — native updates the cache optimistically and forwards the text to
//!   a registered ArkTS function that calls `systemPasteboard.setData`.
//! * **push** — the ArkTS host watches `systemPasteboard.on('update', …)` and calls
//!   `nativeClipboardChanged(text)` whenever the pasteboard changes (including
//!   copies made in other apps), so the synchronous read returns current text.
//!
//! Without a registered bridge (headless / tests) the cache still works as an
//! in-process store, which is what the platform did before the bridge existed.
//!
//! This module is compiled only for `target_env = "ohos"`.

use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

/// Cached pasteboard text. `None` means empty or non-text content. ArkTS pushes
/// updates through [`deliver`]; [`read`] answers GPUI's synchronous trait method.
static CONTENTS: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// Forwards a write to ArkTS: `(text) => void`. Installed by `napi.rs` from
/// `nativeSetClipboard`. `Arc` so it can be cloned out of the lock before being
/// called — the handler may hop to the UI thread and must not hold this lock while
/// it does, or a re-entrant write would deadlock.
type Write = Arc<dyn Fn(&str) + Send + Sync>;

static WRITE: Mutex<Option<Write>> = Mutex::new(None);

fn contents() -> &'static Mutex<Option<String>> {
    CONTENTS.get_or_init(|| Mutex::new(None))
}

/// Install the ArkTS write entry point. Called once, from `nativeSetClipboard`.
pub fn set_write_handler<F>(handler: F)
where
    F: Fn(&str) + Send + Sync + 'static,
{
    *WRITE.lock() = Some(Arc::new(handler));
}

/// Whether an ArkTS pasteboard bridge is wired up.
pub fn is_wired() -> bool {
    WRITE.lock().is_some()
}

/// The cached pasteboard text, for GPUI's synchronous `read_from_clipboard`.
pub fn read() -> Option<String> {
    contents().lock().clone()
}

/// Write text to the system pasteboard. The cache is updated immediately so a
/// read right after a write sees the new text even before ArkTS round-trips; the
/// registered handler then carries it to `@ohos.pasteboard`. Empty text clears
/// the cache. With no bridge registered the text stays in-process only.
pub fn write(text: &str) {
    store(text);
    let handler = WRITE.lock().clone();
    match handler {
        Some(handler) => handler(text),
        None => log::debug!("clipboard: no ArkTS bridge; cached in-process only"),
    }
}

/// ArkTS pushes the current pasteboard text (empty ⇒ cleared or non-text).
/// Called from `nativeClipboardChanged`, typically by the `on('update')` watcher.
pub fn deliver(text: &str) {
    store(text);
}

fn store(text: &str) {
    *contents().lock() = if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    };
}