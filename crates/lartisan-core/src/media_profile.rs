//! Per-disc track pitch, so media of different sensitivity give similar contrast.
//!
//! The tone curve (`raster::ToneCurve::best_pitch`) was measured on Memorex media (imaging
//! parameter 104) at a track pitch of 25 400 nm. HP's per-media table for this drive lists, for
//! every known imaging parameter, the "Best" resolution HP's own software uses to get the same
//! look from that dye (Memorex 1215 tracks/inch, Verbatim 1155, HP 810, ...). Scaling our
//! calibrated pitch by the ratio of those pitches puts a disc at roughly the same exposure as
//! the Memorex measurement:
//!
//! `pitch = 25 400 nm * 1215 / best_tpi(media)`
//!
//! Two media types were measured at their own pitch rather than taken from this rule: Verbatim
//! 122 at the scaled pitch (26.7 um), and HP 116 at HP's absolute Best pitch (31.4 um), which is
//! finer than the scaled 38.1 um. On another drive, every measured pitch is carried over by the
//! ratio of the rule's result on that drive to its result on the 1040r (see `profile`).
//!
//! For an imaging parameter that is not in the table, the Best pitch is estimated from the
//! disc's reported maximum write power at 250 mm/s. On the three discs read so far the table's
//! Best pitch rises by about 1.13 um per mW of that power (20.9 um at 29 mW, 22.0 um at 30 mW,
//! 31.4 um at 38 mW). That line is a fit to those three points, not a validated law: treat the
//! fallback as an estimate and read more discs to test it against the table.

use crate::media_id::MediaId;
use crate::raster::ToneCurve;

/// Pitch (nm) at which the tone curve was measured, on imaging parameter 104.
pub const CALIBRATED_PITCH_NM: u32 = 25_400;
/// HP's Best resolution for imaging parameter 104 (tracks per inch).
pub const CALIBRATED_BEST_TPI: u32 = 1215;
/// Finest track pitch the tool will use, automatic or by hand: the finest pitch burned so far.
pub const MIN_PITCH_NM: u32 = 25_400;
/// Coarsest pitch an automatic choice may give (a hand-set pitch may go up to the drive's Draft).
pub const MAX_AUTO_PITCH_NM: u32 = 40_000;

const TABLE: &str = include_str!("../data/drive_media_tpi.csv");
const POWER: &str = include_str!("../data/drive_power_conversion.csv");

/// The drive the tone curves and pitches were measured on (HP DVD Writer 1040r).
pub const CALIBRATED_DRIVE_ID: u16 = 169;

fn rows(csv: &'static str) -> impl Iterator<Item = Vec<i64>> {
    csv.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).map(|l| l.split(',').filter_map(|x| x.trim().parse().ok()).collect())
}

/// Write-power conversion for a DriveID: drive units = mW * slope + offset / 1000.
/// Returns (slope, offset_in_units), or None if the DriveID is not in HP's table of 123 drives.
pub fn power_conversion(drive_id: u16) -> Option<(f64, f64)> {
    rows(POWER).find(|r| r.len() == 4 && r[0] == drive_id as i64 && r[2] != 0).map(|r| (r[1] as f64 / r[2] as f64, r[3] as f64 / 1000.0))
}

/// HP's "Best" track resolution (tracks per inch) for an imaging parameter on a drive, if listed.
pub fn table_best_tpi(drive_id: u16, imaging: u16) -> Option<u32> {
    rows(TABLE).find(|r| r.len() == 6 && r[0] == drive_id as i64 && r[1] == imaging as i64).map(|r| r[4] as u32)
}

