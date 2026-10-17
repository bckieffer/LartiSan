//! Per-disc pitch scaling, checked against the media IDs read from three real discs.

use lartisan_core::media_id::*;
use lartisan_core::media_profile::*;

const MEMOREX: [u8; 15] = [0x07, 0x1d, 0x6b, 0x61, 0x1d, 0x1b, 0x65, 0x27, 0x25, 0x0f, 0x0f, 0x66, 0x2a, 0x07, 0x27];
const VERBATIM: [u8; 15] = [0x36, 0x1F, 0x70, 0x63, 0x6D, 0x1D, 0x65, 0x27, 0x26, 0x0D, 0x0F, 0x99, 0x39, 0x4F, 0x2F];
const HP: [u8; 15] = [0x5D, 0x1E, 0x9B, 0x74, 0x1C, 0x19, 0x65, 0x27, 0x26, 0x15, 0x0F, 0x0F, 0x8D, 0x54, 0x9C];

#[test]
fn the_other_two_discs_decode_cleanly() {
    let v = decode_outer_id(&VERBATIM).unwrap();
    assert_eq!((v.erasures, v.corrected), (0, 0));
    assert_eq!((v.id.imaging_parameter, v.id.manufacturer), (122, 1));
    assert_eq!((v.id.low_speed_min_power_mw, v.id.low_speed_max_power_mw, v.id.high_speed_min_power_mw, v.id.high_speed_max_power_mw), (24, 30, 37, 44));
    let h = decode_outer_id(&HP).unwrap();
    assert_eq!((h.erasures, h.corrected), (0, 0));
    assert_eq!((h.id.imaging_parameter, h.id.manufacturer), (116, 3));
    assert_eq!((h.id.low_speed_min_power_mw, h.id.low_speed_max_power_mw, h.id.high_speed_min_power_mw, h.id.high_speed_max_power_mw), (22, 38, 28, 52));
    // the drive's 28.7 mW is inside every disc's window at 250 mm/s
    for id in [&decode_outer_id(&MEMOREX).unwrap().id, &v.id, &h.id] {
        let (lo, hi) = id.power_window_mw(250.0);
        assert!(lo <= 28.67 && 28.67 <= hi, "{lo}-{hi}");
    }
}

#[test]
fn table_lookup() {
    assert_eq!(table_len(169), 19);
    assert_eq!(table_best_tpi(169, 104), Some(1215));
    assert_eq!(table_best_tpi(169, 122), Some(1155));
    assert_eq!(table_best_tpi(169, 116), Some(810));
    assert_eq!(table_best_tpi(169, 101), None, "HP's generic fallback row is not a media type");
    assert_eq!(table_best_tpi(169, 999), None);
}

#[test]
fn pitch_per_disc() {
    let m = equal_exposure_pitch(169, &decode_outer_id(&MEMOREX).unwrap().id);
    assert_eq!((m.pitch_nm, m.source), (25_400, PitchSource::Calibrated));
    let v = equal_exposure_pitch(169, &decode_outer_id(&VERBATIM).unwrap().id);
    assert_eq!((v.pitch_nm, v.source), (26_719, PitchSource::Table { best_tpi: 1155 }));
    let h = equal_exposure_pitch(169, &decode_outer_id(&HP).unwrap().id);
    assert_eq!((h.pitch_nm, h.source), (38_100, PitchSource::Table { best_tpi: 810 }));
}

#[test]
fn fallback_formula_for_unlisted_media() {
    let mut id = decode_outer_id(&VERBATIM).unwrap().id;
    id.imaging_parameter = 200; // not in HP's table
    let c = equal_exposure_pitch(169, &id);
    assert_eq!(c.source, PitchSource::Formula { pmax_mw: 30 });
    assert!((26_000..=27_500).contains(&c.pitch_nm), "{}", c.pitch_nm);
    // the formula reproduces the table on the three discs read (it was fitted to them)
    for (pmax, table_um) in [(29u16, 20.9), (30, 22.0), (38, 31.4)] {
        let est = formula_best_pitch_um(pmax);
        assert!((est - table_um).abs() / table_um < 0.03, "{pmax} mW: {est} vs {table_um}");
    }
    // extreme reports are clamped to a sane pitch range
    id.low_speed_max_power_mw = 70;
    assert_eq!(equal_exposure_pitch(169, &id).pitch_nm, 40_000);
    id.low_speed_max_power_mw = 12;
    assert_eq!(equal_exposure_pitch(169, &id).pitch_nm, MIN_PITCH_NM, "never finer than the finest pitch burned");
}


