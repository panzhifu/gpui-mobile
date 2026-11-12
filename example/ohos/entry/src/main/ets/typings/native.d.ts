// Type declarations for the GPUI native module (`libgpui_mobile_example.so`).
//
// The module is registered from Rust via `gpui_ohos_napi_module!`; the exports
// below mirror the `napi_create_function` registrations in
// `gpui-mobile/src/ohos/napi.rs`.
declare module 'libgpui_mobile_example.so' {
  export interface GpuiNative {
    /** Force the module to load / initialise (no-op on the native side). */
    nativeInit(): void;
    /** Display density relative to 160 dpi (densityDPI / 160). */
    nativeSetDensity(scale: number): void;
    /** Window safe-area insets in vp: top, bottom, left, right. */
    nativeSetSafeAreaInsets(top: number, bottom: number, left: number, right: number): void;
    /** App moved to the foreground. */
    nativeOnForeground(): void;
    /** App moved to the background. */
    nativeOnBackground(): void;
    /** App is being destroyed. */
    nativeOnDestroy(): void;
    /**
     * Register the software-keyboard bridge.  The callback receives a
     * keyboard-type string to show the IME, or `undefined` to hide it.
     */
    nativeSetKeyboardCallback(cb: (kind?: string) => void): void;
    /**
     * Register the file / folder picker. Native calls `cb(requestId, mode)` with
     * mode `folders` | `files` | `mixed`; the answer goes back through
     * `nativeDeliverPick` or `nativeFailPick` with the same `requestId`.
     */
    nativeSetFolderPicker(cb: (requestId: number, mode: string) => void): void;
    /** Answer a picker request: real paths joined with U+0001, or '' for cancel. */
    nativeDeliverPick(requestId: number, paths: string): void;
    /** Fail a picker request with a reason the app can show. */
    nativeFailPick(requestId: number, reason: string): void;
  }

  const native: GpuiNative;
  export default native;
}
