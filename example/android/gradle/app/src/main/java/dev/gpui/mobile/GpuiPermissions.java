package dev.gpui.mobile;

import android.Manifest;
import android.app.Activity;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothManager;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.location.LocationManager;
import android.os.Environment;
import android.net.Uri;
import android.os.Build;
import android.provider.Settings;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.atomic.AtomicIntegerArray;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Permission handling helper for the GPUI permission_handler package.
 *
 * <p>Uses a transparent GpuiPermissionActivity to handle the permission request
 * callback, since NativeActivity doesn't support onRequestPermissionsResult.
 * The whole API is synchronous: the calling thread blocks on a latch until the
 * result (or the user returning from a system settings page) arrives — so every
 * entry point here must be called off the UI thread.</p>
 */
public final class GpuiPermissions {

    /** Status constants matching Rust's PermissionStatus enum. */
    private static final int STATUS_GRANTED = 0;
    private static final int STATUS_DENIED = 1;
    private static final int STATUS_PERMANENTLY_DENIED = 2;
    private static final int STATUS_RESTRICTED = 3;

    private static final int REQUEST_CODE = 9002;

    // ── Pending permission request state ─────────────────────────────

    static CountDownLatch sPermLatch;
    static AtomicIntegerArray sPermResults;
    static String[] sPendingPermissions;

    /**
     * Check a single permission.
     *
     * @return 0=granted, 1=denied, 2=permanently_denied
     */
    public static int checkPermission(Activity activity, String permission) {
        if (permission == null || permission.isEmpty()) return STATUS_GRANTED;

        // Special-case: SYSTEM_ALERT_WINDOW
        if ("android.permission.SYSTEM_ALERT_WINDOW".equals(permission)) {
            if (Build.VERSION.SDK_INT >= 23) {
                return Settings.canDrawOverlays(activity) ? STATUS_GRANTED : STATUS_DENIED;
            }
            return STATUS_GRANTED;
        }

        // Special-case: POST_NOTIFICATIONS (API 33+)
        if ("android.permission.POST_NOTIFICATIONS".equals(permission) && Build.VERSION.SDK_INT < 33) {
            return STATUS_GRANTED;
        }

        // Special-case: MANAGE_EXTERNAL_STORAGE ("All files access", API 30+).
        // It is an AppOp-style special permission, not a runtime permission:
        // check via isExternalStorageManager(), never via checkSelfPermission.
        // Below API 30 the grant does not exist — legacy READ_EXTERNAL_STORAGE covers it.
        if ("android.permission.MANAGE_EXTERNAL_STORAGE".equals(permission)) {
            if (Build.VERSION.SDK_INT >= 30) {
                return Environment.isExternalStorageManager() ? STATUS_GRANTED : STATUS_DENIED;
            }
            return checkPermission(activity, Manifest.permission.READ_EXTERNAL_STORAGE);
        }

        int result = activity.checkSelfPermission(permission);
        return (result == PackageManager.PERMISSION_GRANTED) ? STATUS_GRANTED : STATUS_DENIED;
    }

    /**
     * Request a single permission.
     *
     * <p>Launches GpuiPermissionActivity to handle the result callback.</p>
     *
     * @return 0=granted, 1=denied, 2=permanently_denied
     */
    public static int requestPermission(Activity activity, String permission) {
        if (permission == null || permission.isEmpty()) return STATUS_GRANTED;

        // Check if already granted
        if (checkPermission(activity, permission) == STATUS_GRANTED) {
            return STATUS_GRANTED;
        }

        // Special-case: SYSTEM_ALERT_WINDOW
        if ("android.permission.SYSTEM_ALERT_WINDOW".equals(permission)) {
            if (Build.VERSION.SDK_INT >= 23) {
                Intent intent = new Intent(Settings.ACTION_MANAGE_OVERLAY_PERMISSION,
                        Uri.parse("package:" + activity.getPackageName()));
                activity.startActivity(intent);
                return Settings.canDrawOverlays(activity) ? STATUS_GRANTED : STATUS_DENIED;
            }
            return STATUS_GRANTED;
        }

        // Special-case: MANAGE_EXTERNAL_STORAGE — bring the user to the system
        // settings page (Godot PermissionsUtil's approach) and block until they
        // come back, then re-check. Must be called off the UI thread (see the
        // class doc: the whole sync API latches).
        if ("android.permission.MANAGE_EXTERNAL_STORAGE".equals(permission)) {
            if (Build.VERSION.SDK_INT >= 30 && !Environment.isExternalStorageManager()) {
                boolean granted = requestManageExternalStorageViaActivity(activity);
                return granted ? STATUS_GRANTED : STATUS_DENIED;
            }
            return checkPermission(activity, permission);
        }

        return requestViaActivity(activity, new String[]{permission})[0];
    }

