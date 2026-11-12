- HarmonyOS / OpenHarmony: implement the `packages/*` bridges (they are stubs on
  `target_env = "ohos"` — each needs a NAPI/ArkTS implementation). The core
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
  open: map `TextInputControlFamily::KeyboardType` (email / phone / number …) —
  the ArkTS-side bridge receives the type, the NDK path always sends
  `TEXT_INPUT_TYPE_TEXT`.
- HarmonyOS: wire the software keyboard through `@ohos.inputMethod` (currently
  best-effort — the request is logged, not delivered).
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
