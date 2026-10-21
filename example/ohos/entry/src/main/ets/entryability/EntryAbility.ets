import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { display } from '@kit.ArkUI';
import { hilog } from '@kit.PerformanceAnalysisKit';
import native from 'libgpui_mobile_example.so';

const DOMAIN: number = 0x0000;
const TAG: string = 'GpuiExample';

export default class EntryAbility extends UIAbility {
  onCreate(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    hilog.info(DOMAIN, TAG, 'EntryAbility onCreate');
    // A deeplink (gpui://screen) launches the app; stash it for the native side.
    if (want.uri) {
      AppStorage.setOrCreate<string>('gpui_deeplink', want.uri);
    }
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Index', (err) => {
      if (err.code) {
        hilog.error(DOMAIN, TAG, 'loadContent failed: %{public}s', JSON.stringify(err));
        return;
      }

      const win = windowStage.getMainWindowSync();

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
    });
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
