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
//!
//! Two ways to consume readings continuously: [`subscribe`] hands every sample
//! to a callback on the platform's own thread, and [`set_live_updates`] keeps the
//! polling getters live by asking for a redraw per sample. A screen that reads a
//! sensor while painting needs one or the other -- without them a new reading
//! only shows up when something else makes the window redraw.

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

// ── subscriptions and the shared sample cache ────────────────────────────────

/// Which sensor a polled reading or a subscription belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorKind {
    /// m/s², gravity included.
    Accelerometer,
    /// rad/s.
    Gyroscope,
    /// µT.
    Magnetometer,
    /// hPa, carried in [`SensorData::x`]; the other axes stay 0 because Android
    /// reports a single value for pressure.
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

/// A callback that receives every sample of the sensor it subscribed to.
pub type SensorCallback = Box<dyn Fn(SensorData) + Send + 'static>;

/// Newest sample per sensor: written on whichever thread the platform delivers
/// samples on, read by the polling getters.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
static LATEST: [parking_lot::Mutex<Option<SensorData>>; 4] =
    [const { parking_lot::Mutex::new(None) }; 4];

/// One handler per sensor, `None` while nobody has subscribed.
static HANDLERS: [parking_lot::Mutex<Option<SensorCallback>>; 4] =
    [const { parking_lot::Mutex::new(None) }; 4];

/// Deliver every sample of `kind` to `handler` until [`unsubscribe`] is called.
///
/// Subscribing also starts sampling, so a subscriber never has to poll. The
/// handler runs on the thread Android delivers the sample on -- its main (UI)
/// thread -- **not** on GPUI's render thread, so it must not touch window or
/// entity state; hand the reading to an executor the app captured instead. It
/// runs while this sensor's handler slot is locked, so it must not call
/// [`subscribe`] or [`unsubscribe`].
///
/// One handler per sensor: subscribing again replaces the previous one.
pub fn subscribe(
    kind: SensorKind,
    handler: impl Fn(SensorData) + Send + 'static,
) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        android::subscribe(kind, handler)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (kind, handler);
        Err("sensors::subscribe is only implemented on Android".to_string())
    }
}

/// Stop delivering samples of `kind` and stop sampling that sensor.
///
/// The polling getters keep whatever reading was last cached; a later read
/// subscribes the sensor again.
pub fn unsubscribe(kind: SensorKind) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        android::unsubscribe(kind)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = kind;
        Err("sensors::unsubscribe is only implemented on Android".to_string())
    }
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn set_handler(kind: SensorKind, handler: Option<SensorCallback>) {
    *HANDLERS[kind.index()].lock() = handler;
}

/// Whether every arriving sample should ask for a redraw.
static LIVE_UPDATES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Redraw as each new sample arrives so the polling getters stay live.
///
/// A screen that only reads [`accelerometer()`] and friends while painting
/// otherwise sees a new reading when something else happens to make it redraw.
/// Enabling this forces a frame per sample instead -- about 16 per second at the
/// reporting rate this package asks sensors for -- so leave it on only while a
/// screen really is following the sensor. Sensors nobody has read stay silent,
/// and a [`subscribe`]d sensor delivers to its handler rather than forcing
/// frames.
pub fn set_live_updates(enabled: bool) {
    LIVE_UPDATES.store(enabled, std::sync::atomic::Ordering::Release);
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn live_updates() -> bool {
    LIVE_UPDATES.load(std::sync::atomic::Ordering::Acquire)
}

/// Record `data` as the newest reading of `kind` and feed it to that sensor's
/// handler, if it has one.
///
/// Returns whether a frame must be forced so a *polling* reader notices the
/// reading: the sample that first fills an empty cache always does, and later
/// samples only while [`set_live_updates`] is on. A subscriber is being called
/// here already, so it needs no frame.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn dispatch_sample(kind: SensorKind, data: SensorData) -> bool {
    let first = store_latest(kind, data);
    match HANDLERS[kind.index()].lock().as_mut() {
        Some(callback) => {
            callback(data);
            false
        }
        None => first || live_updates(),
    }
}

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
    use std::sync::{Arc, Mutex};

    fn reading(x: f64, y: f64, z: f64) -> SensorData {
        SensorData { x, y, z }
    }

    // Each test owns one sensor: the cache is process-global, so sharing a kind
    // across tests running in parallel threads would make them interfere.

    #[test]
    fn the_cache_holds_the_most_recent_sample_and_flags_the_first() {
        assert!(latest(SensorKind::Accelerometer).is_none());
        assert!(store_latest(
            SensorKind::Accelerometer,
            reading(0.1, 0.2, 9.8)
        ));
        assert!(!store_latest(
            SensorKind::Accelerometer,
            reading(0.1, 0.2, 9.9)
        ));
        assert_eq!(latest(SensorKind::Accelerometer).map(|d| d.z), Some(9.9));
    }

    #[test]
    fn a_sample_reaches_the_handler_and_needs_no_forced_frame() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        set_handler(
            SensorKind::Gyroscope,
            Some(Box::new(move |data| {
                recorder.lock().unwrap().push(data);
            })),
        );

        assert!(!dispatch_sample(
            SensorKind::Gyroscope,
            reading(1.5, 0.0, 0.0)
        ));
        assert!(!dispatch_sample(
            SensorKind::Gyroscope,
            reading(2.5, 0.0, 0.0)
        ));

        set_handler(SensorKind::Gyroscope, None);
        let samples = seen.lock().unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].x, 2.5);
        // Polling still sees what the subscriber was handed.
        assert_eq!(latest(SensorKind::Gyroscope).map(|d| d.x), Some(2.5));
    }

    #[test]
    fn a_polling_reader_is_forced_to_a_frame_on_the_first_sample_and_while_live() {
        let kind = SensorKind::Magnetometer;
        assert!(dispatch_sample(kind, reading(12.0, 0.0, 0.0)));
        assert!(!dispatch_sample(kind, reading(13.0, 0.0, 0.0)));

        set_live_updates(true);
        assert!(dispatch_sample(kind, reading(14.0, 0.0, 0.0)));
        set_live_updates(false);
        assert!(!dispatch_sample(kind, reading(15.0, 0.0, 0.0)));
    }

    #[test]
    fn a_sensor_that_has_not_sampled_yet_has_no_reading() {
        assert!(latest(SensorKind::Barometer).is_none());
    }
}