/// Number of imaging parameters HP lists for a drive.
pub fn table_len(drive_id: u16) -> usize {
    rows(TABLE).filter(|r| r.len() == 6 && r[0] == drive_id as i64).count()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PitchSource {
    /// The calibrated media itself: the pitch the tone curve was measured at.
    Calibrated,
    /// Scaled by HP's table entry for this imaging parameter.
    Table { best_tpi: u32 },
    /// Estimated from the disc's maximum power at 250 mm/s (imaging parameter not in the table).
    Formula { pmax_mw: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PitchChoice {
    pub pitch_nm: u32,
    pub source: PitchSource,
}

/// Estimated HP "Best" pitch in micrometres from the maximum write power at 250 mm/s.
pub fn formula_best_pitch_um(pmax_250_mw: u16) -> f64 {
    20.9 + 1.13 * (pmax_250_mw as f64 - 29.0)
}

/// Track pitch (nm) that puts this disc at about the same exposure as the Memorex calibration.
/// The ratio is taken within the drive's own table (Memorex 104 on that drive against this
/// media), so a drive whose table uses different absolute pitches still scales sensibly. The
/// absolute exposure on a drive other than the calibrated one is an estimate.
pub fn equal_exposure_pitch(drive_id: u16, id: &MediaId) -> PitchChoice {
    if id.imaging_parameter == 104 {
        return PitchChoice { pitch_nm: CALIBRATED_PITCH_NM, source: PitchSource::Calibrated };
    }
    let memorex_tpi = table_best_tpi(drive_id, 104).unwrap_or(CALIBRATED_BEST_TPI) as f64;
    let (pitch_nm, source) = match table_best_tpi(drive_id, id.imaging_parameter) {
        Some(tpi) => ((CALIBRATED_PITCH_NM as f64 * memorex_tpi / tpi as f64).round(), PitchSource::Table { best_tpi: tpi }),
        None => {
            let pmax = id.power_window_mw(250.0).1.round() as u16;
            let um = formula_best_pitch_um(pmax);
            ((CALIBRATED_PITCH_NM as f64 * um / (25_400_000.0 / CALIBRATED_BEST_TPI as f64 / 1000.0)).round(), PitchSource::Formula { pmax_mw: pmax })
        }
    };
    PitchChoice { pitch_nm: (pitch_nm as u32).clamp(MIN_PITCH_NM, MAX_AUTO_PITCH_NM), source }
}

/// Where a disc's tone curve comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneSource {
    /// Measured on this media type (a tone wedge burned and photographed).
    Measured,
    /// Estimated: the Memorex curve, whose shape may not match this dye.
    Estimated,
}

/// Pitch and tone curve to use for a disc.
#[derive(Debug, Clone)]
pub struct Profile {
    pub pitch: PitchChoice,
    pub tone: ToneCurve,
    pub tone_source: ToneSource,
}

/// HP's Best pitch for HP LightScribe media (imaging parameters 112, 114, 116, 130 share the
/// 810 tpi row of the table); the tone curve for 116 was measured at this pitch.
pub const HP_MEASURED_PITCH_NM: u32 = 31_400;

/// Imaging parameters whose tone curve and pitch were measured (116) or share its table row and
/// are assumed to behave the same (112, 114, 130).
pub fn is_hp_family(imaging: u16) -> bool {
    matches!(imaging, 112 | 114 | 116 | 130)
}

/// Verbatim LightScribe DVD-R (imaging parameter 122; 123 and 124 share its table row).
pub fn is_verbatim_family(imaging: u16) -> bool {
    matches!(imaging, 122 | 123 | 124)
}

/// The profile for a disc: measured values where we have them, scaled estimates elsewhere.
pub fn profile(drive_id: u16, id: &MediaId) -> Profile {
    let mut p = profile_on_calibrated_drive(id);
    if drive_id != CALIBRATED_DRIVE_ID {
        // Curves and the pitches they were measured at belong to the 1040r; elsewhere they are
        // estimates. The 1040r pitch is carried over by how the equal-exposure rule differs
        // between the two drives' tables, so a pitch measured off the rule (HP 116) keeps its
        // offset from it, and Memorex stays at the calibrated pitch.
        p.tone_source = ToneSource::Estimated;
        let (here, there) = (equal_exposure_pitch(drive_id, id), equal_exposure_pitch(CALIBRATED_DRIVE_ID, id));
        let nm = (p.pitch.pitch_nm as f64 * here.pitch_nm as f64 / there.pitch_nm as f64).round() as u32;
        p.pitch = PitchChoice { pitch_nm: nm.clamp(MIN_PITCH_NM, MAX_AUTO_PITCH_NM), source: here.source };
    }
    p
}

fn profile_on_calibrated_drive(id: &MediaId) -> Profile {
    let drive_id = CALIBRATED_DRIVE_ID;
    if is_verbatim_family(id.imaging_parameter) {
        // pitch from HP's table scaled from the Memorex calibration; the curve was measured at it
        let pitch = equal_exposure_pitch(drive_id, id);
        return Profile {
            pitch: PitchChoice { pitch_nm: pitch.pitch_nm, source: PitchSource::Calibrated },
            tone: ToneCurve::verbatim_122(),
            tone_source: if id.imaging_parameter == 122 { ToneSource::Measured } else { ToneSource::Estimated },
        };
    }
    if is_hp_family(id.imaging_parameter) {
        return Profile {
            pitch: PitchChoice { pitch_nm: HP_MEASURED_PITCH_NM, source: PitchSource::Calibrated },
            tone: ToneCurve::hp_116(),
            tone_source: if id.imaging_parameter == 116 { ToneSource::Measured } else { ToneSource::Estimated },
        };
    }
    Profile {
        pitch: equal_exposure_pitch(drive_id, id),
        tone: ToneCurve::best_pitch(),
        tone_source: if id.imaging_parameter == 104 { ToneSource::Measured } else { ToneSource::Estimated },
    }
}
