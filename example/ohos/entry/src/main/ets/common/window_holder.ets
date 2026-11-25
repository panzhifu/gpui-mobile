import { window } from '@kit.ArkUI';

/// The main window is created by the Ability, but the system-bar style is
/// applied from the page.  Keep a module-level reference so the page does not
/// have to await `window.getLastWindow()` on every poll.
let mainWindow: window.Window | undefined = undefined;

export function setMainWindow(win: window.Window): void {
  mainWindow = win;
}

export function getMainWindow(): window.Window | undefined {
  return mainWindow;
}
