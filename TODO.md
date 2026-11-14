- HarmonyOS / OpenHarmony: implement the `packages/*` bridges (they are stubs on
  `target_env = "ohos"` — each needs a NAPI/ArkTS implementation). **`device_info`
  is done** (`src/packages/device_info/ohos.rs`, synchronous `deviceinfo` NDK —
  no ArkTS), and the clipboard bridges landed with the lifecycle batch. The core
  platform layer (rendering, touch input, lifecycle) is implemented and runs a live
  frame loop on the emulator.
- HarmonyOS: validate the backend on a **physical device**. The x86_64 emulator is
  validated end to end — links, installs, draws (instance data goes through the
  texture transport because that driver has no SSBOs) — but a phone is a different
  GLES stack, and nobody has run it there.
- HarmonyOS: ship a bundled emoji font the way Android does. **Done** —
  `fonts/NotoColorEmoji.ttf` ships in the HAP's rawfile directory and is loaded
  through the rawfile NDK surface (`src/ohos/rawfile.rs`) as soon as ArkTS hands
  over `context.resourceManager` (`nativeSetResourceManager`). The system's
  `HMOSColorEmojiCompat.ttf` still must not go into the font database: GPUI then
  resolves emoji by that family name and the face has no style it asks for, so the
  first text render panics.
- HarmonyOS: the NDK IME (`src/ohos/ime.rs`, `libohinputmethod`) is wired through
  `set_input_handler` / `take_input_handler` and shows the real keyboard; still
  open: map the keyboard / enter-key type. The ArkTS-side bridge receives the
  type string, but the NDK path always sends `IME_TEXT_INPUT_TYPE_TEXT` (0) and
  never sets an enter-key type. Two seams, in order:
  1. gpui-pre 0.3.12's `TextInputConfiguration` has no `KeyboardType` field (the
     keyboard-type concept landed in upstream zed later), and
     `PlatformInputHandler` only exposes `text_input_configuration(window, cx)`
     with explicit context — the platform window's `set_input_handler` has
     neither. The fix lives in **gpui-folio**: a `query_text_input_configuration()`
     wrapper routing through the stored `AsyncWindowContext`, exactly the
     `query_accepts_text_input` pattern. With that in place the OHOS side is
     mechanical: query the config at attach, cache it next to
     `CACHED_SELECTION`, and set
     `OH_TextConfig_SetInputType` / `OH_TextConfig_SetEnterKeyType` in
     `on_get_text_config` (`inputmethod_text_editor_proxy_capi.h`, API 10+).
     Maps: gpui `TextInputAction` → `InputMethod_EnterKeyType`
     (`Unspecified`→0, `Enter`→8/`Newline`, `Done`→6, `Go`→2, `Search`→3,
     `Send`→4, `Next`→5, `Previous`→7 — values from
     `inputmethod_types_capi.h`); keyboard types → `IME_TEXT_INPUT_TYPE_*`
     (`EMAIL_ADDRESS`=5, `URL`=6, `PHONE`=3, `NUMBER`=2, `NUMBER_DECIMAL`=12,
     …).
  2. the packages keyboard path (`show_keyboard_with_type`) should feed the
     same cache, so `show_keyboard_with_type(EmailAddress)` also picks the
     email layout.
- HarmonyOS: wire the software keyboard through `@ohos.inputMethod` — **delivered**
  in the example (`pages/Index.ets`): the keyboard callback now calls the
  controller's `showTextInput` / `hideTextInput`, logging the rejection. That
  API operates on the focused TextInput, so an XComponent-only page still sees
  the request fail — a hidden, focusable TextInput (the Flutter approach) is
  the remaining step, and the whole path needs device validation.
- Improve the UX of the APIs exposed by the library.
- Assitive technologies support (e.g., screen readers, voice control).
- Deeplinking?
- Provide inbuilt routing/navigation solution ?
- Maps?
- In-app purchases?
- Assets management?
- Google Fonts?

- Use the new Component View System from GPUI once its merged and rewrite the UI and packages using the new system. This will allow us to have a more modular and reusable codebase, and also make it easier to maintain and extend the library in the future. https://github.com/zed-industries/zed/pull/51030

Screens to implement:

https://raw.githubusercontent.com/zed-industries/zed/refs/heads/main/crates/gpui/examples/gif_viewer.rs
https://raw.githubusercontent.com/zed-industries/zed/refs/heads/main/crates/gpui/examples/image_gallery.rs
https://raw.githubusercontent.com/zed-industries/zed/refs/heads/main/crates/gpui/examples/image_loading.rs

Update the text inputs with all the functionality from here https://raw.githubusercontent.com/zed-industries/zed/refs/heads/main/crates/gpui/examples/input.rs

- [ ] **Texture-based composition mode (future)** — offscreen rendering for better perf
