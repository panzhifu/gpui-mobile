//! HarmonyOS display handling.
//!
//! OpenHarmony does not expose a rich display API through the native NDK at
//! the C level.  Display geometry is derived from the `XComponent` surface
//! dimensions (delivered via `OnSurfaceChanged`) and the density reported by
//! the ArkTS `@ohos.display` module (pushed in through `napi.rs`).
//!
//! `OhosDisplay` models a single logical screen.  On phones / tablets this is
//! the only display.

use super::{Bounds, DevicePixels, Pixels, Point, Size};
use anyhow::Result;
use gpui::{self, DisplayId, PlatformDisplay};
use std::fmt;

/// The OpenHarmony baseline density (in dpi) that `1.0` logical px maps to.
const DENSITY_DEFAULT: i32 = 160;

// ── OhosDisplay ───────────────────────────────────────────────────────────────

/// A single logical display on HarmonyOS.
///
/// Holds the physical size of the render surface together with the display
/// density, and exposes the geometry GPUI needs to lay out and render content.
#[derive(Clone)]
pub struct OhosDisplay {
    /// Physical width in device pixels.
    width: i32,
    /// Physical height in device pixels.
    height: i32,
    /// Display density as a scale factor relative to 160 dpi.
    scale_factor: f32,
    /// A stable numeric identifier for this display.
    id: u64,
}

impl OhosDisplay {
    // ── constructors ─────────────────────────────────────────────────────────

    /// Create an `OhosDisplay` with explicit geometry.
    ///
    /// `density_dpi` — the screen density in dots-per-inch as reported by
    /// `@ohos.display`.  Pass `0` or a negative value to fall back to the
    /// baseline density (160 dpi → scale factor 1.0).
    pub fn new(width: i32, height: i32, density_dpi: i32) -> Self {
        let density = if density_dpi > 0 {
            density_dpi
        } else {
            DENSITY_DEFAULT
        };
        let scale_factor = density as f32 / DENSITY_DEFAULT as f32;

        Self {
            width,
            height,
            scale_factor,
            id: ((width as u64) << 32) | (height as u64),
        }
    }

    /// Create a synthetic `OhosDisplay` for headless / testing use.
    ///
    /// Returns a display with a 1× scale factor.
    pub fn headless(width: i32, height: i32) -> Self {
        Self::new(width, height, DENSITY_DEFAULT)
    }

    // ── geometry ──────────────────────────────────────────────────────────────

    /// Physical width of the display in device pixels.
    pub fn physical_width(&self) -> DevicePixels {
        DevicePixels(self.width)
    }

    /// Physical height of the display in device pixels.
    pub fn physical_height(&self) -> DevicePixels {
        DevicePixels(self.height)
    }

    /// Physical size of the display in device pixels.
    pub fn physical_size(&self) -> Size<DevicePixels> {
        Size {
            width: self.physical_width(),
            height: self.physical_height(),
        }
    }

    /// Logical width of the display in density-independent pixels.
    pub fn logical_width(&self) -> Pixels {
        Pixels(self.width as f32 / self.scale_factor)
    }

    /// Logical height of the display in density-independent pixels.
    pub fn logical_height(&self) -> Pixels {
        Pixels(self.height as f32 / self.scale_factor)
    }

    /// Logical size of the display in density-independent pixels.
    pub fn logical_size(&self) -> Size<Pixels> {
        Size {
            width: self.logical_width(),
            height: self.logical_height(),
        }
    }

    /// The full display bounds with origin at `(0, 0)`.
    pub fn bounds(&self) -> Bounds<DevicePixels> {
        Bounds {
            origin: Point {
                x: DevicePixels(0),
                y: DevicePixels(0),
            },
            size: self.physical_size(),
        }
    }

    /// The full display bounds in logical pixels with origin at `(0, 0)`.
    pub fn logical_bounds(&self) -> Bounds<Pixels> {
        Bounds {
            origin: Point {
                x: Pixels(0.0),
                y: Pixels(0.0),
            },
            size: self.logical_size(),
        }
    }

    // ── density / scale ───────────────────────────────────────────────────────

    /// Scale factor relative to 160 dpi.
    ///
    /// `device_px = logical_px * scale_factor`.
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Approximate screen density in dots-per-inch.
    pub fn dpi(&self) -> i32 {
        (self.scale_factor * DENSITY_DEFAULT as f32).round() as i32
    }

    // ── identity ──────────────────────────────────────────────────────────────

