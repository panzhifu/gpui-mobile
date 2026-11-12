//! HAP `resources/rawfile` access, through the OHOS NDK raw-file API.
//!
//! OHOS apps cannot read their own install directory with `std::fs` — bundled
//! assets live in the HAP and are reached through `NativeResourceManager`,
//! which ArkTS owns (`context.resourceManager`).  The hand-off is one napi
//! call: ArkTS passes the resource-manager object, `OH_ResourceManager_
//! InitNativeResourceManager` unwraps the native handle, and from then on
//! [`load`] reads rawfile bytes from any thread (the C handle is thread-safe
//! for `ReadRawFile64`, but we serialise `init` behind a lock so the manager
//! can only be installed once).
//!
//! The immediate consumer is the bundled emoji font (see `platform.rs`):
//! `fonts/NotoColorEmoji.ttf` ships in the HAP's rawfile directory and is
//! added to the text system as soon as the manager arrives.  The same [`load`]
//! doubles as the `AssetSource` backing for apps that register one.
//!
//! Declarations are hand-copied from `raw_file_manager.h` (the
//! `native_resource_manager.h` NDK surface), following the
//! [`super::xcomponent`] precedent — the OHOS sysroot ships no Rust bindings
//! and the type-check build runs without the SDK.

use std::sync::{
    atomic::{AtomicPtr, Ordering},
    Mutex, OnceLock,
};

// Opaque OHOS types. We only ever hold pointers to them.
#[repr(C)]
struct NativeResourceManager {
    _private: [u8; 0],
}
#[repr(C)]
struct RawFile64 {
    _private: [u8; 0],
}

#[link(name = "rawfile.z", kind = "dylib")]
extern "C" {
    fn OH_ResourceManager_InitNativeResourceManager(
        env: super::napi::NapiEnv,
        res_manager: super::napi::NapiValue,
    ) -> *mut NativeResourceManager;
    fn OH_ResourceManager_OpenRawFile64(
        mgr: *const NativeResourceManager,
        file_name: *const std::os::raw::c_char,
    ) -> *mut RawFile64;
    fn OH_ResourceManager_GetRawFileSize64(raw_file: *mut RawFile64) -> i64;
    fn OH_ResourceManager_ReadRawFile64(
        raw_file: *mut RawFile64,
        content: *mut u8,
        length: i64,
    ) -> i32;
    fn OH_ResourceManager_CloseRawFile64(raw_file: *mut RawFile64);
    /// Declared for surface completeness; the manager is deliberately **never**
    /// closed — it is installed once and lives for the process lifetime.
    #[allow(dead_code)]
    fn OH_ResourceManager_CloseNativeResourceManager(mgr: *const NativeResourceManager);
}

/// The installed manager. `AtomicPtr` because the hand-off happens on the NAPI
/// thread while readers run on background executors.
static MANAGER: AtomicPtr<NativeResourceManager> = AtomicPtr::new(std::ptr::null_mut());

/// One-shot guard: a second `nativeSetResourceManager` must not leak the first
/// manager (or hand out two handles to the same HAP).
static INIT_GUARD: OnceLock<()> = OnceLock::new();

static INIT_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Install the native resource manager from the ArkTS
/// `context.resourceManager` value. Returns `false` when the NDK call refuses
/// the hand-off (wrong object, wrong thread — it must run on the thread that
/// owns `env`).
pub fn init(env: super::napi::NapiEnv, res_manager: super::napi::NapiValue) -> bool {
    if INIT_GUARD.get().is_some() {
        log::debug!("rawfile: manager already installed");
        return true;
    }
    let _guard = INIT_ENV_LOCK.lock();
    if INIT_GUARD.get().is_some() {
        return true;
    }
    let manager = unsafe { OH_ResourceManager_InitNativeResourceManager(env, res_manager) };
    if manager.is_null() {
        log::warn!("rawfile: InitNativeResourceManager returned null");
        return false;
    }
    MANAGER.store(manager, Ordering::Release);
    let _ = INIT_GUARD.set(());
    log::info!("rawfile: resource manager installed");
    true
}

/// Whether [`load`] can work yet (the ArkTS hand-off has arrived).
pub fn ready() -> bool {
    !MANAGER.load(Ordering::Acquire).is_null()
}

/// Read one rawfile entry fully into memory (`fonts/NotoColorEmoji.ttf` style
/// path, relative to `resources/rawfile/`).
pub fn load(path: &str) -> Option<Vec<u8>> {
    let manager = MANAGER.load(Ordering::Acquire);
    if manager.is_null() {
        return None;
    }
    let c_path = match std::ffi::CString::new(path) {
        Ok(c_path) => c_path,
        Err(_) => {
            log::warn!("rawfile: path contains a NUL byte: {path:?}");
            return None;
        }
    };
    // SAFETY: `manager` was handed over by ArkTS and stays valid for the
    // process lifetime (we never close it); `c_path` outlives the call.
    let file = unsafe { OH_ResourceManager_OpenRawFile64(manager, c_path.as_ptr()) };
    if file.is_null() {
        log::debug!("rawfile: open failed: {path}");
        return None;
    }
    let size = unsafe { OH_ResourceManager_GetRawFileSize64(file) };
    if size <= 0 {
        unsafe { OH_ResourceManager_CloseRawFile64(file) };
        log::debug!("rawfile: empty file: {path}");
        return None;
    }
    let mut bytes = vec![0u8; size as usize];
    let read = unsafe { OH_ResourceManager_ReadRawFile64(file, bytes.as_mut_ptr(), size) };
    unsafe { OH_ResourceManager_CloseRawFile64(file) };
    if (read as i64) < size {
        log::warn!("rawfile: short read on {path}: {read} of {size}");
        return None;
    }
    Some(bytes)
}
