import native from 'libgpui_mobile_example.so';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { inputMethod } from '@kit.IMEKit';
import { pasteboard } from '@kit.BasicServicesKit';
import { preferences } from '@kit.ArkData';
import { common } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { picker, fileUri } from '@kit.CoreFileKit';
import { getMainWindow } from '../common/window_holder';

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
  @State private imeType: InputType = InputType.Normal;
  @State private imeVisible: boolean = false;
  @State private imeValue: string = '';
  private imeController: TextInputController = new TextInputController();
  private chromeGeneration: number = -1;
  private chromeReapply: number = -1;
  private chromeTimer: number = -1;
  private keyboardTimer: number = -1;
  private deeplinkGeneration: number = -1;
  private deeplinkDelivered: number = 0;
  private deeplinkTimer: number = -1;
  private credentialStore?: preferences.Preferences;

  aboutToAppear(): void {
    // Register the software-keyboard bridge.  Native calls this with a
    // keyboard-type string to show the IME, or with no argument to hide it.
    try {
      native.nativeSetKeyboardCallback((kind?: string) => {
        hilog.info(DOMAIN, TAG, 'keyboard request: %{public}s', kind ?? 'hide');
        if (kind === undefined) {
          this.imeVisible = false;
          this.imeValue = '';
          try { inputMethod.getController().stopInput(); } catch (e) { /* best effort */ }
          return;
        }
        this.imeType = this.inputTypeFor(kind);
        this.imeValue = '';
        this.imeVisible = true;
        // Focus on the next frame so the TextInput is laid out before we ask
        // it to become the IME owner.
        setTimeout(() => {
          try {
            this.imeController.focus();
            inputMethod.getController().showTextInput();
          } catch (e) {
            hilog.warn(DOMAIN, TAG, 'showTextInput failed: %{public}s', JSON.stringify(e));
          }
        }, 0);
      });
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeSetKeyboardCallback failed');
    }

    // Mirror GPUI clipboard writes into the system pasteboard.
    try {
      native.nativeSetClipboardCallback((text: string) => {
        try {
          const data = pasteboard.createData(pasteboard.MIMETYPE_TEXT_PLAIN);
          data.setData(pasteboard.MIMETYPE_TEXT_PLAIN, text);
          pasteboard.getSystemPasteboard().setDataSync(data);
        } catch (e) {
          hilog.warn(DOMAIN, TAG, 'pasteboard write failed: %{public}s', JSON.stringify(e));
        }
      });
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeSetClipboardCallback failed');
    }

    // Register native callbacks that need ArkUI system services.
    this.registerUrlLauncher();
    this.registerPathPicker();
    this.registerCredentialStore();

    // System-bar style is requested from GPUI's render loop. Poll the
    // generation and apply through the ArkUI window APIs when it changes.
    this.chromeTimer = setInterval(() => this.applySystemChromeIfNeeded(), 120);
    // The keyboard may be driven by other IME paths too; report its height
    // through the avoid-area API so GPUI can adjust text input placement.
    this.keyboardTimer = setInterval(() => this.publishKeyboardHeight(), 120);
    // Deeplinks may arrive before the native bridge exists; drain the queue
    // from AppStorage once the page is alive.
    this.deeplinkTimer = setInterval(() => this.flushDeeplinks(), 250);
  }

  aboutToDisappear(): void {
    if (this.chromeTimer >= 0) { clearInterval(this.chromeTimer); }
    if (this.keyboardTimer >= 0) { clearInterval(this.keyboardTimer); }
    if (this.deeplinkTimer >= 0) { clearInterval(this.deeplinkTimer); }
  }

  private inputTypeFor(kind: string): InputType {
    switch (kind) {
      case 'email': return InputType.Email;
      case 'phone': return InputType.PhoneNumber;
      case 'number': return InputType.Number;
      case 'url': return InputType.Url;
      case 'decimal': return InputType.Decimal;
      default: return InputType.Normal;
    }
  }

  private applySystemChromeIfNeeded(): void {
    try {
      const reapply = AppStorage.get<number>('gpui_chrome_reapply') ?? 0;
      const generation = native.nativeSystemChromeGeneration();
      if (generation === this.chromeGeneration && reapply === this.chromeReapply) { return; }
      this.chromeGeneration = generation;
      this.chromeReapply = reapply;
      const packed = native.nativeTakeSystemChrome();
      if (!Array.isArray(packed)) { return; }
      const win = getMainWindow();
      if (!win) { return; }
      const status: number | null = typeof packed[0] === 'number' ? packed[0] : null;
      const light: boolean = packed[1] === true;
      const nav: number | null = typeof packed[2] === 'number' ? packed[2] : null;
      if (status !== null) { win.setStatusBarColor(status); }
      if (nav !== null) { win.setNavigationBarColor(nav); }
      win.setWindowSystemBarProperties({
        statusBarColor: status !== null
          ? '#FF' + status.toString(16).padStart(6, '0').toUpperCase()
          : '#00000000',
        navigationBarColor: nav !== null
          ? '#FF' + nav.toString(16).padStart(6, '0').toUpperCase()
          : '#00000000',
        statusBarIconColor: light ? '#FFFFFFFF' : '#FF000000',
      });
      hilog.info(DOMAIN, TAG, 'system chrome applied: status=%{public}s light=%{public}s nav=%{public}s',
        String(status), light ? 'true' : 'false', String(nav));
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'applySystemChrome failed: %{public}s', JSON.stringify(e));
    }
  }

  private publishKeyboardHeight(): void {
    try {
      const win = getMainWindow();
      if (!win) { return; }
      const area = win.getWindowAvoidArea(window.AvoidAreaType.TYPE_KEYBOARD);
      native.nativeSetKeyboardHeight(px2vp(area.bottomRect.height));
    } catch (e) {
      // Some API versions do not expose TYPE_KEYBOARD; keep the last height.
    }
  }

  private registerUrlLauncher(): void {
    try {
      native.nativeSetOpenUrlCallback((url: string) => {
        try {
          const context = getContext(this) as common.UIAbilityContext;
          context.startAbility({
            action: 'ohos.want.action.viewData',
            entities: ['entity.system.browsable'],
            uri: url,
          });
          return true;
        } catch (e) {
          hilog.warn(DOMAIN, TAG, 'openUrl failed: %{public}s', JSON.stringify(e));
          return false;
        }
      });
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeSetOpenUrlCallback failed');
    }
  }

  private registerPathPicker(): void {
    try {
      native.nativeSetPickPathsCallback(
        async (directories: boolean, _save: boolean, suggestedName?: string) => {
          try {
            const documentPicker = new picker.DocumentViewPicker();
            const options = new picker.DocumentSelectOptions();
            options.maxSelectNumber = directories ? 1 : 100;
            if (suggestedName && !directories) {
              options.displayName = suggestedName;
            }
            const uris = await documentPicker.select(options);
            native.nativePickerResult(
              true,
              uris.map((uri: string) => new fileUri.FileUri(uri).path),
            );
          } catch (e) {
            hilog.warn(DOMAIN, TAG, 'path picker failed: %{public}s', JSON.stringify(e));
            native.nativePickerResult(false, []);
          }
        }
      );
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeSetPickPathsCallback failed');
    }
  }

  private registerCredentialStore(): void {
    try {
      native.nativeSetCredentialCallback(
        async (service: string, username: string, operation: number, password?: string) => {
          try {
            await this.initializeCredentialStore();
            const key = this.credentialKey(service, username);
            if (operation === 0) {
              return await this.credentialStore!.get(key, '') as string || null;
            }
            if (operation === 1) {
              await this.credentialStore!.put(key, password ?? '');
              await this.credentialStore!.flush();
              return undefined;
            }
            if (operation === 2) {
              await this.credentialStore!.delete(key);
              await this.credentialStore!.flush();
              return undefined;
            }
            throw new Error(`unsupported credential operation: ${operation}`);
          } catch (e) {
            hilog.warn(DOMAIN, TAG, 'credential operation failed: %{public}s', JSON.stringify(e));
            throw e;
          }
        }
      );
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeSetCredentialCallback failed');
    }
  }

  private flushDeeplinks(): void {
    try {
      const seq = AppStorage.get<number>('gpui_deeplink_seq') ?? 0;
      const urls = AppStorage.get<string[]>('gpui_deeplinks') ?? [];
      if (urls.length === 0 || seq === this.deeplinkGeneration) { return; }
      const pending = urls.slice(this.deeplinkDelivered);
      if (pending.length === 0) {
        this.deeplinkGeneration = seq;
        return;
      }
      native.nativeOpenUrls(pending);
      this.deeplinkDelivered = urls.length;
      this.deeplinkGeneration = seq;
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'flushDeeplinks failed: %{public}s', JSON.stringify(e));
    }
  }

  private async initializeCredentialStore(): Promise<void> {
    if (this.credentialStore) { return; }
    const context = getContext(this) as common.UIAbilityContext;
    this.credentialStore = await preferences.getPreferences(context, 'gpui_credentials');
  }

  private credentialKey(service: string, username: string): string {
    return `credential:${service}:${username}`;
  }

  build() {
    Stack() {
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

      // Hidden IME owner.  It is 1x1 px, transparent and positioned at the
      // top-left so it never covers the GPUI surface.  Text from the IME is
      // forwarded to the shared text-input callback in Rust.
      TextInput({ controller: this.imeController, text: this.imeValue })
        .type(this.imeType)
        .fontSize(1)
        .opacity(0)
        .width(1)
        .height(1)
        .position({ x: 0, y: 0 })
        .visibility(this.imeVisible ? Visibility.Visible : Visibility.Hidden)
        .onChange((value: string) => {
          // TextInput owns the editing buffer; send only newly appended text
          // plus the removals implied by a shorter value.  The value is a
          // controlled @State so the next IME update can reset it cleanly.
          const previous = this.imeValue;
          this.imeValue = value;
          if (value.length > previous.length) {
            native.nativeSubmitText(value.slice(previous.length));
          } else if (value.length < previous.length) {
            for (let i = value.length; i < previous.length; i++) {
              native.nativeKeyEvent('backspace', 0);
            }
          }
        })
        .onSubmit(() => {
          native.nativeKeyEvent('enter', 0);
          this.imeValue = '';
        })
    }
    .width('100%')
    .height('100%')
    .backgroundColor('#121318')
  }
}
