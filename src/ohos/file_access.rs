//! Persistent file access grants through the OpenHarmony NDK file-share API
//! (`libohfileshare.so`, `oh_file_share.h`).
//!
//! A picker hands back `file://` URIs whose access is *transient*: the app can
//! read them until the process dies. To keep access across restarts the app must
//! (1) `persistPermission` on the URI while it still holds the grant, and
//! (2) `activatePermission` again after each restart with the same URI — the OS
//! remembers the grant but not that this process should have it active. Both are
//! URI-based, so this module also keeps a small history file so a restart can
//! re-activate what it remembered without the caller tracking URIs by hand.
//!
//! ## Scope
//!
//! This is the framework capability only: it exposes [`FileAccess`] for a host to
//! call. Nothing here picks files or decides when grants are needed — the caller
//! does. Every entry point is a synchronous IPC into the file-share service, so
//! call them off the UI thread.
//!
//! Requires `ohos.permission.FILE_ACCESS_PERSIST` in the module manifest.
//!
//! This module is compiled only for `target_env = "ohos"`.

#![allow(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{c_char, c_void, CStr, CString};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use parking_lot::Mutex;

// ── limits ────────────────────────────────────────────────────────────────────

/// `FileShare_OperationMode::READ_MODE`.
pub const READ: u32 = 1;
/// `FileShare_OperationMode::WRITE_MODE`.
pub const WRITE: u32 = 2;

const MAX_GRANTS: usize = 256;
const MAX_URI_BYTES: usize = 4096;
const BATCH_SIZE: usize = 64;
const MAX_HISTORY_BYTES: u64 = 2 * 1024 * 1024;

// ── NDK ABI (filemanagement/fileshare/oh_file_share.h) ────────────────────────

#[repr(C)]
struct FileSharePolicyInfo {
    uri: *mut c_char,
    length: u32,
    operation_mode: u32,
}

#[repr(C)]
struct FileSharePolicyErrorResult {
    uri: *mut c_char,
    code: i32,
    message: *mut c_char,
}

/// `FileManagement_ErrCode::ERR_OK`.
const ERR_OK: i32 = 0;

#[link(name = "ohfileshare", kind = "dylib")]
extern "C" {
    fn OH_FileShare_PersistPermission(
        policies: *const FileSharePolicyInfo,
        policy_num: u32,
        result: *mut *mut FileSharePolicyErrorResult,
        result_num: *mut u32,
    ) -> i32;
    fn OH_FileShare_RevokePermission(
        policies: *const FileSharePolicyInfo,
        policy_num: u32,
        result: *mut *mut FileSharePolicyErrorResult,
        result_num: *mut u32,
    ) -> i32;
    fn OH_FileShare_ActivatePermission(
        policies: *const FileSharePolicyInfo,
        policy_num: u32,
        result: *mut *mut FileSharePolicyErrorResult,
        result_num: *mut u32,
    ) -> i32;
    fn OH_FileShare_CheckPersistentPermission(
        policies: *const FileSharePolicyInfo,
        policy_num: u32,
        result: *mut *mut bool,
        result_num: *mut u32,
    ) -> i32;
    fn OH_FileShare_ReleasePolicyErrorResult(
        error_result: *mut FileSharePolicyErrorResult,
        result_num: u32,
    );
}

// ── public types ──────────────────────────────────────────────────────────────

/// A grant the OS refused, with the reason it gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub uri: String,
    pub message: String,
}

/// Persistent file-access grants. The history file (when given) records the URIs
/// this app persisted, so [`activate_persisted`](FileAccess::activate_persisted)
/// can re-activate them after a restart without the caller remembering them.
pub struct FileAccess {
    history_path: Option<PathBuf>,
    /// Serialises history read-modify-write so two concurrent persists cannot
    /// clobber each other's grants.
    history: Mutex<()>,
}

