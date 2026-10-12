package dev.gpui.mobile;

import android.app.Activity;
import android.content.Context;
import android.hardware.Sensor;
import android.hardware.SensorEvent;
import android.hardware.SensorEventListener;
import android.hardware.SensorManager;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;

/**
 * Sensor sampling for the Rust {@code sensors} package.
 *
 * Rust calls {@link #start(Activity, int)} the first time an app reads a sensor.
 * Every sample after that is handed to
 * {@link #nativeSensorSample(int, float, float, float)} as it arrives, so the
 * newest reading lives in native memory and {@code sensors::accelerometer()} is
 * a plain read of it.
 *
 * Registrations are dropped by {@link #pause()} and re-armed by
 * {@link #resume()}; the Rust event loop calls both from the Activity's
 * pause/resume. Sampling costs power whether or not anyone reads the values, so
 * a backgrounded app must not keep a listener registered.
 *
 * Readings keep Android's own frame — {@code SensorEvent.values} as the HAL
 * delivers it, i.e. the device's natural pose, not rotated to match the current
 * display orientation. iOS reports the same frame.
 *
 * Requested-sensor state is confined to the main thread: {@code start},
 * {@code pause} and {@code resume} are called from the Rust loop thread, so each
 * marshals onto {@link #sMainHandler} rather than touching the
 * {@link SensorManager} directly.
 */
public class GpuiSensors {

    private static final String TAG = "GpuiSensors";

    /** Sensor ids Rust subscribes with; keep in sync with sensors/android.rs. */
    private static final int ID_ACCELEROMETER = 0;
    private static final int ID_GYROSCOPE = 1;
    private static final int ID_MAGNETOMETER = 2;
    private static final int ID_BAROMETER = 3;

    private static final int ID_COUNT = 4;

    /**
     * Godot asks for {@code SENSOR_DELAY_GAME} (50–100 Hz) because a game
     * consumes every event it gets. These readings are polled instead, so the
     * cheaper UI rate (~60 ms between samples) is enough.
     */
    private static final int SAMPLING_RATE = SensorManager.SENSOR_DELAY_UI;

    private static final Handler sMainHandler = new Handler(Looper.getMainLooper());

    /** Set once, from the first {@link #start} that found a SensorManager. */
    private static volatile SensorManager sSensorManager;

    /** Which sensor ids Rust has asked for. Main thread only. */
    private static final boolean[] sRequested = new boolean[ID_COUNT];

    /** False between {@link #pause()} and {@link #resume()}. Main thread only. */
    private static boolean sRunning = true;

    private static final SensorEventListener sListener = new SensorEventListener() {
        @Override
        public void onSensorChanged(SensorEvent event) {
            float[] values = event.values;
            if (values == null || values.length < 1) {
                return;
            }
            int id = idForType(event.sensor.getType());
            if (id < 0) {
                return;
            }
            // Primitives only — this runs at up to ~100 Hz per sensor.
            // TYPE_PRESSURE carries a single value, so the missing axes are 0.
            float x = values[0];
            float y = values.length > 1 ? values[1] : 0f;
            float z = values.length > 2 ? values[2] : 0f;
            nativeSensorSample(id, x, y, z);
        }

        @Override
        public void onAccuracyChanged(Sensor sensor, int accuracy) {
            // This class reports readings, not accuracy.
        }
    };

    /**
     * Subscribe to one sensor. Idempotent, and safe to call while backgrounded:
     * the request is remembered and gets a listener as soon as {@link #resume()}
     * runs.
     *
     * @param activity used only to reach the {@code SENSOR_SERVICE}.
     * @param id       one of the {@code ID_*} values Rust passes through.
     */
    public static void start(final Activity activity, final int id) {
        final SensorManager manager = obtainManager(activity);
        if (manager == null) {
            Log.w(TAG, "No SensorManager; sensor id " + id + " is unavailable");
            return;
        }
        if (id < 0 || id >= ID_COUNT) {
            Log.w(TAG, "Unknown sensor id " + id);
            return;
        }
        sMainHandler.post(new Runnable() {
            @Override
            public void run() {
                Sensor sensor = manager.getDefaultSensor(typeForId(id));
                if (sensor == null) {
                    Log.i(TAG, "Device has no sensor id " + id);
                    return;
                }
                if (sRequested[id]) {
                    return;
                }
                sRequested[id] = true;
                if (sRunning) {
                    register(manager, sensor);
                }
            }
        });
    }

    /**
     * Stop sampling. Remembered requests survive, so {@link #resume()} re-arms
     * exactly what was being read.
     */
    public static void pause() {
        final SensorManager manager = sSensorManager;
        if (manager == null) {
            return;
        }
        sMainHandler.post(new Runnable() {
            @Override
            public void run() {
                sRunning = false;
                manager.unregisterListener(sListener);
            }
        });
    }

    /**
     * Re-subscribe every sensor {@link #start} had asked for before {@link #pause()}.
     *
     * Requests are deliberately kept for the life of the process: a host that
     * recreates its Activity while GPUI keeps running (see {@code android::host})
     * must not lose the pairing with Rust, which subscribes each sensor once.
     */
    public static void resume() {
        final SensorManager manager = sSensorManager;
        if (manager == null) {
            return;
        }
        sMainHandler.post(new Runnable() {
            @Override
            public void run() {
                sRunning = true;
                for (int id = 0; id < ID_COUNT; id++) {
                    if (!sRequested[id]) {
                        continue;
                    }
                    Sensor sensor = manager.getDefaultSensor(typeForId(id));
                    if (sensor != null) {
                        register(manager, sensor);
                    }
                }
            }
        });
    }

    private static void register(SensorManager manager, Sensor sensor) {
        // maxReportLatencyUs = 0: batching would hold samples back, and a
        // freshly polled reading is the whole point of this class.
        if (!manager.registerListener(sListener, sensor, SAMPLING_RATE, 0, sMainHandler)) {
            Log.w(TAG, "registerListener failed for " + sensor.getName());
        }
    }

    private static SensorManager obtainManager(Activity activity) {
        SensorManager manager = sSensorManager;
        if (manager != null || activity == null) {
            return manager;
        }
        manager = (SensorManager) activity.getApplicationContext()
                .getSystemService(Context.SENSOR_SERVICE);
        sSensorManager = manager;
        return manager;
    }

    private static int typeForId(int id) {
        switch (id) {
            case ID_GYROSCOPE:
                return Sensor.TYPE_GYROSCOPE;
            case ID_MAGNETOMETER:
                return Sensor.TYPE_MAGNETIC_FIELD;
            case ID_BAROMETER:
                return Sensor.TYPE_PRESSURE;
            case ID_ACCELEROMETER:
            default:
                return Sensor.TYPE_ACCELEROMETER;
        }
    }

    private static int idForType(int type) {
        switch (type) {
            case Sensor.TYPE_ACCELEROMETER:
                return ID_ACCELEROMETER;
            case Sensor.TYPE_GYROSCOPE:
                return ID_GYROSCOPE;
            case Sensor.TYPE_MAGNETIC_FIELD:
                return ID_MAGNETOMETER;
            case Sensor.TYPE_PRESSURE:
                return ID_BAROMETER;
            default:
                return -1;
        }
    }

    /**
     * Push one reading into native memory.
     *
     * @param id the same sensor id {@link #start} was called with.
     */
    private static native void nativeSensorSample(int id, float x, float y, float z);
}
