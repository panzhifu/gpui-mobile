//! File / folder picking, through the ArkTS picker.
//!
//! OpenHarmony has **no native picker**: the NDK exposes no `OH_*_Picker` entry
//! point, and selection lives in `@ohos.file.picker` (`DocumentViewPicker`), which
//! only exists in ArkTS. So GPUI's `prompt_for_paths` cannot be answered here — it
//! has to be forwarded to a JS function the host registers, and the answer carried
//! back on a second call. That is the same shape as the software-keyboard bridge.
//!
//! ## What comes back is a URI, and we need a path
//!
//! The picker resolves to URIs (`file://docs/storage/Users/currentUser/Documents`).
//! folio's library is a real directory: it copies into it with `std::fs` and watches
//! it for new books. Inside the app's sandbox mount the user files are reachable at
//! the URI's own path, so the ArkTS side reduces the URI to that path before handing
//! it over. URIs that do *not* reduce — the media library's `file://media/...` — are
//! refused here rather than handed back as something `std::fs` will fail on later,
//! which is the same policy Android's document picker uses for providers it cannot
//! resolve.
//!
//! ## Wire format
//!
//! One string, paths separated by `U+0001`. A filesystem path can contain almost
//! anything, but not a record separator in practice; JSON would need a parser this
//! crate does not carry.
//!
//! This module is compiled only for `target_env = "ohos"`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use futures::channel::oneshot;
use gpui::{PathPromptOptions, Result};
use parking_lot::Mutex;

/// The separator the ArkTS side joins picked paths with.
pub const SEPARATOR: char = '\u{1}';

/// Sends the answer back to the GPUI task that asked.
type Pending = HashMap<u64, oneshot::Sender<Result<Option<Vec<PathBuf>>>>>;

/// `(request id, mode)` → start a picker on the ArkTS side. Installed by `napi.rs`.
///
/// `Arc` rather than `Box` so it can be cloned *out* of the lock before being
/// called: the ArkTS side may answer on this very thread, and a handler calling
/// back into [`set_prompt_handler`] under the lock would deadlock.
type Prompt = Arc<dyn Fn(u64, &str) + Send + Sync>;

static PENDING: OnceLock<Mutex<Pending>> = OnceLock::new();
static PROMPT: Mutex<Option<Prompt>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn pending() -> &'static Mutex<Pending> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Install the JS-side entry point. Called once, from `nativeSetFolderPicker`.
pub fn set_prompt_handler<F>(handler: F)
where
    F: Fn(u64, &str) + Send + Sync + 'static,
{
    *PROMPT.lock() = Some(Arc::new(handler));
}

/// Whether any ArkTS picker is wired up. folio's ArkTS host registers one at page
/// load; until then there is nothing to ask.
pub fn is_wired() -> bool {
    PROMPT.lock().is_some()
}

/// The mode string the ArkTS side maps onto `DocumentSelectMode`.
fn mode_of(options: &PathPromptOptions) -> &'static str {
    match (options.files, options.directories) {
        (true, true) => "mixed",
        (false, true) => "folders",
        _ => "files",
    }
}

/// Ask ArkTS to open a picker. The returned receiver resolves exactly once: with
/// the picked paths, with `Ok(None)` when the reader cancelled, or with `Err` when
/// the request could not be made at all.
pub fn prompt(options: PathPromptOptions) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
    let (tx, rx) = oneshot::channel();
    let handler = PROMPT.lock().clone();
    let Some(handler) = handler else {
        // Answering `Ok(None)` here would be a lie with teeth: it is indistinguishable
        // from "the reader pressed cancel", and folio treats cancel as a no-op, so the
        // button looks dead. Say what is missing instead.
        let _ = tx.send(Err(anyhow::anyhow!(
            "no folder picker is registered — call nativeSetFolderPicker from ArkTS \
             (@ohos.file.picker); the NDK has no picker of its own"
        )));
        return rx;
    };

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mode = mode_of(&options);
    // Register before asking: a picker already open in front of the reader could
    // answer synchronously on this very thread.
    pending().lock().insert(id, tx);
    handler(id, mode);
    rx
}

/// Deliver the ArkTS side's answer. `joined` is the picked paths separated by
/// [`SEPARATOR`]; empty means the reader cancelled.
pub fn deliver(id: u64, joined: &str) {
    let sender = pending().lock().remove(&id);
    let Some(sender) = sender else {
        log::warn!("file_picker: no request pending for id {id}");
        return;
    };

    let paths: Vec<PathBuf> = joined
        .split(SEPARATOR)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect();
    log::info!("file_picker: id {id} resolved with {} path(s)", paths.len());
    let _ = sender.send(Ok(if paths.is_empty() { None } else { Some(paths) }));
}

/// The ArkTS side could not produce usable paths (a refused URI scheme, or the
/// picker itself failed).
pub fn fail(id: u64, reason: &str) {
    let sender = pending().lock().remove(&id);
    if let Some(sender) = sender {
        let _ = sender.send(Err(anyhow::anyhow!("folder picker: {reason}")));
    }
}

/// Drop every waiting sender. Called on teardown: a GPUI task awaiting a picker
/// that will never answer would hang forever, and `Receiver` reports the drop as
/// a cancelled request, which callers already handle.
pub fn abandon_all() {
    let dropped = pending().lock().drain().count();
    if dropped > 0 {
        log::info!("file_picker: abandoned {dropped} pending request(s)");
    }
}
