import {
  AbilityConstant, Configuration, ConfigurationConstant, MemoryLevel, UIAbility, Want,
} from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { display } from '@kit.ArkUI';
import { hilog } from '@kit.PerformanceAnalysisKit';
import native from 'libgpui_mobile_example.so';
import { pasteboard } from '@kit.BasicServicesKit';
import { setMainWindow } from '../common/window_holder';

const DOMAIN: number = 0x0000;
const TAG: string = 'GpuiExample';

export default class EntryAbility extends UIAbility {
  onCreate(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    hilog.info(DOMAIN, TAG, 'EntryAbility onCreate');
    this.forwardWant(want);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Index', (err) => {
      if (err.code) {
        hilog.error(DOMAIN, TAG, 'loadContent failed: %{public}s', JSON.stringify(err));
        return;
      }

      const win = windowStage.getMainWindowSync();
      setMainWindow(win);
      this.publishDisplayConfig(win);
    });
  }

  onForeground(): void {
    try {
      native.nativeOnForeground();
      // The window resets system-bar colours while backgrounded; bump a token
      // so Index.ets re-applies the last native style on every foreground.
      AppStorage.setOrCreate<number>('gpui_chrome_reapply', Date.now());
      const data = pasteboard.getSystemPasteboard().getDataSync();
      const text = data?.getPrimaryText?.() ?? '';
      if (text.length > 0) { native.nativeClipboardChanged(text); }
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeOnForeground / clipboard failed');
    }
  }

  onConfigurationUpdate(config: Configuration): void {
    try {
      const dark = config.colorMode === ConfigurationConstant.ColorMode.DARK;
      native.nativeSetAppearance(dark ? 1 : 0);
      const win = getMainWindow();
      if (win) { this.publishDisplayConfig(win); }
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'native configuration update failed: %{public}s', JSON.stringify(e));
    }
  }

  onMemoryLevel(level: MemoryLevel): void {
    try {
      hilog.info(DOMAIN, TAG, 'memory level: %{public}d', level);
      native.nativeMemoryWarning();
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'native memory warning failed: %{public}s', JSON.stringify(e));
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
    this.forwardWant(want);
  }

  private publishDisplayConfig(win: window.Window): void {
    // Report density and safe-area insets to the native layer.  The
    // XComponent surface is created by Index.ets; these values are also
    // refreshed on configuration changes such as rotation and font scaling.
    try {
      const d = display.getDefaultDisplaySync();
      native.nativeSetDensity(d.densityDPI / 160);

      const config = this.context.getApplicationContext().getConfiguration();
      const dark = config.colorMode === ConfigurationConstant.ColorMode.DARK;
      native.nativeSetAppearance(dark ? 1 : 0);

      const area = win.getWindowAvoidArea(window.AvoidAreaType.TYPE_SYSTEM);
      const top = px2vp(area.topRect.height);
      const bottom = px2vp(area.bottomRect.height);
      const left = px2vp(area.leftRect.width);
      const right = px2vp(area.rightRect.width);
      native.nativeSetSafeAreaInsets(top, bottom, left, right);
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'native display config failed: %{public}s', JSON.stringify(e));
    }
  }

  private forwardWant(want: Want): void {
    if (!want.uri) {
      return;
    }
    // A deeplink can arrive before Index.ets registers its NAPI bridge.
    // Queue it in AppStorage and let the page deliver it once it is ready.
    const queued = AppStorage.get<string[]>('gpui_deeplinks') ?? [];
    queued.push(want.uri);
    AppStorage.setOrCreate<string[]>('gpui_deeplinks', queued);
    AppStorage.setOrCreate<number>('gpui_deeplink_seq', Date.now() + queued.length);
  }
}