impl FileAccess {
    /// `history_path` is the file that remembers persisted URIs (typically
    /// `<filesDir>/file-grants.txt`). `None` keeps grants in the OS only: persist
    /// and check still work, but nothing survives a restart to re-activate.
    pub fn new(history_path: Option<PathBuf>) -> Self {
        Self {
            history_path,
            history: Mutex::new(()),
        }
    }

    /// Persist and activate for this session, and remember the grants. `writable`
    /// asks for read-write; a provider that only allows reading is retried
    /// read-only so a save dialog is not silently lost.
    pub fn persist(&self, uris: &[String], writable: bool) -> Result<Vec<Failure>> {
        if uris.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = self.history.lock();
        let mut failures = Vec::new();
        let mut policies = self.load_history(&mut failures);

        let requested = if writable { READ | WRITE } else { READ };
        let mut requests: Vec<(String, u32)> = Vec::new();
        let mut new_uris: BTreeSet<String> = BTreeSet::new();
        for uri in uris {
            if !valid_uri(uri) {
                failures.push(Failure {
                    uri: uri.clone(),
                    message: "invalid or oversized file grant URI".into(),
                });
                continue;
            }
            let is_new = !policies.contains_key(uri) && !new_uris.contains(uri);
            if is_new && policies.len() + new_uris.len() >= MAX_GRANTS {
                failures.push(Failure {
                    uri: uri.clone(),
                    message: format!("file grant history is full ({MAX_GRANTS})"),
                });
                continue;
            }
            if is_new {
                new_uris.insert(uri.clone());
            }
            requests.push((uri.clone(), requested));
        }

        let mut saved = persist_batches(&requests, &mut failures);

        // Providers that grant only reading reject a read-write request; retry
        // those read-only and drop the read-write failure they recovered from.
        if requested != READ {
            let read_fallbacks: Vec<(String, u32)> = requests
                .iter()
                .filter(|(uri, _)| !saved.iter().any(|(saved_uri, _)| saved_uri == uri))
                .map(|(uri, _)| (uri.clone(), READ))
                .collect();
            if !read_fallbacks.is_empty() {
                let recovered = persist_batches(&read_fallbacks, &mut failures);
                if !recovered.is_empty() {
                    let recovered_uris: BTreeSet<String> =
                        recovered.iter().map(|(uri, _)| uri.clone()).collect();
                    failures.retain(|failure| !recovered_uris.contains(&failure.uri));
                    saved.extend(recovered);
                }
            }
        }

        for (uri, mode) in &saved {
            let merged = policies.get(uri).copied().unwrap_or(0) | *mode;
            policies.insert(uri.clone(), merged);
        }
        activate_batches(&saved, &mut failures);
        self.save_history(&policies, &mut failures);
        Ok(failures)
    }

    /// Check every remembered grant, prune the revoked, and re-activate the
    /// survivors. Call once on startup, off the UI thread. A check call that
    /// fails outright keeps its grants (a transient IPC failure must not wipe the
    /// history); a definitive "not granted" prunes them.
    pub fn activate_persisted(&self) -> Result<Vec<Failure>> {
        let _guard = self.history.lock();
        let mut failures = Vec::new();
        let policies = self.load_history(&mut failures);

        let mut alive: BTreeMap<String, u32> = BTreeMap::new();
        let mut to_activate: Vec<(String, u32)> = Vec::new();
        let mut to_check: Vec<(String, u32)> = policies
            .iter()
            .map(|(uri, mode)| (uri.clone(), *mode))
            .collect();
        // A revoked read-write grant may still be held read-only; the second pass
        // checks that fallback before dropping the URI.
        for _ in 0..2 {
            let mut fallback = Vec::new();
            for batch in to_check.chunks(BATCH_SIZE) {
                match check_batch(batch) {
                    Some(results) => {
                        for ((uri, mode), granted) in batch.iter().zip(results) {
                            if granted {
                                alive.insert(uri.clone(), *mode);
                                to_activate.push((uri.clone(), *mode));
                            } else if *mode == (READ | WRITE) {
                                fallback.push((uri.clone(), READ));
                            }
                        }
                    }
                    None => {
                        for (uri, mode) in batch {
                            failures.push(Failure {
                                uri: uri.clone(),
                                message: "check persistent permission failed; kept".into(),
                            });
                            alive.insert(uri.clone(), *mode);
                            to_activate.push((uri.clone(), *mode));
                        }
                    }
                }
            }
            to_check = fallback;
        }
        activate_batches(&to_activate, &mut failures);
        self.save_history(&alive, &mut failures);
        Ok(failures)
    }

