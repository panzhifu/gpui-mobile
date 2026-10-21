# GPUI Mobile

Mobile platform layer for [GPUI](https://github.com/zed-industries/zed) — run Rust UI apps natively on **iOS**, **Android** and **HarmonyOS / OpenHarmony**.

[![Build & Test](https://github.com/longbridge/gpui-mobile/actions/workflows/ci.yml/badge.svg)](https://github.com/longbridge/gpui-mobile/actions/workflows/ci.yml)

## Overview

Implements the `gpui::Platform` trait for mobile targets, following the same architecture as Zed's desktop platform crates.

| Platform | Renderer | Text |
|----------|----------|------|
| **iOS** | Metal via wgpu | CoreText |
| **Android** | Vulkan/GL via wgpu | cosmic-text + swash |
| **HarmonyOS / OpenHarmony** | OpenGL ES via wgpu (EGL, `OHNativeWindow`) | cosmic-text + swash |

**Highlights:** GPU-accelerated rendering, touch input with momentum scrolling, keyboard support, safe area insets, dark mode, and emoji rendering.

## GPUI pre-release dependencies

This fork publishes as `gpui-pre-mobile` and uses the crates.io GPUI snapshots
from GPUI Kit. Version `0.1.0` targets `gpui-pre` and `gpui-pre-wgpu` **0.3.4**.
Both are pinned together so the platform, renderer, and application share the
same GPUI types. No Zed Git checkout or consumer Cargo patches are needed.

```toml
[dependencies]
gpui = { package = "gpui-pre", version = "=0.3.4", default-features = false }
gpui-mobile = { package = "gpui-pre-mobile", version = "0.1.0" }
```

The Rust library name remains `gpui_mobile`, including its native library
filenames. Existing `use gpui_mobile::...` imports continue to work. Native app
entry points and build steps are demonstrated in `example/`.

This is an experimental platform release. Switching to `gpui-pre` does not
complete mobile IME composition, accessibility, or the mobile lifecycle hooks.

Custom Android hosts should extend `dev.gpui.mobile.GpuiInputActivity` instead of
`android.app.NativeActivity` to receive multistage IME composition through a
native `InputConnection`. Include `GpuiInputActivity.java` and retain the usual
`android.app.lib_name` metadata. For GPUI file prompts, also package
`GpuiPathPicker.java` and register its non-exported Activity as shown in the
example manifest. File prompts import documents into app cache before returning
filesystem paths. Folder prompts (`PathPromptOptions { directories: true, .. }`)
use `ACTION_OPEN_DOCUMENT_TREE` and return the folder as a filesystem path when
it lives on a local volume (internal storage or an SD card), falling back to the
`content://` tree URI otherwise. Neither prompt requires broad storage
permissions.

`packages::sensors` needs `GpuiSensors.java` as well. Rust subscribes a sensor
there the first time an app reads it, and the newest sample of each subscribed
sensor is kept in native memory for `accelerometer()` / `gyroscope()` /
`magnetometer()` / `barometer()` to read. `subscribe()` instead delivers every
sample to a callback on Android's main thread (not GPUI's render thread, so it
cannot touch entity state), and `set_live_updates(true)` makes the polling
getters live by asking for a redraw per sample. Subscriptions are dropped while
the app is in the background and re-armed on resume. Readings stay in the
device's natural frame — they are not rotated to match the display orientation,
which is also what iOS reports. Without that class `available_sensors()` still
answers and every reading stays `None`.

`GpuiInputActivity` also wires two things an app usually wants:

- **Window insets.** It installs an `OnApplyWindowInsetsListener` and reports
  the *system-bar* insets (with the IME excluded) plus the keyboard visibility
  and height through `nativeWindowInsets`. `AndroidWindow::safe_area_insets_logical()`
  therefore stays put when the software keyboard opens; the keyboard is exposed
  separately via `AndroidWindow::ime_insets_logical()` / `keyboard_visible()`.
  Keep `android:windowSoftInputMode="adjustNothing"` in the manifest so the
  keyboard overlays the window instead of resizing it.
- **The back gesture.** From Android 13 the system routes back through
  `OnBackInvokedDispatcher` rather than delivering `AKEYCODE_BACK`, so an edge
  swipe does nothing unless a callback is registered. `GpuiInputActivity`
  registers one and forwards it to the native side, which dispatches the same
  synthetic `AKEYCODE_BACK` the input queue produces (mapped to `escape`). Add
  `android:enableOnBackInvokedCallback="true"` to `<application>`. **Apps are
  responsible for handling back** — bind `escape` to your navigation and quit at
  the root, or the app will not exit from a back gesture.


## Quick Start

### Prerequisites

- **Rust** 1.75+
- **iOS**: macOS + Xcode 15+, [XcodeGen](https://github.com/yonaskolb/XcodeGen) (`brew install xcodegen`)
- **Android**: Android SDK + NDK r25+, [cargo-ndk](https://github.com/nickelc/cargo-ndk) (`cargo install cargo-ndk`)
- **HarmonyOS**: DevEco Studio + OpenHarmony SDK (set `OHOS_NDK`), nightly Rust with `rust-src`

### Build & Run

```bash
cd example

# iOS
./build.sh ios --device        # or --simulator
./build.sh ios --device --release

# Android
./build.sh android --device    # or --emulator
./build.sh android --device --release

# HarmonyOS / OpenHarmony
export OHOS_NDK="$HOME/OpenHarmony/Sdk/12/native"
./build.sh ohos --device       # or --emulator
./build.sh ohos --device --release
```

### Manual Build

```bash
# iOS
rustup target add aarch64-apple-ios
cd example && cargo build --target aarch64-apple-ios
cd ios && xcodegen generate --spec project.yml
xcodebuild -project GpuiExample.xcodeproj -scheme GpuiExample build

# Android
rustup target add aarch64-linux-android
cd example
cargo ndk -t arm64-v8a -P 31 -o android/gradle/app/src/main/jniLibs build
cd android/gradle && ./gradlew assembleDebug

# HarmonyOS (nightly; std is built from source)
rustup toolchain install nightly --component rust-src
export OHOS_NDK="$HOME/OpenHarmony/Sdk/12/native"
cd example
cargo +nightly build --target aarch64-unknown-linux-ohos -Z build-std=std,panic_abort
cp target/aarch64-unknown-linux-ohos/debug/libgpui_mobile_example.so \
   ohos/entry/libs/arm64-v8a/
cd ohos && hvigorw --mode module -p product=default assembleHap
hdc install -r entry/build/default/outputs/default/entry-default-unsigned.hap
```

> **Type-checking without an SDK.** The OHOS target is Tier 3, so `std` must be
> built from source with nightly (`-Z build-std`). If you only want to verify
> that the code compiles (no linking, no device), the host `clang` can be used
> as the cross-assembler — this is wired up in `.cargo/config.toml`:
>
> ```bash
> cargo +nightly check --target aarch64-unknown-linux-ohos -Z build-std=std,panic_abort
> ```

## Platform Support

| Platform | Status | Min Version | GPU Backend |
|----------|--------|-------------|-------------|
| iOS (device + sim) | ✅ | iOS 13.0+ | Metal |
| Android (arm64) | ✅ | API 26+ | Vulkan (preferred), GL ES 3.0 |
| Android (armv7/x86_64) | ⚠️ Untested | API 26+ | Vulkan / GL ES |
| HarmonyOS / OpenHarmony (arm64) | ⚠️ 未上真机 | API 12+ | OpenGL ES (EGL) |
| HarmonyOS emulator (x86_64) | ⚠️ 起得来、画不出 | API 26 | OpenGL ES (EGL) — 见下文那台驱动的 SSBO |

### HarmonyOS / OpenHarmony

The `ohos` backend mirrors the Android one. Key points:

- The Rust target is `aarch64-unknown-linux-ohos` — note this reports
  `target_os = "linux"` **and** `target_env = "ohos"`, so every platform branch
  keys off `target_env`.
- Rendering goes through wgpu's **GLES/EGL** backend: the ArkUI `XComponent`
  (`type: SURFACE`) hands native code an `OHNativeWindow`, which is passed to
  wgpu as a `raw_window_handle::OhosNdkWindowHandle`. wgpu's Vulkan backend has
  no OHOS surface support, so GLES is used.
- Frames are driven by `OH_NativeVSync`; its callback arrives on a vsync thread the
  platform picks, so the trampoline never runs a frame — it posts one to
  `ohos::main_thread` (a `napi_threadsafe_function` ring) and re-arms. GPUI's `App`
  lives on the UI thread, and `gpui-pre-scheduler` stamps every local task with the
  `ThreadId` that spawned it, so running a frame on the vsync thread aborts on the
  first foreground task rather than merely running slowly. Touch input arrives
  through the XComponent `DispatchTouchEvent` callback and is turned into GPUI mouse
  / scroll events (with momentum scrolling) exactly like Android.
- Entry point is a Node-API module registered from `example/src/lib.rs` via
  `gpui_mobile::gpui_ohos_napi_module!`. The ArkTS `EntryAbility` forwards
  density, safe-area insets and lifecycle events to native code.
- Fonts are loaded directly from `/system/fonts` (`HarmonyOS Sans`), so
  `gpui_wgpu` is built **without** its `font-kit` feature on OHOS.
- The `packages/*` modules are stubs on OHOS for now (they compile and return
  `Err`/`None`). See `PACKAGES-TO-IMPL.md`.

> **What is actually verified.** Type-check passes for both OHOS triples
> (`cargo +nightly check --target {aarch64,x86_64}-unknown-linux-ohos
> -Z build-std=std,panic_abort`). Beyond that, everything below was measured on
> a **HarmonyOS emulator** (API 26, `x86_64`, whose GLES comes from Mesa on the
> host): the `.so` links, the HAP installs **without signing**, the module
> loads, its NAPI exports are callable, `OH_NativeXComponent_RegisterCallback`
> returns success, `OnSurfaceCreated` reaches native with the real window size,
> a wgpu device is created, and the frame loop runs — GPUI lays the screen out
> and renders a scene (`Router: render() screen=Home`, then 20 quads and 199
> monochrome sprites per frame). Nothing aborts; the app sits there drawing.
>
> What it cannot do is bind that scene. The driver reports
> `storage_buffers_per_stage = 0` and `max_storage_buffer_binding_size = 0`, and
> lacks `DownlevelFlags::VERTEX_STORAGE` — GLES 3.0 has no SSBOs, and gpui_wgpu
> keeps its instance data in a storage buffer. Per frame the renderer then says
> `PipelineLayout with 'poly_sprites_layout' label is invalid` and
> `scene too large: … instance data needs 3200 bytes, above the maximum of 0`.
> **So: no pixels on that emulator, for a driver reason, not a code reason.** A
> physical HarmonyOS phone (GLES 3.2) has not been tested here.
>
> Four things that looked like bugs during that investigation, all now fixed:
> `hilog`'s `LogType` is `LOG_APP = 0` in the current SDK (the old `1 = APP`
> numbering silently drops every native log line); the OHOS system libraries
> must be linked explicitly, which now lives in the `#[link]` attributes in
> `src/ohos/mod.rs` rather than in each downstream's build script; a frame must
> not run on the vsync thread (see above); and the app has to be started with
> `Application::run_embedded` with the returned `ApplicationHandle` kept alive —
> `Platform::run` returns immediately here, so dropping that handle releases the
> `App`, and the runtime calls the module init a *second* time when it binds the
> XComponent, so a second `run` also overwrites the launch callback the first one
> is waiting on. Both show up as `app was released` sixty times a second.
>
> The software keyboard is best-effort (driven from ArkTS via
> `@ohos.inputMethod`); hardware key events already work.

## Rendering views to images

The `offscreen` feature adds `gpui_mobile::offscreen::OffscreenWindow`, a GPUI
window drawn into a texture instead of onto the screen, for turning a view into
an image (a share card, a long screenshot):

```rust
// Open with the view to capture, then render from an async task
// (`render` drives the window's own frame callbacks, which borrow the `App`).
let window = OffscreenWindow::open(size(px(390.), px(844.)), 3.0, WindowAppearance::Light, cx, |window, cx| {
    cx.new(|cx| MyView::new(window, cx))
})?;
window.layout(size(px(390.), px(844.)))?; // lay out without reading pixels back, e.g. to measure content
let image = window.render(size(px(390.), content_height))?; // image::RgbaImage
window.close(cx);
```

Dropping an `OffscreenWindow` without calling `close` (for example when the task
holding it is cancelled) still closes the window, on the next turn of the main
thread's executor. The GPU device and pipelines are kept for later captures;
once the last offscreen window closes, the render target sized to the last
frame and the glyph and image tiles the offscreen windows uploaded are released.

It needs `gpui-pre-wgpu`'s headless renderer, which upstream only builds for
tests and benchmarks and which picks a Vulkan/GL device that iOS lacks. That
patched renderer lives in [`panzhifu/gpui-folio`](https://github.com/panzhifu/gpui-folio)
— the same tree that carries the page-curl fork of `gpui-pre`, so the two are
pinned together at one rev — with the renderer behind an `offscreen` feature, a
constructor that takes the wgpu instance, methods that release its render target
and clear its sprite atlas, and a `request_device` that asks for the limits the
adapter actually reports. This repository patches to it; an application enabling
`offscreen` must patch the same way (cargo only reads a git source's root
manifest, so pointing at a subdirectory of another repo is not an option):

```toml
[patch.crates-io]
gpui-pre = { git = "https://github.com/panzhifu/gpui-folio", rev = "002b89f54338e4a13f8a361885cf2d5c1f4323b2" }
gpui-pre-wgpu = { git = "https://github.com/panzhifu/gpui-folio", rev = "002b89f54338e4a13f8a361885cf2d5c1f4323b2" }
```

## Screenshots

Running on a Motorola Edge 50 Pro (Android, Vulkan):

| Home | Counter | Settings | About |
|------|---------|----------|-------|
| ![Home](screenshots/home.png) | ![Counter](screenshots/counter.png) | ![Settings](screenshots/settings.png) | ![About](screenshots/about.png) |

A demo video is available at [`screenshots/demo.mp4`](screenshots/demo.mp4).

## Example App

The example app includes screens for Home, Counter, About, Settings, Components (Apple Glass + Material Design), Animations, and Shaders.

## Contributing

1. Fork & create a feature branch
2. Ensure all targets compile:
   ```bash
   cargo check
   cargo check --target aarch64-apple-ios
   cargo check --target aarch64-linux-android
   cargo test
   cargo fmt --all
   ```
3. Open a Pull Request

## License

Licensed under any one of:

- [GPL-3.0-or-later](LICENSE-GPL)
- [AGPL-3.0-or-later](LICENSE-AGPL)
- [Apache-2.0](LICENSE-APACHE)

## Acknowledgements

[Zed Industries](https://github.com/zed-industries/zed) (GPUI framework) · [wgpu](https://wgpu.rs/) · [cosmic-text](https://github.com/pop-os/cosmic-text) · [swash](https://github.com/dfrg/swash) · [Google Noto Fonts](https://github.com/googlefonts/noto-emoji)
