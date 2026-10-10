package dev.gpui.mobile;

import android.app.Activity;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.Environment;
import android.os.Looper;
import android.provider.DocumentsContract;
import android.provider.OpenableColumns;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.UUID;
import java.util.concurrent.CountDownLatch;

/** GPUI file prompts import SAF documents into cache before releasing URI access. */
public final class GpuiPathPicker extends Activity {
    private static final int PICK_FILES = 9010;
    private static final int PICK_FOLDER = 9011;
    private static Request pending;
    private Request request;

    private static final class Request {
        final CountDownLatch done = new CountDownLatch(1);
        final boolean multiple;
        final boolean directory;
        String[] paths;
        Exception error;

        Request(boolean multiple, boolean directory) {
            this.multiple = multiple;
            this.directory = directory;
        }

        synchronized void complete(String[] paths, Exception error) {
            if (done.getCount() == 0) return;
            this.paths = paths;
            this.error = error;
            done.countDown();
        }
    }

    /** Called by a Rust worker, never the UI or GPUI event thread. */
    public static String[] openFiles(Activity activity, boolean multiple) throws Exception {
        return open(activity, new Request(multiple, false));
    }

    /**
     * Pick a single directory tree via {@code ACTION_OPEN_DOCUMENT_TREE}.
     *
     * <p>Returns the folder as a filesystem path when it lives on a local volume
     * (internal storage or an SD card), and falls back to the {@code content://}
     * tree URI otherwise — so the caller always gets a usable string. The picked
     * URI is also persisted for read/write, matching SDL's folder dialog.</p>
     */
    public static String openFolder(Activity activity) throws Exception {
        String[] paths = open(activity, new Request(false, true));
        return (paths != null && paths.length > 0) ? paths[0] : null;
    }

