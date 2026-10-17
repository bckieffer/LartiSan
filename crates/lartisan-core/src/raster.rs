//! Polar rasterizer: turns an image function into per-track dot bitmaps using error diffusion
//! in polar space (7/16 along the track, 3/16, 5/16, 1/16 to the next track).

use crate::geometry::Geometry;
use crate::track::{Bits, Track};

/// Darkness (0 = leave unmarked, 1 = burn) at a disc position in millimetres, as seen from the
/// label side: x to the right, y up, origin at the disc centre.
pub trait Sampler {
    fn darkness(&self, x_mm: f64, y_mm: f64) -> f64;
}

impl<F: Fn(f64, f64) -> f64> Sampler for F {
    fn darkness(&self, x: f64, y: f64) -> f64 {
        self(x, y)
    }
}

/// Maps a wanted printed darkness t in 0..=1 to the dot density that produces it.
/// `response[k]` is the measured relative darkness (linear light, normalised to the 100% patch)
/// at density `k / (response.len() - 1)`.
#[derive(Debug, Clone)]
pub struct ToneCurve {
    pub response: Vec<f64>,
}

impl ToneCurve {
    /// Measured on the HP 1040r with Memorex media at Best pitch (25 400 nm), drive defaults:
    /// 21 density steps of 5%, from a close-up photo with a bare-label reference.
    /// Nothing prints below about 8% density; the response is convex above that.
    pub fn best_pitch() -> Self {
        Self { response: vec![0.0, 0.0, 0.054, 0.099, 0.169, 0.217, 0.265, 0.33, 0.344, 0.385, 0.417, 0.489, 0.531, 0.613, 0.68, 0.777, 0.797, 0.82, 0.898, 1.0, 1.0] }
    }

    /// Measured on an HP LightScribe DVD-R (imaging parameter 116) with the drive defaults at a
    /// track pitch of 31 400 nm (HP's own Best pitch for it). Derived from two rounds of a 10-step
    /// strip of 2.4 mm-wide patches, photographed with a ruler: a first wedge of 0.5 mm bands
    /// overstated the mid-tones (this dye's pale coating scatters light sideways by a few tenths
    /// of a millimetre), the second round showed the corrected curve was a little too gentle at
    /// both ends, and this is the average of those two (a damped update). Accuracy about
    /// +-0.08 in relative darkness; fine detail in an image still bleeds a little.
    pub fn hp_116() -> Self {
        Self { response: vec![0.0, 0.025, 0.049, 0.093, 0.142, 0.218, 0.296, 0.373, 0.436, 0.491, 0.541, 0.593, 0.648, 0.703, 0.758, 0.812, 0.871, 0.934, 0.978, 1.0, 1.0] }
    }

    /// Measured on a Verbatim LightScribe DVD-R (imaging parameter 122) with the drive defaults at
    /// a track pitch of 26 719 nm (the pitch scaled from HP's table): 21 density steps of 5%, from
    /// a close-up photo of a tone wedge. Very close to the Memorex shape (convex, half the darkness
    /// at about 57% density); maximum darkness about 10% higher.
    pub fn verbatim_122() -> Self {
        Self { response: vec![0.0, 0.033, 0.094, 0.167, 0.212, 0.252, 0.31, 0.345, 0.381, 0.401, 0.443, 0.49, 0.557, 0.607, 0.699, 0.769, 0.839, 0.886, 0.952, 1.0, 1.0] }
    }

    /// Dot density (0..=1) needed for printed darkness `t` (0..=1), by inverting the response.
    pub fn density_for(&self, t: f64) -> f64 {
        let r = &self.response;
        let n = r.len() - 1;
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 {
            return 0.0;
        }
        for k in 1..=n {
            if r[k] >= t {
                let (lo, hi) = (r[k - 1], r[k]);
                let f = if hi > lo { (t - lo) / (hi - lo) } else { 1.0 };
                return ((k - 1) as f64 + f) / n as f64;
            }
        }
        1.0
    }
}

/// A grayscale image placed on the disc, `size_mm` wide on its longer side, centred.
pub struct GrayImage {
    pub w: usize,
    pub h: usize,
    /// 0..=1 darkness, row 0 = top.
    pub dark: Vec<f32>,
    pub size_mm: f64,
    pub gamma: f64,
    pub tone: Option<ToneCurve>,
    /// Where the image centre sits on the disc (mm, x right, y up as seen from the label side).
    pub offset_mm: (f64, f64),
    /// Rotation of the image about its centre, degrees clockwise as seen from the label side.
    pub rotation_deg: f64,
}

impl GrayImage {
    pub fn from_gray8(w: usize, h: usize, gray: &[u8], size_mm: f64, gamma: f64) -> Self {
        let dark = gray.iter().map(|&g| 1.0 - g as f32 / 255.0).collect();
        Self { w, h, dark, size_mm, gamma, tone: None, offset_mm: (0.0, 0.0), rotation_deg: 0.0 }
    }