    /**
     * Request multiple permissions at once.
     *
     * @param permissions Pipe-separated permission strings.
     * @return Pipe-separated status ints.
     */
    public static String requestPermissions(Activity activity, String permissions) {
        if (permissions == null || permissions.isEmpty()) return "";

        String[] perms = permissions.split("\\|");
        int[] results = requestViaActivity(activity, perms);

        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < results.length; i++) {
            if (i > 0) sb.append("|");
            sb.append(results[i]);
        }
        return sb.toString();
    }

    /**
     * Check if a platform service is enabled.
     *
     * @param serviceType 0=location, 1=bluetooth
     */
    public static boolean isServiceEnabled(Activity activity, int serviceType) {
        switch (serviceType) {
            case 0: // Location
                LocationManager lm = (LocationManager) activity.getSystemService(Context.LOCATION_SERVICE);
                if (lm == null) return false;
                return lm.isProviderEnabled(LocationManager.GPS_PROVIDER)
                        || lm.isProviderEnabled(LocationManager.NETWORK_PROVIDER);
            case 1: // Bluetooth
                BluetoothManager bm = (BluetoothManager) activity.getSystemService(Context.BLUETOOTH_SERVICE);
                if (bm == null) return false;
                BluetoothAdapter adapter = bm.getAdapter();
                return adapter != null && adapter.isEnabled();
            default:
                return false;
        }
    }

    /**
     * Open the app's settings page.
     */
    public static boolean openAppSettings(Activity activity) {
        try {
            Intent intent = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS);
            intent.setData(Uri.parse("package:" + activity.getPackageName()));
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            activity.startActivity(intent);
            return true;
        } catch (Exception e) {
            android.util.Log.e("GpuiPermissions", "openAppSettings failed", e);
            return false;
        }
    }

    /**
     * Check if rationale should be shown for a permission.
     */
    public static boolean shouldShowRationale(Activity activity, String permission) {
        if (permission == null || permission.isEmpty()) return false;
        return activity.shouldShowRequestPermissionRationale(permission);
    }

    // ── Internal: request via helper Activity ────────────────────────

    private static int[] requestViaActivity(Activity activity, String[] permissions) {
        int[] results = new int[permissions.length];

        // Check which permissions are already granted
        boolean allGranted = true;
        for (int i = 0; i < permissions.length; i++) {
            if (checkPermission(activity, permissions[i]) == STATUS_GRANTED) {
                results[i] = STATUS_GRANTED;
            } else {
                results[i] = -1; // needs request
                allGranted = false;
            }
        }

        if (allGranted) return results;

        // MANAGE_EXTERNAL_STORAGE is not a runtime permission — feeding it to
        // requestPermissions() is a silent no-op. Route it through the settings
        // round-trip instead, then handle the rest as ordinary runtime requests.
        java.util.List<String> manage = new java.util.ArrayList<>();
        for (int i = 0; i < permissions.length; i++) {
            if ("android.permission.MANAGE_EXTERNAL_STORAGE".equals(permissions[i])
                    && results[i] == -1 && Build.VERSION.SDK_INT >= 30) {
                manage.add(permissions[i]);
            }
        }
        for (String permission : manage) {
            boolean granted = requestManageExternalStorageViaActivity(activity);
            for (int i = 0; i < permissions.length; i++) {
                if (permissions[i].equals(permission)) {
                    results[i] = granted ? STATUS_GRANTED : STATUS_DENIED;
                }
            }
        }

        // Filter to only ungranted permissions
        java.util.List<String> needed = new java.util.ArrayList<>();
        java.util.List<Integer> neededIndices = new java.util.ArrayList<>();
        for (int i = 0; i < permissions.length; i++) {
            if (results[i] == -1) {
                needed.add(permissions[i]);
                neededIndices.add(i);
            }
        }

        String[] neededArray = needed.toArray(new String[0]);

        // Use GpuiPermissionActivity for the request
        CountDownLatch latch = new CountDownLatch(1);
        GpuiPermissionActivity.sLatch = latch;
        GpuiPermissionActivity.sPermissions = neededArray;
        GpuiPermissionActivity.sResults = new AtomicIntegerArray(neededArray.length);
        for (int i = 0; i < neededArray.length; i++) {
            GpuiPermissionActivity.sResults.set(i, STATUS_DENIED);
        }

        Intent intent = new Intent(activity, GpuiPermissionActivity.class);
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        activity.startActivity(intent);

        try {
            latch.await();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }

        // Map results back
        for (int i = 0; i < neededIndices.size(); i++) {
            int idx = neededIndices.get(i);
            int grantResult = GpuiPermissionActivity.sResults.get(i);
            if (grantResult == PackageManager.PERMISSION_GRANTED) {
                results[idx] = STATUS_GRANTED;
            } else {
                // Check if permanently denied (shouldShowRequestPermissionRationale returns false after denial)
                if (!activity.shouldShowRequestPermissionRationale(neededArray[i])) {
                    results[idx] = STATUS_PERMANENTLY_DENIED;
                } else {
                    results[idx] = STATUS_DENIED;
                }
            }
        }

        return results;
    }

    /**
     * Bring the user to the system "All files access" settings page and block
     * until they come back (via GpuiPermissionActivity's onActivityResult).
     *
     * <p>The calling thread latches — same contract as the runtime-permission
     * path: call this off the UI thread or you will deadlock the main thread.</p>
     *
     * @return true if the user left the settings page with the grant on.
     */
    private static boolean requestManageExternalStorageViaActivity(Activity activity) {
        if (Build.VERSION.SDK_INT < 30) {
            return false;
        }
        if (Environment.isExternalStorageManager()) {
            return true;
        }

        // Godot PermissionsUtil's two intents: the package-scoped one lands
        // directly on our switch; the generic one is the fallback some skins need.
        Intent intent;
        try {
            intent = new Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION);
            intent.setData(Uri.parse("package:" + activity.getPackageName()));
        } catch (Exception ignored) {
            intent = new Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION);
        }
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);

        CountDownLatch latch = new CountDownLatch(1);
        GpuiPermissionActivity.sSettingsIntent = intent;
        GpuiPermissionActivity.sSettingsLatch = latch;

        activity.startActivity(new Intent(activity, GpuiPermissionActivity.class)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
        try {
            latch.await();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }

        return Environment.isExternalStorageManager();
    }

    private GpuiPermissions() {}
}
