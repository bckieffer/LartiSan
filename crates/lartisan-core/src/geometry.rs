//! Disc geometry: track number <-> radius, dots per track, and the calibrated angle mapping.

use std::f64::consts::PI;

/// Calibrated: angle of dot 0 (track shift 0), degrees clockwise from the image's "up", as seen
/// from the label side. The same at every radius; tied to the disc. ±2°.
pub const ORIGIN_DEG: f64 = -17.0;

/// Spokes per revolution (page 0x32); the header's track shift is in spokes.
pub const SPOKES: f64 = 400.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub start_um: f64,
    pub mark_pitch_nm: f64,
    pub track_pitch_nm: f64,
    pub drive_inner_um: f64,
    pub drive_outer_um: f64,
}

impl Geometry {
    /// radius (um) = start radius + track number * track pitch (nm) / 1000
    pub fn radius_um(&self, n: u32) -> f64 {
        self.start_um + n as f64 * self.track_pitch_nm / 1000.0
    }

    /// dots per revolution = floor(2*pi*r / mark pitch) + 1
    pub fn dots(&self, n: u32) -> u32 {
        (2.0 * PI * self.radius_um(n) / (self.mark_pitch_nm / 1000.0)).floor() as u32 + 1
    }

    /// Nearest track for a radius in micrometres (may be negative if inside the start radius).
    pub fn track_for_radius_um(&self, r_um: f64) -> i64 {
        ((r_um - self.start_um) / (self.track_pitch_nm / 1000.0)).round() as i64
    }

    /// Is track `n` inside the drive's usable band?
    pub fn in_band(&self, n: u32) -> bool {
        let r = self.radius_um(n);
        r >= self.start_um && r >= self.drive_inner_um && r <= self.drive_outer_um
    }

    /// Image-space angle (radians, clockwise from up) of dot `i` of `n_dots` with `shift` spokes.
    pub fn dot_angle_rad(i: u32, n_dots: u32, shift_spokes: u32) -> f64 {
        (ORIGIN_DEG + 360.0 * i as f64 / n_dots as f64 + shift_spokes as f64 * 360.0 / SPOKES).to_radians()
    }

    /// Seconds one revolution takes at linear velocity `spindle_um_s` at track `n` (one track per rev).
    pub fn rev_seconds(&self, n: u32, spindle_um_s: f64) -> f64 {
        2.0 * PI * self.radius_um(n) / spindle_um_s
    }
}
