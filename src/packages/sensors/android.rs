use super::{
    dispatch_sample, latest, set_handler, BarometerData, SensorAvailability, SensorData, SensorKind,
};
use crate::android::jni as jni_helpers;
use jni::objects::JValue;
use std::sync::atomic::{AtomicBool, Ordering};

const HELPER_CLASS: &str = "dev.gpui.mobile.GpuiSensors";

// Android Sensor.TYPE_* constants
const TYPE_ACCELEROMETER: i32 = 1;
const TYPE_GYROSCOPE: i32 = 4;
const TYPE_MAGNETIC_FIELD: i32 = 2;
const TYPE_PRESSURE: i32 = 6;

/// Sensor ids `GpuiSensors.start()` / `.stop()` take; keep in sync with
/// `GpuiSensors.java`.
const SENSOR_ACCELEROMETER: i32 = 0;
const SENSOR_GYROSCOPE: i32 = 1;
const SENSOR_MAGNETOMETER: i32 = 2;
const SENSOR_BAROMETER: i32 = 3;

/// Whether `GpuiSensors` is reporting each sensor, so a screen that reads one
/// every frame costs a single JNI call per subscription rather than per frame.
static LISTENING: [AtomicBool; 4] = [const { AtomicBool::new(false) }; 4];

pub fn available_sensors() -> SensorAvailability {
    jni_helpers::with_env(|env| {
        let activity = jni_helpers::activity(env)?;

        let sm = match get_sensor_manager(env, &activity) {
            Some(sm) => sm,
            None => return Ok(SensorAvailability::default()),
        };

        Ok(SensorAvailability {
            accelerometer: has_sensor(env, &sm, TYPE_ACCELEROMETER),
            gyroscope: has_sensor(env, &sm, TYPE_GYROSCOPE),
            magnetometer: has_sensor(env, &sm, TYPE_MAGNETIC_FIELD),
            barometer: has_sensor(env, &sm, TYPE_PRESSURE),
        })
    })
    .unwrap_or_default()
}

pub fn accelerometer() -> Option<SensorData> {
    read(SensorKind::Accelerometer)
}

pub fn gyroscope() -> Option<SensorData> {
    read(SensorKind::Gyroscope)
}

pub fn magnetometer() -> Option<SensorData> {
    read(SensorKind::Magnetometer)
}

pub fn barometer() -> Option<BarometerData> {
    // `Sensor.TYPE_PRESSURE` carries its one value in `values[0]`.
    read(SensorKind::Barometer).map(|d| BarometerData { pressure: d.x })
}

pub(crate) fn subscribe(
    kind: SensorKind,
    handler: impl Fn(SensorData) + Send + 'static,
) -> Result<(), String> {
    set_handler(kind, Some(Box::new(handler)));
    // An app calling this took control of the subscription, so make a real
    // attempt instead of honouring the once-per-sensor shortcut a poll uses --
    // otherwise retrying after a failure would answer `Ok` while delivering
    // nothing.
    LISTENING[kind.index()].store(false, Ordering::Release);
    start(kind)
}

pub(crate) fn unsubscribe(kind: SensorKind) -> Result<(), String> {
    set_handler(kind, None);
    LISTENING[kind.index()].store(false, Ordering::Release);
    stop(kind)
}

/// Start reporting `kind` if it is not already, then hand back the newest sample.
fn read(kind: SensorKind) -> Option<SensorData> {
    if let Err(e) = start(kind) {
        log::warn!("sensors: subscribing to {kind:?} failed: {e}");
    }
    latest(kind)
}

fn start(kind: SensorKind) -> Result<(), String> {
    if LISTENING[kind.index()].swap(true, Ordering::AcqRel) {
        return Ok(());
    }

    jni_helpers::with_env(|env| {
        let activity = jni_helpers::activity(env)?;
        let cls = jni_helpers::find_app_class(env, HELPER_CLASS)?;

        env.call_static_method(
            &cls,
            jni::jni_str!("start"),
            jni::jni_sig!("(Landroid/app/Activity;I)V"),
            &[JValue::Object(&activity), JValue::Int(sensor_id(kind))],
        )
        .map_err(|e| {
            env.exception_clear();
            e.to_string()
        })?;

        Ok(())
    })
}

fn stop(kind: SensorKind) -> Result<(), String> {
    jni_helpers::with_env(|env| {
        let cls = jni_helpers::find_app_class(env, HELPER_CLASS)?;

        env.call_static_method(
            &cls,
            jni::jni_str!("stop"),
            jni::jni_sig!("(I)V"),
            &[JValue::Int(sensor_id(kind))],
        )
        .map_err(|e| {
            env.exception_clear();
            e.to_string()
        })?;

        Ok(())
    })
}

fn sensor_id(kind: SensorKind) -> i32 {
    match kind {
        SensorKind::Accelerometer => SENSOR_ACCELEROMETER,
        SensorKind::Gyroscope => SENSOR_GYROSCOPE,
        SensorKind::Magnetometer => SENSOR_MAGNETOMETER,
        SensorKind::Barometer => SENSOR_BAROMETER,
    }
}

/// Store a sample pushed by `GpuiSensors` and deliver it to any subscriber.
pub(crate) fn notify_sample(sensor: i32, x: f32, y: f32, z: f32) {
    let kind = match sensor {
        SENSOR_ACCELEROMETER => SensorKind::Accelerometer,
        SENSOR_GYROSCOPE => SensorKind::Gyroscope,
        SENSOR_MAGNETOMETER => SensorKind::Magnetometer,
        SENSOR_BAROMETER => SensorKind::Barometer,
        other => {
            log::warn!("sensors: sample for unknown sensor id {other}");
            return;
        }
    };

    let data = SensorData {
        x: x as f64,
        y: y as f64,
        z: z as f64,
    };
    if dispatch_sample(kind, data) {
        // The read that subscribed returned `None` and drew nothing new, so
        // without a forced frame there would be no further read to notice this
        // sample. Reuses the platform's existing "state changed outside GPUI's
        // invalidator" hook; it costs one frame per subscription, not per
        // sample, because only an empty-to-filled transition reports newness.
        crate::mark_text_input_dirty();
    }
}

fn get_sensor_manager<'local>(
    env: &mut jni::Env<'local>,
    activity: &jni::objects::JObject<'_>,
) -> Option<jni::objects::JObject<'local>> {
    let service_name = env.new_string("sensor").ok()?;
    let sm = env
        .call_method(
            activity,
            jni::jni_str!("getSystemService"),
            jni::jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
            &[JValue::Object(&service_name)],
        )
        .and_then(|v| v.l())
        .ok()?;
    if sm.is_null() {
        env.exception_clear();
        None
    } else {
        Some(sm)
    }
}

fn has_sensor(env: &mut jni::Env<'_>, sm: &jni::objects::JObject<'_>, sensor_type: i32) -> bool {
    match env
        .call_method(
            sm,
            jni::jni_str!("getDefaultSensor"),
            jni::jni_sig!("(I)Landroid/hardware/Sensor;"),
            &[JValue::Int(sensor_type)],
        )
        .and_then(|v| v.l())
    {
        Ok(sensor) => !sensor.is_null(),
        Err(_) => {
            env.exception_clear();
            false
        }
    }
}