    pub fn with_offset(mut self, x_mm: f64, y_mm: f64) -> Self {
        self.offset_mm = (x_mm, y_mm);
        self
    }

    pub fn with_rotation(mut self, deg: f64) -> Self {
        self.rotation_deg = deg;
        self
    }

    /// The image's rectangle on the disc as (half width, half height) in mm.
    pub fn half_extent_mm(&self) -> (f64, f64) {
        let dim = self.w.max(self.h) as f64;
        (self.w as f64 / dim * self.size_mm / 2.0, self.h as f64 / dim * self.size_mm / 2.0)
    }

    /// Smallest and largest radius (mm) the image rectangle touches.
    pub fn radius_range_mm(&self) -> (f64, f64) {
        let (hw, hh) = self.half_extent_mm();
        let (mut lo, mut hi) = (f64::MAX, 0.0f64);
        let (nx, ny) = (60, 60);
        for i in 0..=nx {
            for j in 0..=ny {
                let (vx, vy) = (-hw + 2.0 * hw * i as f64 / nx as f64, -hh + 2.0 * hh * j as f64 / ny as f64);
                let t = self.rotation_deg.to_radians();
                let x = self.offset_mm.0 + vx * t.cos() + vy * t.sin();
                let y = self.offset_mm.1 - vx * t.sin() + vy * t.cos();
                let r = x.hypot(y);
                lo = lo.min(r);
                hi = hi.max(r);
            }
        }
        (lo, hi)
    }

    /// Treat the pixel values as sRGB-encoded (the usual case) and compensate for the measured
    /// tone response of the label: wanted darkness = 1 - linear luminance, then invert the curve.
    pub fn with_tone(mut self, tone: ToneCurve, gray: &[u8]) -> Self {
        self.dark = gray.iter().map(|&g| 1.0 - (g as f32 / 255.0).powf(2.2)).collect();
        self.tone = Some(tone);
        self
    }
}

