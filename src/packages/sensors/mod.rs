//! Device sensor access — accelerometer, gyroscope, magnetometer, barometer.
//!
//! Every getter returns the **most recent delivered sample**, or `None` while no
//! sample has arrived yet. The first read of a sensor subscribes to it, so that
//! first read is `None` and the value shows up on a later read. Subscriptions
//! live while the app is in the foreground and are dropped when it leaves, so an
//! app that is never read costs no sensor power.
//!
//! ```ignore
//! if let Some(accel) = sensors::accelerometer() {
//!     // accel.x / accel.y / accel.z in m/s², device frame, gravity included
//! }
//! ```

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
mod ios;

/// A 3-axis sensor reading (x, y, z) in SI units.
///
/// - Accelerometer: m/s² (includes gravity)
/// - Gyroscope: rad/s
/// - Magnetometer: µT (microtesla)
///
/// Axes are in the device's **natural frame** — the pose the device ships in —
/// and are not remapped for the current screen rotation, so the same physical
/// tilt produces the same numbers here and on iOS. A landscape interaction that
/// means "toward the left edge" has to rotate the vector by the display
/// orientation itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct SensorData {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Atmospheric pressure reading.
#[derive(Debug, Clone, Copy, Default)]
pub struct BarometerData {
    /// Pressure in hectopascals (hPa).
    pub pressure: f64,
}

/// Which sensor types are available on this device.
#[derive(Debug, Clone, Copy, Default)]
pub struct SensorAvailability {
    pub accelerometer: bool,
    pub gyroscope: bool,
    pub magnetometer: bool,
    pub barometer: bool,
}

/// Check which sensors are available on this device.
pub fn available_sensors() -> SensorAvailability {
    #[cfg(target_os = "ios")]
    {
        ios::available_sensors()
    }
    #[cfg(target_os = "android")]
    {
        android::available_sensors()
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        SensorAvailability::default()
    }
}

/// Get the latest accelerometer reading (includes gravity).
pub fn accelerometer() -> Option<SensorData> {
    #[cfg(target_os = "ios")]
    {
        ios::accelerometer()
    }
    #[cfg(target_os = "android")]
    {
        android::accelerometer()
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        None
    }
}

/// Get the latest gyroscope reading.
pub fn gyroscope() -> Option<SensorData> {
    #[cfg(target_os = "ios")]
    {
        ios::gyroscope()
    }
    #[cfg(target_os = "android")]
    {
        android::gyroscope()
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        None
    }
}

/// Get the latest magnetometer reading.
pub fn magnetometer() -> Option<SensorData> {
    #[cfg(target_os = "ios")]
    {
        ios::magnetometer()
    }
    #[cfg(target_os = "android")]
    {
        android::magnetometer()
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        None
    }
}

/// Get the latest barometer (atmospheric pressure) reading.
pub fn barometer() -> Option<BarometerData> {
    #[cfg(target_os = "ios")]
    {
        ios::barometer()
    }
    #[cfg(target_os = "android")]
    {
        android::barometer()
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        None
    }
}

// ── sample cache shared by the platform layers ───────────────────────────────

/// Which polled sensor a cached reading belongs to.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SensorKind {
    Accelerometer,
    Gyroscope,
    Magnetometer,
    Barometer,
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
impl SensorKind {
    fn index(self) -> usize {
        match self {
            Self::Accelerometer => 0,
            Self::Gyroscope => 1,
            Self::Magnetometer => 2,
            Self::Barometer => 3,
        }
    }
}

/// Newest sample per sensor: written on whichever thread the platform delivers
/// samples on, read by the polling getters.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
static LATEST: [parking_lot::Mutex<Option<SensorData>>; 4] =
    [const { parking_lot::Mutex::new(None) }; 4];

/// Record `data` as the newest reading of `kind`.
///
/// Returns whether this is the first sample for that sensor since it was
/// subscribed to.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn store_latest(kind: SensorKind, data: SensorData) -> bool {
    let mut slot = LATEST[kind.index()].lock();
    let first = slot.is_none();
    *slot = Some(data);
    first
}

/// The newest reading of `kind`, or `None` if it has not sampled yet.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn latest(kind: SensorKind) -> Option<SensorData> {
    *LATEST[kind.index()].lock()
}

/// Deliver one sensor sample from the platform layer.
///
/// `kind` is the sensor id the platform subscribed with.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn notify_sample(kind: i32, x: f32, y: f32, z: f32) {
    #[cfg(target_os = "android")]
    {
        android::notify_sample(kind, x, y, z);
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (kind, x, y, z);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_of_a_sensor_is_reported_as_new() {
        let reading = SensorData {
            x: 0.1,
            y: 0.2,
            z: 9.8,
        };
        assert!(store_latest(SensorKind::Accelerometer, reading));
        assert!(!store_latest(SensorKind::Accelerometer, reading));
    }

    #[test]
    fn latest_returns_the_most_recent_sample() {
        store_latest(
            SensorKind::Gyroscope,
            SensorData {
                x: 1.0,
                y: 0.0,
                z: 0.0,
            },
        );
        store_latest(
            SensorKind::Gyroscope,
            SensorData {
                x: 2.5,
                y: 0.0,
                z: 0.0,
            },
        );
        assert_eq!(latest(SensorKind::Gyroscope).map(|d| d.x), Some(2.5));
    }

    #[test]
    fn a_sensor_that_has_not_sampled_yet_has_no_reading() {
        assert!(latest(SensorKind::Barometer).is_none());
    }
}
