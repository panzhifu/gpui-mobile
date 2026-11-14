//! OHOS device metadata through the `deviceinfo` NDK (`libdeviceinfo_ndk.z.so`).
//!
//! Every query is a synchronous C call (API 10+), so this works on any thread
//! and needs no ArkTS round-trip — the same hand-rolled NDK style as the rest
//! of `src/ohos`. The `OH_Get*` strings are the device's own report; the table
//! they cover (`2in1`/`pc` desktop, `phone`/`default` phone, `tablet`,
//! `wearable`/`liteWearable`) is the same classification
//! [ohos-rs/gpui-ohos](https://github.com/ohos-rs/gpui-ohos) sorts by in its
//! device module.
//!
//! Gaps versus the other arms, both honest limitations of the NDK surface:
//! - OHOS exposes no user-assigned device name through the NDK, so
//!   `device_name` carries the market name (the closest "friendly name").
//! - Physical-vs-emulator has no direct probe. This fork's toolchain builds
//!   `x86_64-unknown-linux-ohos` **only** for the emulator (`example/build.sh`
//!   picks the target from the device's ABI list), so the arch check is the
//!   decision, same as reading `const.product.cpu.abilist` on the device.

use super::DeviceInfo;

pub fn get_device_info() -> Result<DeviceInfo, String> {
    unsafe {
        Ok(DeviceInfo {
            model: from_c(OH_GetProductModel()),
            manufacturer: from_c(OH_GetManufacture()),
            // HarmonyOS 的整串版本名（如 "HarmonyOS 6.0.0"）；NDK 没有单独的
            // release-version 格。
            os_version: from_c(OH_GetOSFullName()),
            device_name: from_c(OH_GetMarketName()),
            is_physical_device: !cfg!(target_arch = "x86_64"),
        })
    }
}

/// `OH_Get*` 的静态 C 串 → Rust String。空指针与空串归一成空串 —— 与 android 臂
/// 「属性读不到就空着」同一条口径。
unsafe fn from_c(raw: *const std::os::raw::c_char) -> String {
    if raw.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(raw).to_string_lossy().into_owned()
}

#[link(name = "deviceinfo_ndk.z", kind = "dylib")]
extern "C" {
    fn OH_GetProductModel() -> *const std::os::raw::c_char;
    fn OH_GetManufacture() -> *const std::os::raw::c_char;
    fn OH_GetOSFullName() -> *const std::os::raw::c_char;
    fn OH_GetMarketName() -> *const std::os::raw::c_char;
}
