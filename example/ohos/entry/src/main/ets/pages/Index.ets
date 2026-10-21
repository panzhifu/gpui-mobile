import native from 'libgpui_mobile_example.so';
import { hilog } from '@kit.PerformanceAnalysisKit';

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
    // NOTE: driving the OHOS IME requires a focused TextInput / the
    // `@ohos.inputMethod` API; that wiring is best-effort and currently just
    // logs.  Hardware / XComponent key events already reach GPUI.
    try {
      native.nativeSetKeyboardCallback((kind?: string) => {
        hilog.info(DOMAIN, TAG, 'keyboard request: %{public}s', kind ?? 'hide');
      });
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeSetKeyboardCallback failed');
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
