# GPUI Mobile

Mobile platform layer for [GPUI](https://github.com/zed-industries/zed) — run Rust UI apps natively on **iOS**, **Android** and **HarmonyOS / OpenHarmony**.

[![Build & Test](https://github.com/itsbalamurali/gpui-mobile/actions/workflows/ci.yml/badge.svg)](https://github.com/itsbalamurali/gpui-mobile/actions/workflows/ci.yml)

## Overview

Implements the `gpui::Platform` trait for mobile targets, following the same architecture as Zed's desktop platform crates.

| Platform | Renderer | Text |
|----------|----------|------|
| **iOS** | Metal via wgpu | CoreText |
| **Android** | Vulkan/GL via wgpu | cosmic-text + swash |
| **HarmonyOS / OpenHarmony** | OpenGL ES via wgpu (EGL, `OHNativeWindow`) | cosmic-text + swash |

**Highlights:** GPU-accelerated rendering, touch input with momentum scrolling, keyboard support, safe area insets, dark mode, and emoji rendering.

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
| HarmonyOS / OpenHarmony (arm64) | ✅ (core platform) | API 12+ | OpenGL ES (EGL) |

### HarmonyOS / OpenHarmony

The `ohos` backend mirrors the Android one. Key points:

- The Rust target is `aarch64-unknown-linux-ohos` — note this reports
  `target_os = "linux"` **and** `target_env = "ohos"`, so every platform branch
  keys off `target_env`.
- Rendering goes through wgpu's **GLES/EGL** backend: the ArkUI `XComponent`
  (`type: SURFACE`) hands native code an `OHNativeWindow`, which is passed to
  wgpu as a `raw_window_handle::OhosNdkWindowHandle`. wgpu's Vulkan backend has
  no OHOS surface support, so GLES is used.
- Frames are driven by `OH_NativeVSync`; touch input arrives through the
  XComponent `DispatchTouchEvent` callback and is turned into GPUI mouse /
  scroll events (with momentum scrolling) exactly like Android.
- Entry point is a Node-API module registered from `example/src/lib.rs` via
  `gpui_mobile::gpui_ohos_napi_module!`. The ArkTS `EntryAbility` forwards
  density, safe-area insets and lifecycle events to native code.
- Fonts are loaded directly from `/system/fonts` (`HarmonyOS Sans`), so
  `gpui_wgpu` is built **without** its `font-kit` feature on OHOS.
- The `packages/*` modules are stubs on OHOS for now (they compile and return
  `Err`/`None`). See `PACKAGES-TO-IMPL.md`.

> The backend is verified to compile/type-check (`cargo +nightly check
> --target aarch64-unknown-linux-ohos -Z build-std=std,panic_abort`) but has
> not yet been validated on a physical HarmonyOS device in this repository.
> The software keyboard is best-effort (driven from ArkTS via
> `@ohos.inputMethod`); hardware key events already work.

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