#[test]
fn profiles_use_measured_values_where_they_exist() {
    let m = profile(169, &decode_outer_id(&MEMOREX).unwrap().id);
    assert_eq!((m.pitch.pitch_nm, m.tone_source), (25_400, ToneSource::Measured));
    let h = profile(169, &decode_outer_id(&HP).unwrap().id);
    assert_eq!((h.pitch.pitch_nm, h.pitch.source, h.tone_source), (31_400, PitchSource::Calibrated, ToneSource::Measured));
    let v = profile(169, &decode_outer_id(&VERBATIM).unwrap().id);
    assert_eq!((v.pitch.pitch_nm, v.pitch.source, v.tone_source), (26_719, PitchSource::Calibrated, ToneSource::Measured));
    // both dyes are convex for wide features: half the maximum darkness needs more than half... but
    // the HP dye needs fewer dots than the Memorex for the same darkness
    assert!(m.tone.density_for(0.5) > h.tone.density_for(0.5));
    assert!((0.35..0.50).contains(&h.tone.density_for(0.5)), "{}", h.tone.density_for(0.5));
    assert!(h.tone.response.windows(2).all(|w| w[1] >= w[0]));
    // other imaging parameters in HP's 810 tpi row are assumed to behave like 116, but only 116 is "measured"
    let mut id = decode_outer_id(&HP).unwrap().id;
    id.imaging_parameter = 112;
    assert_eq!(profile(169, &id).tone_source, ToneSource::Estimated);
}


#[test]
fn verbatim_curve_is_close_to_memorex_but_its_own() {
    let v = lartisan_core::raster::ToneCurve::verbatim_122();
    let m = lartisan_core::raster::ToneCurve::best_pitch();
    assert!(v.response.windows(2).all(|w| w[1] >= w[0]));
    for (a, b) in v.response.iter().zip(&m.response) {
        assert!((a - b).abs() < 0.07, "{a} vs {b}");
    }
    assert!(v.response != m.response);
    assert!((0.50..0.65).contains(&v.density_for(0.5)));
}


#[test]
fn other_drives_are_known_but_their_numbers_are_estimates() {
    // HP's table lists 123 drives, each with a write-power conversion and per-media resolutions
    assert_eq!(power_conversion(169), Some((1.744, 0.0)));
    assert!(power_conversion(104).is_some() && power_conversion(104) != power_conversion(169));
    assert_eq!(power_conversion(9999), None);
    assert!(table_len(104) > 10 && table_len(169) == 19);
    assert_ne!(table_best_tpi(104, 104), None);
    // on a drive other than the calibrated one the measured curves are only estimates
    let id = decode_outer_id(&MEMOREX).unwrap().id;
    assert_eq!(profile(169, &id).tone_source, ToneSource::Measured);
    let other = profile(104, &id);
    assert_eq!(other.tone_source, ToneSource::Estimated);
    assert_eq!(other.pitch.pitch_nm, 25_400, "Memorex stays at the calibrated pitch by definition");
    // the pitch ratio is taken inside the other drive's own table
    let v = decode_outer_id(&VERBATIM).unwrap().id;
    let on_other = equal_exposure_pitch(104, &v);
    let (m, x) = (table_best_tpi(104, 104).unwrap() as f64, table_best_tpi(104, 122));
    if let Some(x) = x {
        assert_eq!(on_other.pitch_nm, ((25_400.0 * m / x as f64).round() as u32).clamp(MIN_PITCH_NM, MAX_AUTO_PITCH_NM));
    }
}

#[test]
fn measured_pitches_carry_over_to_other_drives_by_the_same_rule() {
    // HP 116 was measured at 31.4 um, finer than the rule's 38.1 um on the 1040r; on another drive
    // it keeps that ratio to the rule instead of jumping to the rule's value
    let h = decode_outer_id(&HP).unwrap().id;
    let (here, there) = (equal_exposure_pitch(104, &h).pitch_nm as f64, equal_exposure_pitch(169, &h).pitch_nm as f64);
    let want = ((31_400.0 * here / there).round() as u32).clamp(MIN_PITCH_NM, MAX_AUTO_PITCH_NM);
    let p = profile(104, &h);
    assert_eq!(p.pitch.pitch_nm, want);
    assert_eq!(p.tone_source, ToneSource::Estimated);
    // Verbatim was measured at the rule's pitch, so on another drive it gets that drive's rule
    let v = decode_outer_id(&VERBATIM).unwrap().id;
    assert_eq!(profile(104, &v).pitch, equal_exposure_pitch(104, &v));
    // every automatic pitch stays inside the bounds a hand-set pitch must respect
    for id in [&h, &v] {
        for drive in [104u16, 169] {
            assert!((MIN_PITCH_NM..=MAX_AUTO_PITCH_NM).contains(&profile(drive, id).pitch.pitch_nm));
        }
    }
}