impl Sampler for GrayImage {
    fn darkness(&self, x: f64, y: f64) -> f64 {
        let (qx, qy) = (x - self.offset_mm.0, y - self.offset_mm.1);
        let t = self.rotation_deg.to_radians();
        let (x, y) = (qx * t.cos() - qy * t.sin(), qx * t.sin() + qy * t.cos());
        let dim = self.w.max(self.h) as f64;
        let u = x / self.size_mm * dim + self.w as f64 / 2.0 - 0.5;
        let v = self.h as f64 / 2.0 - y / self.size_mm * dim - 0.5;
        if u < -0.5 || v < -0.5 || u > self.w as f64 - 0.5 || v > self.h as f64 - 0.5 {
            return 0.0;
        }
        let (u, v) = (u.max(0.0), v.max(0.0));
        let x0 = (u as usize).min(self.w.saturating_sub(2));
        let y0 = (v as usize).min(self.h.saturating_sub(2));
        let (fx, fy) = (u - x0 as f64, v - y0 as f64);
        let dx = (self.w > 1) as usize;
        let dy = (self.h > 1) as usize;
        let p = |xx: usize, yy: usize| self.dark[yy * self.w + xx] as f64;
        let val = (p(x0, y0) * (1.0 - fx) + p(x0 + dx, y0) * fx) * (1.0 - fy)
            + (p(x0, y0 + dy) * (1.0 - fx) + p(x0 + dx, y0 + dy) * fx) * fy;
        let val = if self.gamma == 1.0 { val } else { val.powf(self.gamma) };
        match &self.tone {
            Some(t) => t.density_for(val),
            None => val,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RasterOptions {
    /// Only tracks whose radius is within [r_min_um, r_max_um] are considered.
    pub r_min_um: f64,
    pub r_max_um: f64,
}

/// Rasterize `sampler` into tracks (blank tracks are skipped, as the original software does).
pub fn rasterize(geom: &Geometry, sampler: &dyn Sampler, opts: &RasterOptions) -> Vec<Track> {
    let mut tracks = Vec::new();
    let mut carry: Vec<f32> = Vec::new();
    for n in 0..4000u32 {
        let r_um = geom.radius_um(n);
        if r_um > opts.r_max_um || !geom.in_band(n) {
            if r_um > opts.r_max_um {
                break;
            }
            continue;
        }
        let nd = geom.dots(n) as usize;
        let mut err = vec![0.0f32; nd + 3];
        for (j, e) in err.iter_mut().enumerate().take(nd) {
            if let Some(c) = carry.get(j) {
                *e += *c;
            }
        }
        let next_n = geom.dots(n + 1) as usize;
        let mut next = vec![0.0f32; next_n + 3];
        let mut bits = Bits::new(nd as u32);
        let in_range = r_um >= opts.r_min_um;
        for i in 0..nd {
            let (x, y) = if in_range {
                let ph = Geometry::dot_angle_rad(i as u32, nd as u32, 0);
                (r_um / 1000.0 * ph.sin(), r_um / 1000.0 * ph.cos())
            } else {
                (0.0, 0.0)
            };
            let d = if in_range { sampler.darkness(x, y) } else { 0.0 };
            let v = d + err[i] as f64;
            let on = v >= 0.5;
            let e = (v - if on { 1.0 } else { 0.0 }) as f32;
            if on {
                bits.set(i as u32);
            }
            err[i + 1] += e * 7.0 / 16.0;
            let j = (i as f64 * next_n as f64 / nd as f64) as usize;
            if j > 0 {
                next[j - 1] += e * 3.0 / 16.0;
            }
            next[j] += e * 5.0 / 16.0;
            next[j + 1] += e * 1.0 / 16.0;
        }
        carry = next;
        if bits.count() > 0 {
            tracks.push(Track { n, shift: 0, bits });
        }
    }
    tracks
}

// ---- test patterns ----

/// Asymmetric "F": 7 mm wide, 10 mm tall, centred at (cx, cy) mm. Upright as seen from the label side.
pub fn f_glyph(cx: f64, cy: f64) -> impl Sampler {
    move |x: f64, y: f64| {
        let (u, v) = (x - cx + 3.5, y - cy + 5.0);
        if !(0.0..=7.0).contains(&u) || !(0.0..=10.0).contains(&v) {
            return 0.0;
        }
        if u <= 2.0 || v >= 8.0 || ((4.0..=6.0).contains(&v) && u <= 5.0) { 1.0 } else { 0.0 }
    }
}

/// Tone wedge: 11 radial bands of 1 mm from `r0_mm`, darkness k/10, within a clockwise angle sector.
pub fn tone_wedge(r0_mm: f64, a0_deg: f64, a1_deg: f64) -> impl Sampler {
    tone_wedge_n(r0_mm, 1.0, 11, a0_deg, a1_deg)
}

/// Tone wedge with `levels` radial bands of `band_mm` each from `r0_mm`; band k has darkness
/// k / (levels - 1). Only inside the clockwise angle sector [a0_deg, a1_deg].
pub fn tone_wedge_n(r0_mm: f64, band_mm: f64, levels: u32, a0_deg: f64, a1_deg: f64) -> impl Sampler {
    move |x: f64, y: f64| {
        let r = x.hypot(y);
        let mut a = x.atan2(y).to_degrees();
        if a < 0.0 {
            a += 360.0;
        }
        if r < r0_mm || r >= r0_mm + band_mm * levels as f64 || a < a0_deg || a > a1_deg {
            return 0.0;
        }
        ((r - r0_mm) / band_mm).floor() / (levels - 1) as f64
    }
}

/// Achieved dot density per wedge band (percent), measured from the rasterized tracks inside the
/// sector, ignoring the outer 5% of each band so neighbouring bands do not bleed in.
pub fn wedge_achieved(tracks: &[Track], geom: &Geometry, r0_mm: f64, band_mm: f64, levels: u32, a0_deg: f64, a1_deg: f64) -> Vec<f64> {
    (0..levels)
        .map(|k| {
            let (lo, hi) = (r0_mm + band_mm * (k as f64 + 0.05), r0_mm + band_mm * (k as f64 + 0.95));
            let (mut on, mut tot) = (0u64, 0u64);
            for t in tracks {
                let r = geom.radius_um(t.n) / 1000.0;
                if r < lo || r >= hi {
                    continue;
                }
                for i in 0..t.bits.dots {
                    let a = (crate::geometry::ORIGIN_DEG + 360.0 * i as f64 / t.bits.dots as f64).rem_euclid(360.0);
                    if a >= a0_deg && a <= a1_deg {
                        tot += 1;
                        on += t.bits.get(i) as u64;
                    }
                }
            }
            if tot == 0 { 0.0 } else { 100.0 * on as f64 / tot as f64 }
        })
        .collect()
}

/// A solid band: darkness 1 between `r0_mm` and `r0_mm + width_mm`, inside a clockwise angle sector.
pub fn solid_band(r0_mm: f64, width_mm: f64, a0_deg: f64, a1_deg: f64) -> impl Sampler {
    move |x: f64, y: f64| {
        let r = x.hypot(y);
        let mut a = x.atan2(y).to_degrees();
        if a < 0.0 {
            a += 360.0;
        }
        if r >= r0_mm && r < r0_mm + width_mm && a >= a0_deg && a <= a1_deg { 1.0 } else { 0.0 }
    }
}

/// A full ring at one track: every dot of track `n` marked (the first test burn).
pub fn full_ring(geom: &Geometry, n: u32) -> Track {
    let dots = geom.dots(n);
    let mut bits = Bits::new(dots);
    for i in 0..dots {
        bits.set(i);
    }
    Track { n, shift: 0, bits }
}
