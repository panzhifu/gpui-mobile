// File / folder picking and save dialogs for the GPUI OHOS NAPI module.
//
// `gpui_mobile::ohos::file_picker` owns the request/reply bookkeeping; the actual
// picker only exists in ArkTS (`@ohos.file.picker`), so every app has to register
// this callback once — the NDK exposes no `OH_*_Picker`.  The contract is:
//
//   native.nativeSetFolderPicker((requestId, mode) => { ... })
//   mode = "folders" | "files" | "mixed"
//        | "save\u0001<dir>\u0001<suggestedName>"   // save dialog (GPUI's
//                                                    // `prompt_for_new_path`)
//   -> native.nativeDeliverPick(requestId, paths.join('\u0001'))   // '' == cancel
//   -> native.nativeFailPick(requestId, reason)
//
// This file is the canonical implementation: copy it into an app and call
// `registerFolderPicker(native)` from the page that owns the `.so`.
//
// Two details matter for a book-library style consumer:
//
// 1. The picker answers with URIs (`file://docs/storage/...`).  The native side
//    wants real paths, and `FileUri.path` is the official mapping — inside the
//    app's mount namespace the URI's path component *is* the sandbox path.
// 2. The grant that comes with a picked folder is temporary.  It lives as long
//    as this process, so a library that is watched across restarts would lose
//    access.  `fileShare.persistPermission` upgrades it to a persistent grant;
//    it needs `ohos.permission.FILE_ACCESS_PERSIST` in `module.json5`
//    (system_grant / normal, so no runtime dialog).  Failure is logged and the
//    path is still delivered — a temporary grant is better than no picker.
//    On the native side the same grants are re-activated after a restart
//    (`nativeSetFilesDir` → `FileAccess::activate_persisted`).

import { fileIo, fileShare, fileUri, picker } from '@kit.CoreFileKit';
import { BusinessError } from '@kit.BasicServicesKit';
import { hilog } from '@kit.PerformanceAnalysisKit';

const DOMAIN: number = 0x0000;
const TAG: string = 'GpuiPicker';

/// Separator the native side splits paths on (`U+0001`).  Must match
/// `gpui_mobile::ohos::file_picker::SEPARATOR`.
const SEPARATOR: string = '\u0001';

/// The three native entry points this module needs.  Declared structurally so
/// the file can be copied into any app — pass that app's own NAPI export.
export interface FolderPickerHost {
  nativeSetFolderPicker(cb: (requestId: number, mode: string) => void): void;
  nativeDeliverPick(requestId: number, paths: string): void;
  nativeFailPick(requestId: number, reason: string): void;
}

function selectModeOf(mode: string): picker.DocumentSelectMode {
  if (mode === 'folders') {
    return picker.DocumentSelectMode.FOLDER;
  }
  if (mode === 'mixed') {
    return picker.DocumentSelectMode.MIXED;
  }
  return picker.DocumentSelectMode.FILE;
}

/// Upgrade a folder grant from "this process" to "until revoked".  Files are
/// left alone: their temporary grant is enough for the copy-and-forget flow, and
/// persisting every picked file would grow the system's grant table for nothing.
/// Save targets **are** persisted: the reader will write into that file across
/// sessions, so the same folder rule applies to it.
async function persistIfFolder(uri: string, path: string, mode: string): Promise<void> {
  try {
    const isFolder: boolean =
      mode === 'folders' ||
      mode === 'save' ||
      (mode === 'mixed' && fileIo.statSync(path).isDirectory());
    if (!isFolder) {
      return;
    }
    const policy: fileShare.PolicyInfo = {
      uri: uri,
      operationMode: fileShare.OperationMode.READ_MODE | fileShare.OperationMode.WRITE_MODE,
    };
    await fileShare.persistPermission([policy]);
    hilog.info(DOMAIN, TAG, 'persisted folder grant: %{public}s', path);
  } catch (err) {
    const e = err as BusinessError;
    hilog.warn(DOMAIN, TAG, 'persistPermission failed: %{public}d %{public}s', e.code, e.message);
  }
}

/// The save branch: `DocumentViewPicker.save` with the native side's directory
/// and suggested name folded into `newFileUri`.  The answer is a single URI.
async function runSave(host: FolderPickerHost, requestId: number, dir: string, name: string): Promise<void> {
  const fileName: string = name !== '' ? name : 'output.txt';
  const saveOptions: picker.DocumentSaveOptions = new picker.DocumentSaveOptions();
  saveOptions.newFileUri = fileUri.getUriFromPath(`${dir}/${fileName}`);

  new picker.DocumentViewPicker()
    .save(saveOptions)
    .then(async (uris: string[]) => {
      if (!uris || uris.length === 0) {
        host.nativeDeliverPick(requestId, '');
        return;
      }
      const path: string = new fileUri.FileUri(uris[0]).path;
      if (!path.startsWith('/storage/')) {
        hilog.warn(DOMAIN, TAG, 'unusable save target: %{public}s', uris[0]);
        host.nativeFailPick(requestId, `no real path for ${uris[0]}`);
        return;
      }
      await persistIfFolder(uris[0], path, 'save');
      host.nativeDeliverPick(requestId, path);
    })
    .catch((err: Error) => {
      hilog.warn(DOMAIN, TAG, 'save rejected: %{public}s', err?.message ?? 'unknown');
      host.nativeDeliverPick(requestId, '');
    });
}

/// Register the picker.  Call once, after the native module is imported: the
/// native side stores the callback in a `napi_reference`, so a later request can
/// arrive at any time.
export function registerFolderPicker(host: FolderPickerHost): void {
  host.nativeSetFolderPicker((requestId: number, mode: string) => {
    // Save requests carry their payload in the mode string:
    // `save\u0001<dir>\u0001<suggestedName>`.
    if (mode.startsWith('save')) {
      const parts: string[] = mode.split(SEPARATOR);
      void runSave(host, requestId, parts[1] ?? '', parts[2] ?? '');
      return;
    }

    const options: picker.DocumentSelectOptions = new picker.DocumentSelectOptions();
    options.selectMode = selectModeOf(mode);
    // Folder mode ignores this (one directory per pick before API 26; API 26 has
    // `allowsMulFolderSelection`, which the bridge does not carry yet).
    options.maxSelectNumber = mode === 'files' ? 20 : 1;

    new picker.DocumentViewPicker()
      .select(options)
      .then(async (uris: string[]) => {
        if (!uris || uris.length === 0) {
          host.nativeDeliverPick(requestId, '');
          return;
        }
        const paths: string[] = [];
        for (const uri of uris) {
          const path: string = new fileUri.FileUri(uri).path;
          // Media-library URIs (`file://media/...`) are fd-access only; handing
          // those to `std::fs` would fail later with a much worse error.
          if (!path.startsWith('/storage/')) {
            hilog.warn(DOMAIN, TAG, 'unusable pick: %{public}s', uri);
            host.nativeFailPick(requestId, `no real path for ${uri}`);
            return;
          }
          await persistIfFolder(uri, path, mode);
          paths.push(path);
        }
        hilog.info(DOMAIN, TAG, 'picked %{public}d path(s)', paths.length);
        host.nativeDeliverPick(requestId, paths.join(SEPARATOR));
      })
      .catch((err: Error) => {
        // Backing out of the picker arrives as a rejection; keep the reason in
        // the log so a real failure is not indistinguishable from cancel.
        hilog.warn(DOMAIN, TAG, 'picker rejected: %{public}s', err?.message ?? 'unknown');
        host.nativeDeliverPick(requestId, '');
      });
  });
  hilog.info(DOMAIN, TAG, 'folder picker registered');
}
