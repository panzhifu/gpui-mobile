//! Glue between feature-gated `packages/*` APIs and the OHOS NAPI bridges.

pub(crate) mod clipboard {
    pub fn set_text(text: &str) -> Result<(), String> {
        match super::super::napi::try_platform() {
            Some(platform) => {
                platform.write_to_clipboard_impl(text.to_owned());
                Ok(())
            }
            None => Err("OHOS platform is not initialized".into()),
        }
    }

    pub fn get_text() -> Result<Option<String>, String> {
        match super::super::napi::try_platform() {
            Some(platform) => Ok(platform.read_from_clipboard_impl()),
            None => Err("OHOS platform is not initialized".into()),
        }
    }

    pub fn has_text() -> Result<bool, String> {
        Ok(get_text()?.is_some())
    }
}
