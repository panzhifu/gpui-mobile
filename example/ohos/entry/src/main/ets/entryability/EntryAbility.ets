import { AbilityConstant, ConfigurationConstant, UIAbility, Want } from '@kit.AbilityKit';
import { pasteboard, BusinessError } from '@kit.BasicServicesKit';
import { window } from '@kit.ArkUI';
import { display } from '@kit.ArkUI';
import { hilog } from '@kit.PerformanceAnalysisKit';
import native from 'libgpui_mobile_example.so';

const DOMAIN: number = 0x0000;
const TAG: string = 'GpuiExample';

/** The main window, kept for the keep-screen-on sink (see `wireKeepScreenOn`). */
let mainWindow: window.Window | undefined = undefined;

export default class EntryAbility extends UIAbility {
  onCreate(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    hilog.info(DOMAIN, TAG, 'EntryAbility onCreate');
    // A deeplink (gpui://screen) launches the app; stash it for the native side.
    if (want.uri) {
      AppStorage.setOrCreate<string>('gpui_deeplink', want.uri);
    }
    this.wireClipboard();

    // Hand the resource manager to native: the rawfile NDK surface
    // (`OH_ResourceManager_*`) unwraps this object and afterwards reads
    // `resources/rawfile/` — the bundled emoji font lives there and loads
    // into the text system the moment this lands (before any surface).
    try {
      native.nativeSetResourceManager(this.context.resourceManager);
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeSetResourceManager failed: %{public}s', JSON.stringify(e));
    }

    // URL opener (GPUI's `open_url`) and keep-screen-on sink. Both call into
    // ArkTS because the NDK has no WANT launcher / window-property surface.
    try {
      native.nativeSetUrlHandler((url: string) => {
        this.context.openLink(url, { appLinkingOnly: false }).catch((err: BusinessError) => {
          hilog.warn(DOMAIN, TAG, 'openLink failed: %{public}s', JSON.stringify(err));
        });
      });
      native.nativeSetKeepScreenOnHandler((on: boolean) => {
        if (mainWindow !== undefined) {
          mainWindow.setWindowKeepScreenOn(on).catch((err: BusinessError) => {
            hilog.warn(DOMAIN, TAG, 'setWindowKeepScreenOn failed: %{public}s', JSON.stringify(err));
          });
        }
      });
      native.nativeSetFilesDir(this.context.filesDir);
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'platform bridges failed: %{public}s', JSON.stringify(e));
    }
  }

  /**
   * Clipboard bridge (both halves live here):
   *
   * - **write** — native calls `nativeSetClipboard`'s callback when GPUI writes
   *   the clipboard; we carry it to `systemPasteboard.setData`.
   * - **push** — `on('update')` fires for every pasteboard change (including
   *   copies made in other apps); we read the first record and push its plain
   *   text back through `nativeClipboardChanged`, which refreshes the native
   *   cache that GPUI's synchronous read answers from.  Our own writes land in
   *   the same watcher; the round-trip is idempotent.
   *
   * The native cache stays the source of truth so `read_from_clipboard` never
   * has to await a Promise.
   */
  private wireClipboard(): void {
    try {
      const system = pasteboard.getSystemPasteboard();

      native.nativeSetClipboard((text: string) => {
        const data = pasteboard.createData(pasteboard.MIMETYPE_TEXT_PLAIN, text);
        system.setData(data).catch((err: BusinessError) => {
          hilog.warn(DOMAIN, TAG, 'pasteboard setData failed: %{public}s', JSON.stringify(err));
        });
      });

      system.on('update', () => {
        system.getData().then((data) => {
          let text = '';
          // The first record carries the plain text when the copy was text;
          // image / URI copies report empty (GPUI's cache stores text only).
          const record = data.getRecord(0);
          if (record && record.plainText) {
            text = record.plainText;
          }
          native.nativeClipboardChanged(text);
        }).catch((err: BusinessError) => {
          hilog.warn(DOMAIN, TAG, 'pasteboard getData failed: %{public}s', JSON.stringify(err));
        });
      });

      hilog.info(DOMAIN, TAG, 'clipboard bridge wired');
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'clipboard bridge failed: %{public}s', JSON.stringify(e));
    }
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Index', (err) => {
      if (err.code) {
        hilog.error(DOMAIN, TAG, 'loadContent failed: %{public}s', JSON.stringify(err));
        return;
      }

      const win = windowStage.getMainWindowSync();
      mainWindow = win;

      // Report density and safe-area insets to the native layer.  The
      // XComponent (and therefore the native surface) is created by Index.ets;
      // these values are read when the window is opened.
      try {
        const d = display.getDefaultDisplaySync();
        native.nativeSetDensity(d.densityDPI / 160);

        const area = win.getWindowAvoidArea(window.AvoidAreaType.TYPE_SYSTEM);
        const top = px2vp(area.topRect.height);
        const bottom = px2vp(area.bottomRect.height);
        const left = px2vp(area.leftRect.width);
        const right = px2vp(area.rightRect.width);
        native.nativeSetSafeAreaInsets(top, bottom, left, right);
      } catch (e) {
        hilog.warn(DOMAIN, TAG, 'native display config failed: %{public}s', JSON.stringify(e));
      }

      this.reportAppearance();
    });
  }

  /**
   * Report the system color mode to native so GPUI's `window_appearance`
   * follows the system setting instead of the hardcoded Dark.  Also called
   * from `onConfigurationUpdate` when the reader flips the system switch.
   */
  private reportAppearance(): void {
    try {
      const config = this.context.getApplicationContext().getConfiguration();
      const dark = config.colorMode === ConfigurationConstant.ColorMode.COLOR_MODE_DARK;
      native.nativeSetAppearance(dark ? 'dark' : 'light');
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'appearance report failed: %{public}s', JSON.stringify(e));
    }
  }

  onConfigurationUpdate(_newConfig: import('@kit.AbilityKit').Configuration): void {
    this.reportAppearance();
  }

  onMemoryLevel(_level: AbilityConstant.MemoryLevel): void {
    try {
      native.nativeOnMemoryWarning();
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeOnMemoryWarning failed');
    }
  }

  onForeground(): void {
    try {
      native.nativeOnForeground();
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeOnForeground failed');
    }
  }

  onBackground(): void {
    try {
      native.nativeOnBackground();
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeOnBackground failed');
    }
  }

  onDestroy(): void {
    try {
      native.nativeOnDestroy();
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeOnDestroy failed');
    }
  }

  onNewWant(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    if (want.uri) {
      AppStorage.setOrCreate<string>('gpui_deeplink', want.uri);
    }
  }
}
