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
    /** System memory-pressure warning (`onMemoryLevel` → `MEMORY_LEVEL_*`). */
    nativeOnMemoryWarning(): void;
    /** App is being destroyed. */
    nativeOnDestroy(): void;
    /**
     * Register the pasteboard write bridge.  Native calls `cb(text)` when GPUI
     * writes the clipboard; the ArkTS side carries it to
     * `systemPasteboard.setData` (empty string = clear).
     */
    nativeSetClipboard(cb: (text: string) => void): void;
    /**
     * Push the current pasteboard text back to native.  Call this from a
     * `systemPasteboard.on('update', …)` watcher so GPUI's synchronous read
     * sees copies made in other apps; empty string = cleared or non-text.
     */
    nativeClipboardChanged(text: string): void;
    /** Report the system color mode (`dark` | `light`); see `nativeSetAppearance`. */
    nativeSetAppearance(mode: string): void;
    /**
     * Hand the ability's `resourceManager` to native. Backs the rawfile access
     * (`OH_ResourceManager_*`) that loads the bundled emoji font; call it once
     * in `onCreate` with `this.context.resourceManager`.
     */
    nativeSetResourceManager(resManager: object): void;
    /**
     * Register the URL opener. Native calls `cb(url)` from
     * `Platform::open_url`; carry it to `this.context.openLink(url)` — the NDK
     * has no WANT launcher.
     */
    nativeSetUrlHandler(cb: (url: string) => void): void;
    /**
     * Register the keep-screen-on sink. Native calls `cb(on)` when the number
     * of active `prevent_idle_sleep` reasons moves 0→1 (`true`) or 1→0
     * (`false`); toggle `window.setWindowKeepScreenOn` on the main window.
     */
    nativeSetKeepScreenOnHandler(cb: (on: boolean) => void): void;
    /**
     * Hand the ability's `filesDir` to native. Builds the persistent
     * file-grants store (`<filesDir>/file-grants.txt`) and re-activates the
     * grants from the previous session, so picked folders stay readable after
     * a restart.
     */
    nativeSetFilesDir(path: string): void;
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
