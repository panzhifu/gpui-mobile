//! Deep linking — handle incoming URLs and universal links.

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
pub(crate) mod ios;

use std::sync::Mutex;

/// Callback type for deep link events.
type DeepLinkCallback = Box<dyn Fn(&str) + Send + 'static>;

static CALLBACK: Mutex<Option<DeepLinkCallback>> = Mutex::new(None);

/// Register a callback to be invoked when a deep link is received.
pub fn set_deep_link_handler(handler: impl Fn(&str) + Send + 'static) {
    *CALLBACK.lock().unwrap() = Some(Box::new(handler));
}

/// Called internally when a deep link URL is received.
#[allow(dead_code)]
pub(crate) fn notify_deep_link(url: &str) {
    if let Some(cb) = CALLBACK.lock().unwrap().as_ref() {
        cb(url);
    }
}

#[cfg(target_env = "ohos")]
use std::sync::Mutex as OhosMutex;
#[cfg(target_env = "ohos")]
static OHOS_INITIAL_LINK: OhosMutex<Option<String>> = OhosMutex::new(None);
#[cfg(target_env = "ohos")]
static OHOS_LATEST_LINK: OhosMutex<Option<String>> = OhosMutex::new(None);

/// Called by the OHOS NAPI bridge when a `want` URI arrives.
#[cfg(target_env = "ohos")]
pub(crate) fn ohos_notify_deep_link(url: &str) {
    let mut latest = OHOS_LATEST_LINK.lock().unwrap();
    let is_first = latest.is_none();
    *latest = Some(url.to_owned());
    drop(latest);
    if is_first {
        let mut initial = OHOS_INITIAL_LINK.lock().unwrap();
        if initial.is_none() {
            *initial = Some(url.to_owned());
        }
    }
    notify_deep_link(url);
}

/// Get the initial deep link URL that launched the app (if any).
pub fn get_initial_link() -> Result<Option<String>, String> {
    #[cfg(target_os = "ios")]
    {
        ios::get_initial_link()
    }
    #[cfg(target_os = "android")]
    {
        android::get_initial_link()
    }
    #[cfg(target_env = "ohos")]
    {
        Ok(OHOS_INITIAL_LINK.lock().unwrap().clone())
    }
    #[cfg(not(any(target_os = "ios", target_os = "android", target_env = "ohos")))]
    {
        Ok(None)
    }
}

/// Get the latest deep link URL received while the app was running.
pub fn get_latest_link() -> Option<String> {
    #[cfg(target_os = "ios")]
    {
        ios::get_latest_link()
    }
    #[cfg(target_os = "android")]
    {
        android::get_latest_link()
    }
    #[cfg(target_env = "ohos")]
    {
        OHOS_LATEST_LINK.lock().unwrap().clone()
    }
    #[cfg(not(any(target_os = "ios", target_os = "android", target_env = "ohos")))]
    {
        None
    }
}