    /// A numeric identifier for this display.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Returns `true` if this display has a non-zero surface size.
    pub fn is_real(&self) -> bool {
        self.width > 0 && self.height > 0
    }
}

impl fmt::Debug for OhosDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OhosDisplay")
            .field("id", &format_args!("{:#x}", self.id))
            .field("physical_size", &self.physical_size())
            .field("scale_factor", &self.scale_factor)
            .field("dpi", &self.dpi())
            .finish()
    }
}

// ── impl PlatformDisplay ──────────────────────────────────────────────────────

impl PlatformDisplay for OhosDisplay {
    fn id(&self) -> DisplayId {
        // gpui-pre 0.3.9 的 `DisplayId::new` 收 u64（与 Android 臂同一条）。
        DisplayId::new(self.id as u64)
    }

    fn refresh_interval(&self) -> Option<std::time::Duration> {
        // `OhosDisplay` 手上只有那块 XComponent 的尺寸，没有 `OH_NativeVSync` 的句柄
        // —— 刷新周期在 vsync 那一格里，不在显示器这一格。报 `None` 让 GPUI 用它
        // 自己的时钟，与 Android 臂同一条（不报假数）。
        None
    }

    fn uuid(&self) -> Result<uuid::Uuid> {
        // The OHOS NDK does not provide a stable UUID for displays.
        // Synthesise a deterministic v5 UUID from the display id so that the
        // same surface always produces the same UUID within a process lifetime.
        let namespace = uuid::Uuid::NAMESPACE_OID;
        let name = format!("ohos-display-{:#x}", self.id);
        Ok(uuid::Uuid::new_v5(&namespace, name.as_bytes()))
    }

    fn bounds(&self) -> gpui::Bounds<gpui::Pixels> {
        let logical = self.logical_size();
        gpui::Bounds {
            origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
            size: gpui::size(gpui::px(logical.width.0), gpui::px(logical.height.0)),
        }
    }
}

impl fmt::Display for OhosDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ps = self.physical_size();
        write!(
            f,
            "OhosDisplay({}×{}px, {:.1}×, {}dpi)",
            ps.width.0,
            ps.height.0,
            self.scale_factor,
            self.dpi(),
        )
    }
}

// ── DisplayList helper ────────────────────────────────────────────────────────

/// A list of currently connected displays.
pub struct DisplayList {
    displays: Vec<OhosDisplay>,
}

impl DisplayList {
    /// Constructs a list containing a single primary display.
    pub fn single(display: OhosDisplay) -> Self {
        Self {
            displays: vec![display],
        }
    }

    /// Returns the primary (first) display.
    pub fn primary(&self) -> Option<&OhosDisplay> {
        self.displays.first()
    }

    /// Returns all displays.
    pub fn all(&self) -> &[OhosDisplay] {
        &self.displays
    }

    /// Returns the number of connected displays.
    pub fn len(&self) -> usize {
        self.displays.len()
    }

    /// Returns `true` if the list is empty.
    pub fn is_empty(&self) -> bool {
        self.displays.is_empty()
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_display_geometry() {
        let d = OhosDisplay::headless(1920, 1080);
        assert_eq!(d.physical_width(), DevicePixels(1920));
        assert_eq!(d.physical_height(), DevicePixels(1080));
        assert!(d.is_real());
    }

    #[test]
    fn headless_scale_factor_is_one() {
        let d = OhosDisplay::headless(800, 600);
        assert!((d.scale_factor() - 1.0).abs() < f32::EPSILON);
        assert_eq!(d.dpi(), DENSITY_DEFAULT);
    }

    #[test]
    fn density_scales() {
        let d = OhosDisplay::new(1080, 2400, 480);
        assert!((d.scale_factor() - 3.0).abs() < f32::EPSILON);
        assert_eq!(d.dpi(), 480);
        assert!((d.logical_width().0 - 360.0).abs() < f32::EPSILON);
    }

    #[test]
    fn display_list_primary() {
        let list = DisplayList::single(OhosDisplay::headless(1080, 1920));
        assert!(!list.is_empty());
        assert_eq!(list.len(), 1);
        assert_eq!(list.primary().unwrap().physical_width(), DevicePixels(1080));
    }

    #[test]
    fn debug_format_contains_dimensions() {
        let s = format!("{:?}", OhosDisplay::headless(1080, 1920));
        assert!(s.contains("1080"));
        assert!(s.contains("1920"));
    }
}