    /// Which of `uris` are currently persisted. Returns one bool per URI, in order.
    pub fn check(&self, uris: &[String]) -> Result<Vec<bool>> {
        let mut out = Vec::with_capacity(uris.len());
        for chunk in uris.chunks(BATCH_SIZE) {
            let items: Vec<(String, u32)> = chunk.iter().map(|uri| (uri.clone(), READ)).collect();
            match check_batch(&items) {
                Some(results) => out.extend(results),
                None => out.resize(out.len() + chunk.len(), false),
            }
        }
        Ok(out)
    }

    /// Revoke the grants and forget them.
    pub fn revoke(&self, uris: &[String]) -> Result<Vec<Failure>> {
        let _guard = self.history.lock();
        let mut failures = Vec::new();
        let items: Vec<(String, u32)> = uris
            .iter()
            .map(|uri| (uri.clone(), READ | WRITE))
            .collect();
        for batch in items.chunks(BATCH_SIZE) {
            revoke_batch(batch, &mut failures);
        }
        let mut policies = self.load_history(&mut failures);
        for uri in uris {
            policies.remove(uri);
        }
        self.save_history(&policies, &mut failures);
        Ok(failures)
    }

    // ── history file ──────────────────────────────────────────────────────────

    fn load_history(&self, failures: &mut Vec<Failure>) -> BTreeMap<String, u32> {
        let Some(path) = self.history_path.as_deref() else {
            return BTreeMap::new();
        };
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
            Err(error) => {
                failures.push(Failure {
                    uri: path.display().to_string(),
                    message: format!("cannot read file grant history: {error}"),
                });
                return BTreeMap::new();
            }
        };
        let mut contents = String::new();
        let mut limited = file.take(MAX_HISTORY_BYTES + 1);
        if let Err(error) = limited.read_to_string(&mut contents) {
            failures.push(Failure {
                uri: path.display().to_string(),
                message: format!("cannot read file grant history: {error}"),
            });
            return BTreeMap::new();
        }
        if contents.len() as u64 > MAX_HISTORY_BYTES {
            failures.push(Failure {
                uri: path.display().to_string(),
                message: "file grant history is oversized".into(),
            });
            return BTreeMap::new();
        }
        parse_history(&contents)
    }

    fn save_history(&self, policies: &BTreeMap<String, u32>, failures: &mut Vec<Failure>) {
        let Some(path) = self.history_path.as_deref() else {
            return;
        };
        if let Err(error) = write_history(path, policies) {
            failures.push(Failure {
                uri: path.display().to_string(),
                message: format!("cannot save file grant history: {error:#}"),
            });
        }
    }
}

// ── NDK call wrappers (batched, error-collecting) ─────────────────────────────

/// Keep the C strings alive for as long as the policy array points into them.
fn build_policies(items: &[(String, u32)]) -> Option<(Vec<CString>, Vec<FileSharePolicyInfo>)> {
    let mut owned = Vec::with_capacity(items.len());
    let mut policies = Vec::with_capacity(items.len());
    for (uri, mode) in items {
        let c_uri = CString::new(uri.as_str()).ok()?;
        policies.push(FileSharePolicyInfo {
            uri: c_uri.as_ptr() as *mut c_char,
            length: uri.len() as u32,
            operation_mode: *mode,
        });
        owned.push(c_uri);
    }
    Some((owned, policies))
}

