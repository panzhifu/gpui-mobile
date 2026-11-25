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
    /** Deliver committed IME text / editing sequences to GPUI. */
    nativeSubmitText(text: string): void;
    /** Deliver a navigation/editing key to GPUI. */
    nativeKeyEvent(key: string, modifiers?: number): void;
    /** System-bar style tuple, or null when no style has been requested. */
    nativeTakeSystemChrome(): (number | boolean | null)[] | null;
    /** Monotonic generation of the system-bar style. */
    nativeSystemChromeGeneration(): number;
    /** Register a callback that writes text to the system pasteboard. */
    nativeSetClipboardCallback(cb: (text: string) => void): void;
    /** Report current system pasteboard text to native. */
    nativeClipboardChanged(text: string): void;
    /** App lifecycle phase: 0=active, 1=inactive, 2=background, 3=foreground. */
    nativeLifecycle(phase: number): void;
    /** Report an OS memory-pressure event. */
    nativeMemoryWarning(): void;
    /** Appearance: 0=light, 1=dark, 2=vibrant light, 3=vibrant dark. */
    nativeSetAppearance(mode: number): void;
    /** Report the on-screen keyboard height in vp. */
    nativeSetKeyboardHeight(height: number): void;
    /** Register a URL launcher callback; return true when the URL was accepted. */
    nativeSetOpenUrlCallback(cb: (url: string) => boolean): void;
    /** Register a path picker callback.  It must call nativePickerResult. */
    nativeSetPickPathsCallback(cb: (directories: boolean, save: boolean, suggestedName?: string) => void): void;
    /** Complete a native path picker request. */
    nativePickerResult(ok: boolean, paths: string[]): void;
    /**
     * Register credential storage. Operation: 0=read, 1=write, 2=delete.
     * Read returns the stored password string or null; write and delete
     * return void. Throw to report a failure.
     */
    nativeSetCredentialCallback(
      cb: (service: string, username: string, operation: number, password?: string) => string | null | void,
    ): void;
    /** Report deeplink URLs to GPUI. */
    nativeOpenUrls(urls: string[]): void;
  }

  const native: GpuiNative;
  export default native;
}
