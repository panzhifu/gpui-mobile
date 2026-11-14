import native from 'libgpui_mobile_example.so';
import inputMethod from '@ohos.inputMethod';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { registerFolderPicker } from '../common/folder_picker';

const DOMAIN: number = 0x0000;
const TAG: string = 'GpuiExample';

/// Root page.  The `XComponent` of type `SURFACE` with
/// `libraryname: 'gpui_mobile_example'` makes the runtime load our native
/// module (`libgpui_mobile_example.so`) and bind it to this component.  The
/// native `OnSurfaceCreated` callback then receives the `OHNativeWindow`
/// that GPUI renders into.
@Entry
@Component
struct Index {
  private xcomponentId: string = 'gpui_surface';

  aboutToAppear(): void {
    // Register the software-keyboard bridge.  Native calls this with a
    // keyboard-type string to show the IME, or with no argument to hide it.
    //
    // The request is delivered through `@ohos.inputMethod`'s controller
    // (`showTextInput` / `hideTextInput`).  Those operate on the currently
    // focused TextInput, so on a page whose only surface is the GPUI
    // XComponent they reject — the rejection is logged and the request is
    // otherwise dropped (the keyboard-type string currently informs nothing
    // on this path; the IME picks the layout from the focused component).
    // GPUI's own text fields drive the IME through the NDK controller
    // (`src/ohos/ime.rs`) and never come through here.  Hardware /
    // XComponent key events reach GPUI regardless.
    try {
      native.nativeSetKeyboardCallback((kind?: string) => {
        hilog.info(DOMAIN, TAG, 'keyboard request: %{public}s', kind ?? 'hide');
        const controller = inputMethod.getController();
        if (kind === undefined) {
          controller.hideTextInput().catch((err: Error) => {
            hilog.warn(DOMAIN, TAG, 'hideTextInput: %{public}s', JSON.stringify(err));
          });
        } else {
          controller.showTextInput().catch((err: Error) => {
            hilog.warn(DOMAIN, TAG, 'showTextInput: %{public}s', JSON.stringify(err));
          });
        }
      });
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeSetKeyboardCallback failed');
    }

    // File / folder picker.  The NDK has no picker, so GPUI's
    // `prompt_for_paths` arrives here as `(requestId, mode)` and the answer goes
    // back through `nativeDeliverPick` / `nativeFailPick`.  See
    // `common/folder_picker.ets` for the protocol and the persist step.
    try {
      registerFolderPicker(native);
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'registerFolderPicker failed: %{public}s', JSON.stringify(e));
    }
  }

  build() {
    Column() {
      XComponent({
        id: this.xcomponentId,
        type: XComponentType.SURFACE,
        libraryname: 'gpui_mobile_example'
      })
        .onLoad(() => {
          try {
            native.nativeInit();
          } catch (e) {
            hilog.warn(DOMAIN, TAG, 'nativeInit failed');
          }
        })
        .width('100%')
        .height('100%')
    }
    .width('100%')
    .height('100%')
    .backgroundColor('#121318')
  }
}