/// Drain a per-URI error array into `failures` and release it. Returns the URIs
/// that did *not* fail.
fn drain_errors(
    code: i32,
    result: *mut FileSharePolicyErrorResult,
    result_num: u32,
    batch: &[(String, u32)],
    failures: &mut Vec<Failure>,
) -> Vec<(String, u32)> {
    if result.is_null() || result_num == 0 {
        if code != ERR_OK {
            for (uri, _) in batch {
                failures.push(Failure {
                    uri: uri.clone(),
                    message: format!("file-share call failed with code {code}"),
                });
            }
            return Vec::new();
        }
        return batch.to_vec();
    }

    let mut failed: Vec<(String, String)> = Vec::with_capacity(result_num as usize);
    unsafe {
        let errors = std::slice::from_raw_parts(result, result_num as usize);
        for error in errors {
            let uri = if error.uri.is_null() {
                String::new()
            } else {
                CStr::from_ptr(error.uri).to_string_lossy().into_owned()
            };
            let message = if error.message.is_null() {
                format!("code {}", error.code)
            } else {
                format!(
                    "{} ({})",
                    CStr::from_ptr(error.message).to_string_lossy(),
                    error.code
                )
            };
            failed.push((uri, message));
        }
        OH_FileShare_ReleasePolicyErrorResult(result, result_num);
    }

    let mut retained = Vec::new();
    for item in batch {
        match failed
            .iter()
            .find(|(uri, _)| !uri.is_empty() && *uri == item.0)
        {
            Some((uri, message)) => failures.push(Failure {
                uri: uri.clone(),
                message: message.clone(),
            }),
            None => retained.push(item.clone()),
        }
    }
    retained
}

fn persist_batches(items: &[(String, u32)], failures: &mut Vec<Failure>) -> Vec<(String, u32)> {
    let mut retained = Vec::new();
    for batch in items.chunks(BATCH_SIZE) {
        let Some((_owned, policies)) = build_policies(batch) else {
            for (uri, _) in batch {
                failures.push(Failure {
                    uri: uri.clone(),
                    message: "URI contains an interior NUL".into(),
                });
            }
            continue;
        };
        let mut result: *mut FileSharePolicyErrorResult = std::ptr::null_mut();
        let mut result_num: u32 = 0;
        let code = unsafe {
            OH_FileShare_PersistPermission(
                policies.as_ptr(),
                policies.len() as u32,
                &mut result,
                &mut result_num,
            )
        };
        retained.extend(drain_errors(code, result, result_num, batch, failures));
    }
    retained
}

fn activate_batches(items: &[(String, u32)], failures: &mut Vec<Failure>) {
    for batch in items.chunks(BATCH_SIZE) {
        let Some((_owned, policies)) = build_policies(batch) else {
            continue;
        };
        let mut result: *mut FileSharePolicyErrorResult = std::ptr::null_mut();
        let mut result_num: u32 = 0;
        let code = unsafe {
            OH_FileShare_ActivatePermission(
                policies.as_ptr(),
                policies.len() as u32,
                &mut result,
                &mut result_num,
            )
        };
        drain_errors(code, result, result_num, batch, failures);
    }
}

fn revoke_batch(batch: &[(String, u32)], failures: &mut Vec<Failure>) {
    let Some((_owned, policies)) = build_policies(batch) else {
        return;
    };
    let mut result: *mut FileSharePolicyErrorResult = std::ptr::null_mut();
    let mut result_num: u32 = 0;
    let code = unsafe {
        OH_FileShare_RevokePermission(
            policies.as_ptr(),
            policies.len() as u32,
            &mut result,
            &mut result_num,
        )
    };
    drain_errors(code, result, result_num, batch, failures);
}

