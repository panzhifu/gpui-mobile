//! Android Storage Access Framework bridge returning readable local files
//! and picked directory folders.

use super::jni::{self as jni_helpers, JniExt};
use anyhow::{anyhow, Result};
use futures::channel::oneshot;
use gpui::PathPromptOptions;
use jni::objects::{JObject, JObjectArray, JValue};
use std::path::PathBuf;

pub(super) fn prompt(
    options: PathPromptOptions,
) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
    let (sender, receiver) = oneshot::channel();
    // Waiting for an Activity result on GPUI's native event thread deadlocks
    // Android's pause/resume handshakes. The Java helper waits only on this worker.
    std::thread::spawn(move || {
        let result = match (options.files, options.directories) {
            (true, true) => Err(anyhow!(
                "Android cannot mix file and directory selection in a single prompt"
            )),
            (false, true) => pick_folder().map_err(|error| anyhow!(error)),
            (true, false) => pick_files(options.multiple).map_err(|error| anyhow!(error)),
            (false, false) => Err(anyhow!("The file prompt must allow files or directories")),
        };
        let _ = sender.send(result);
    });
    receiver
}

fn pick_files(multiple: bool) -> std::result::Result<Option<Vec<PathBuf>>, String> {
    jni_helpers::with_env(|env| {
        let activity = jni_helpers::activity(env)?;
        let class = jni_helpers::find_app_class(env, "dev.gpui.mobile.GpuiPathPicker")?;
        let result = env
            .call_static_method(
                &class,
                jni::jni_str!("openFiles"),
                jni::jni_sig!("(Landroid/app/Activity;Z)[Ljava/lang/String;"),
                &[JValue::Object(&activity), JValue::Bool(multiple)],
            )
            .and_then(|value| value.l())
            .map_err(|error| {
                env.exception_clear();
                error.to_string()
            })?;
        if result.is_null() {
            return Ok(None);
        }
        let array = unsafe { JObjectArray::<JObject>::from_raw(env, result.as_raw()) };
        let mut paths = Vec::with_capacity(array.len(env).e()?);
        for index in 0..array.len(env).e()? {
            let path: JObject = array.get_element(env, index).e()?;
            paths.push(PathBuf::from(jni_helpers::get_string(env, &path)));
        }
        Ok((!paths.is_empty()).then_some(paths))
    })
}

/// Pick a single folder via `ACTION_OPEN_DOCUMENT_TREE`, the way SDL's folder
/// dialog does.
///
/// SAF hands a directory back as a `content://…/tree/…` document URI. The Java
/// helper resolves that to a filesystem path when the folder lives on a local
/// volume (internal storage or an SD card) and returns the raw URI otherwise, so
/// the caller always gets a usable string.
fn pick_folder() -> std::result::Result<Option<Vec<PathBuf>>, String> {
    jni_helpers::with_env(|env| {
        let activity = jni_helpers::activity(env)?;
        let class = jni_helpers::find_app_class(env, "dev.gpui.mobile.GpuiPathPicker")?;
        let result = env
            .call_static_method(
                &class,
                jni::jni_str!("openFolder"),
                jni::jni_sig!("(Landroid/app/Activity;)Ljava/lang/String;"),
                &[JValue::Object(&activity)],
            )
            .and_then(|value| value.l())
            .map_err(|error| {
                env.exception_clear();
                error.to_string()
            })?;
        if result.is_null() {
            return Ok(None);
        }
        let path = jni_helpers::get_string(env, &result);
        Ok(Some(vec![PathBuf::from(path)]))
    })
}