    /** Called by a Rust worker, never the UI or GPUI event thread. */
    private static String[] open(Activity activity, Request current) throws Exception {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            throw new IllegalStateException("File prompts must run on a worker thread");
        }
        synchronized (GpuiPathPicker.class) {
            if (pending != null) throw new IllegalStateException("A file picker is already open");
            pending = current;
        }
        try {
            activity.runOnUiThread(() -> {
                try {
                    activity.startActivity(new Intent(activity, GpuiPathPicker.class));
                } catch (Exception error) {
                    current.complete(null, error);
                }
            });
            current.done.await();
            if (current.error != null) throw current.error;
            return current.paths;
        } finally {
            synchronized (GpuiPathPicker.class) {
                if (pending == current) pending = null;
            }
        }
    }

    @Override protected void onCreate(Bundle state) {
        super.onCreate(state);
        synchronized (GpuiPathPicker.class) { request = pending; }
        // After process death the original GPUI request no longer exists.
        if (request == null) { finish(); return; }
        if (state != null) return;
        Intent intent;
        if (request.directory) {
            intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
            // Ask for durable read/write access so the tree URI survives a
            // process restart (SDL gates this behind a hint; a folder picker
            // almost always wants it, so request it unconditionally).
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION
                    | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                    | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
        } else {
            intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, request.multiple);
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        }
        int code = request.directory ? PICK_FOLDER : PICK_FILES;
        try {
            startActivityForResult(intent, code);
        } catch (Exception error) {
            request.complete(null, error);
            finish();
        }
    }

    @Override protected void onActivityResult(int code, int result, Intent data) {
        super.onActivityResult(code, result, data);
        if (request == null) return;

        if (request.directory) {
            if (code != PICK_FOLDER) return;
            if (result != RESULT_OK || data == null || data.getData() == null) {
                request.complete(null, null);
                finish();
                return;
            }
            Uri treeUri = data.getData();
            // Persisting can throw when the provider is not persistable; the
            // grant is a bonus, so keep the transient one on failure.
            try {
                getContentResolver().takePersistableUriPermission(treeUri,
                        Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            } catch (Exception ignored) {
            }
            String path = resolveTreeUri(treeUri);
            request.complete(new String[] { path != null ? path : treeUri.toString() }, null);
            finish();
            return;
        }

        if (code != PICK_FILES) return;
        if (result != RESULT_OK || data == null) {
            request.complete(null, null);
            finish();
            return;
        }
        ArrayList<Uri> uris = new ArrayList<>();
        if (data.getClipData() != null) {
            for (int i = 0; i < data.getClipData().getItemCount(); i++) {
                Uri uri = data.getClipData().getItemAt(i).getUri();
                if (uri != null) uris.add(uri);
            }
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        // Keep this Activity (and its temporary URI grants) alive while reading.
        // Files from remote providers can be large, so copying cannot run on UI.
        new Thread(() -> {
            ArrayList<File> imported = new ArrayList<>();
            try {
                for (Uri uri : uris) imported.add(importFile(uri));
                String[] paths = new String[imported.size()];
                for (int i = 0; i < paths.length; i++) paths[i] = imported.get(i).getAbsolutePath();
                request.complete(paths, null);
            } catch (Exception error) {
                for (File file : imported) {
                    file.delete();
                    file.getParentFile().delete();
                }
                request.complete(null, error);
            } finally {
                runOnUiThread(this::finish);
            }
        }, "gpui-file-import").start();
    }

    /**
     * Best-effort map a SAF tree URI to a filesystem path.
     *
     * <p>Only the external-storage provider exposes a path stable enough to use:
     * its tree document id is {@code <volume>:<relative>}, where {@code primary}
     * is {@link Environment#getExternalStorageDirectory()} and any other volume
     * name is a removable volume mounted at {@code /storage/<volume>}. Other
     * providers (Downloads, cloud) return {@code null} so the caller keeps the
     * URI instead.</p>
     */
    private static String resolveTreeUri(Uri treeUri) {
        try {
            String documentId = DocumentsContract.getTreeDocumentId(treeUri);
            int colon = documentId.indexOf(':');
            if (colon < 0) return null;
            String volume = documentId.substring(0, colon);
            String relative = documentId.substring(colon + 1);
            String base;
            if ("primary".equalsIgnoreCase(volume)) {
                base = Environment.getExternalStorageDirectory().getAbsolutePath();
            } else {
                base = "/storage/" + volume;
                if (!new File(base).isDirectory()) return null;
            }
            return relative.isEmpty() ? base : base + "/" + relative;
        } catch (Exception ignored) {
            return null;
        }
    }

    private File importFile(Uri uri) throws IOException {
        String name = null;
        try (Cursor cursor = getContentResolver().query(uri,
                new String[] { OpenableColumns.DISPLAY_NAME }, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) name = cursor.getString(0);
        }
        // Keep the original extension and Unicode filename for attachment checks,
        // but never allow a provider's display name to escape the import directory.
        if (name == null || name.isEmpty()) name = uri.getLastPathSegment();
        if (name == null) name = "attachment";
        name = name.replace('/', '_').replace('\\', '_').replace('\0', '_');
        if (name.isEmpty() || name.equals(".") || name.equals("..")) name = "attachment";
        File directory = new File(getCacheDir(), "gpui-import-" + UUID.randomUUID());
        if (!directory.mkdir()) throw new IOException("Could not create import directory");
        File file = new File(directory, name);
        try (InputStream input = getContentResolver().openInputStream(uri);
             FileOutputStream output = new FileOutputStream(file)) {
            if (input == null) throw new IOException("Could not read selected document");
            byte[] buffer = new byte[64 * 1024];
            int count;
            while ((count = input.read(buffer)) != -1) output.write(buffer, 0, count);
        } catch (Exception error) {
            file.delete();
            directory.delete();
            throw error;
        }
        return file;
    }

    @Override protected void onDestroy() {
        if (isFinishing() && request != null) request.complete(null, null);
        super.onDestroy();
    }
}