/// `None` means the check call itself failed; the caller decides whether that
/// means "keep" (activate_persisted) or "not granted" (check).
fn check_batch(batch: &[(String, u32)]) -> Option<Vec<bool>> {
    let (_owned, policies) = build_policies(batch)?;
    let mut result: *mut bool = std::ptr::null_mut();
    let mut result_num: u32 = 0;
    let code = unsafe {
        OH_FileShare_CheckPersistentPermission(
            policies.as_ptr(),
            policies.len() as u32,
            &mut result,
            &mut result_num,
        )
    };
    if code != ERR_OK {
        if !result.is_null() {
            unsafe { libc::free(result as *mut c_void) };
        }
        return None;
    }
    if result.is_null() || result_num as usize != batch.len() {
        if !result.is_null() {
            // The header says the bool array is released with free().
            unsafe { libc::free(result as *mut c_void) };
        }
        return None;
    }
    let granted = unsafe { std::slice::from_raw_parts(result, result_num as usize).to_vec() };
    unsafe { libc::free(result as *mut c_void) };
    Some(granted)
}

// ── pure helpers ──────────────────────────────────────────────────────────────

fn valid_uri(uri: &str) -> bool {
    uri.starts_with("file://") && uri.len() <= MAX_URI_BYTES && !uri.contains('\0')
}

fn parse_history(contents: &str) -> BTreeMap<String, u32> {
    let mut policies = BTreeMap::new();
    for line in contents.lines() {
        if policies.len() >= MAX_GRANTS {
            break;
        }
        let Some((mode, uri)) = line.split_once('\t') else {
            continue;
        };
        let Ok(mode) = mode.parse::<u32>() else {
            continue;
        };
        if !(READ..=READ | WRITE).contains(&mode) {
            continue;
        }
        if valid_uri(uri) {
            policies.insert(uri.to_string(), mode);
        }
    }
    policies
}

fn write_history(path: &Path, policies: &BTreeMap<String, u32>) -> Result<()> {
    anyhow::ensure!(
        policies.len() <= MAX_GRANTS,
        "file grant history exceeds {MAX_GRANTS}"
    );
    let mut contents = String::new();
    for (uri, mode) in policies {
        contents.push_str(&format!("{mode}\t{uri}\n"));
    }
    anyhow::ensure!(
        contents.len() as u64 <= MAX_HISTORY_BYTES,
        "file grant history exceeds {MAX_HISTORY_BYTES} bytes"
    );
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).context("create file grant history directory")?;
        }
    }
    let temporary = path.with_extension("txt.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .context("write file grant history")?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, path).context("replace file grant history")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_history, valid_uri, write_history, FileAccess, READ, WRITE};
    use std::collections::BTreeMap;

    #[test]
    fn uri_validation() {
        assert!(valid_uri("file://docs/storage/Users/currentUser/Documents"));
        assert!(!valid_uri("/storage/Users/currentUser/Documents"));
        assert!(!valid_uri("file://x\0y"));
        assert!(!valid_uri(&format!("file://{}", "a".repeat(5000))));
    }

    #[test]
    fn history_round_trip() {
        let path = std::env::temp_dir().join(format!(
            "gpui-mobile-grants-{}-round-trip.txt",
            std::process::id()
        ));
        let mut policies = BTreeMap::new();
        policies.insert("file://a/1".to_string(), READ);
        policies.insert("file://a/2".to_string(), READ | WRITE);
        write_history(&path, &policies).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed = parse_history(&contents);
        let _ = std::fs::remove_file(&path);
        assert_eq!(parsed, policies);
    }

    #[test]
    fn history_skips_malformed_lines() {
        let parsed =
            parse_history("1\tfile://ok\ngarbage\nx\tfile://bad-mode\n9\tfile://bad-mode\n3\t/storage/not-a-uri\n");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed.get("file://ok"), Some(&READ));
    }

    #[test]
    fn in_memory_history_is_empty() {
        let access = FileAccess::new(None);
        assert!(access.check(&[]).unwrap().is_empty());
        // persist with no history path still reports nothing for an empty input
        assert!(access.persist(&[], false).unwrap().is_empty());
    }
}