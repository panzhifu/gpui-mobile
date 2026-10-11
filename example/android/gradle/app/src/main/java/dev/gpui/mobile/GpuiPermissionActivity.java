package dev.gpui.mobile;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.atomic.AtomicIntegerArray;

/**
 * Transparent helper Activity for handling runtime permission requests.
 *
 * <p>NativeActivity does not receive onRequestPermissionsResult callbacks,
 * so this lightweight Activity is used as a proxy.</p>
 *
 * <p>Handles process death: if the process is killed while the permission
 * dialog is showing, the recreated Activity gracefully finishes without
 * crashing.</p>
 */
public class GpuiPermissionActivity extends Activity {

    private static final int PERMISSION_REQUEST_CODE = 9002;
    /** Settings round-trip (e.g. MANAGE_EXTERNAL_STORAGE "All files access"). */
    private static final int SETTINGS_REQUEST_CODE = 9003;
    private static final String KEY_WAITING = "gpui_waiting_for_permission";
    private static final String KEY_WAITING_SETTINGS = "gpui_waiting_for_settings";

    /** Latch that the calling thread waits on. */
    static CountDownLatch sLatch;
    /** Permissions to request. */
    static String[] sPermissions;
    /** Grant results (PackageManager.PERMISSION_GRANTED or DENIED). */
    static AtomicIntegerArray sResults;

    /** Settings round-trip: the intent to fire, and the latch to release on return. */
    static Intent sSettingsIntent;
    static CountDownLatch sSettingsLatch;

    /** Whether we are waiting for a permission result. */
    private boolean mWaitingForResult = false;
    /** Whether we are waiting for the user to come back from a settings page. */
    private boolean mWaitingForSettings = false;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        if (savedInstanceState != null && savedInstanceState.getBoolean(KEY_WAITING, false)) {
            // Process was killed while permission dialog was showing.
            // The system will re-deliver the result via onRequestPermissionsResult.
            android.util.Log.i("GpuiPermission", "Recreated after process death, waiting for result");
            mWaitingForResult = true;
            return;
        }

        if (sSettingsIntent != null && sSettingsLatch != null) {
            // Settings round-trip (e.g. "All files access"): fire the intent and
            // release the latch in onActivityResult when the user comes back.
            mWaitingForSettings = true;
            startActivityForResult(sSettingsIntent, SETTINGS_REQUEST_CODE);
            return;
        }

        if (sPermissions != null && sPermissions.length > 0) {
            mWaitingForResult = true;
            requestPermissions(sPermissions, PERMISSION_REQUEST_CODE);
        } else {
            deliverResult();
            finish();
        }
    }

    @Override
    protected void onSaveInstanceState(Bundle outState) {
        super.onSaveInstanceState(outState);
        outState.putBoolean(KEY_WAITING, mWaitingForResult);
        outState.putBoolean(KEY_WAITING_SETTINGS, mWaitingForSettings);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == SETTINGS_REQUEST_CODE) {
            // The caller re-checks the actual grant (isExternalStorageManager);
            // our job is only to release the latch.
            mWaitingForSettings = false;
            releaseSettingsLatch();
            finish();
        }
    }

    private void releaseSettingsLatch() {
        if (sSettingsLatch != null) {
            sSettingsLatch.countDown();
            sSettingsLatch = null;
            sSettingsIntent = null;
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        mWaitingForResult = false;

        if (requestCode == PERMISSION_REQUEST_CODE && sResults != null) {
            for (int i = 0; i < grantResults.length && i < sResults.length(); i++) {
                sResults.set(i, grantResults[i]);
            }
        }

        deliverResult();
        finish();
    }

    @Override
    public void onBackPressed() {
        mWaitingForResult = false;
        mWaitingForSettings = false;
        releaseSettingsLatch();
        deliverResult();
        super.onBackPressed();
    }

    private void deliverResult() {
        if (sLatch != null) {
            sLatch.countDown();
        } else {
            // Process death recovery — latch is gone. Just finish gracefully.
            android.util.Log.i("GpuiPermission", "No latch (process death recovery), finishing");
        }
        // If this instance was recreated for a settings round-trip that its
        // process can no longer answer, release that latch too so the waiting
        // thread does not hang forever.
        if (isFinishing() && mWaitingForSettings) {
            releaseSettingsLatch();
        }
    }
}
