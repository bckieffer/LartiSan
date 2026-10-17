//! Golden tests against real captures from the HP 1040r (testdata/) and the burns done with the
//! C prototype (lsburn, not part of this repository): the Rust core must reproduce those bytes exactly.

use lartisan_core::cdb;
use lartisan_core::geometry::Geometry;
use lartisan_core::pages::*;
use lartisan_core::plan::{burn_steps, Dir};
use lartisan_core::raster::{self, RasterOptions};
use lartisan_core::track;

fn td(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../testdata/{}", env!("CARGO_MANIFEST_DIR"), name)).unwrap()
}

fn caps_and_geom() -> (DriveCaps, Geometry, Vec<u8>) {
    let ms = td("mode_sense_all_current.bin");
    let page = find_page(&ms, 0x31).unwrap().to_vec();
    let caps = DriveCaps::parse(&page).unwrap();
    let g = Geometry {
        start_um: caps.start_radius_um as f64,
        mark_pitch_nm: caps.mark_pitch_nm as f64,
        track_pitch_nm: caps.track_pitch_nm as f64,
        drive_inner_um: caps.inner_radius_um as f64,
        drive_outer_um: caps.outer_radius_um as f64,
    };
    (caps, g, page)
}

fn feature() -> LightScribeFeature {
    LightScribeFeature::parse(&td("get_configuration_ff33.bin")).unwrap()
}

#[test]
fn feature_0xff33() {
    let f = feature();
    assert_eq!(f.raw, [0xff, 0x33, 0x05, 0x04, 0xfc, 0x31, 0x32, 0x02]);
    assert!(f.current);
    assert_eq!((f.print_opcode, f.print_cdb1, f.labeling_page, f.media_page), (0xFC, 0x02, 0x31, 0x32));
}

#[test]
fn page_0x31_and_0x32() {
    let (caps, _, _) = caps_and_geom();
    assert_eq!(caps.drive_id, 169);
    assert_eq!((caps.mark_pitch_nm, caps.track_pitch_nm, caps.spindle_um_s), (42333, 50800, 250_000));
    assert_eq!((caps.start_radius_um, caps.inner_radius_um, caps.outer_radius_um), (23800, 21700, 58700));
    assert!((caps.write_power_mw().unwrap() - 28.67).abs() < 0.01);
    assert_eq!(caps.focus_offset_um, -20);
    let ms = td("mode_sense_all_current.bin");
    let m = MediaParams::parse(find_page(&ms, 0x32).unwrap()).unwrap();
    assert!(m.media_present);
    assert_eq!(m.spokes_per_rev, 400);
    assert_eq!(m.outer_id, KNOWN_MEDIA_ID_104);
}

#[test]
fn geometry_and_first_ring() {
    let (_, g, _) = caps_and_geom();
    assert!((g.radius_um(319) - 40005.2).abs() < 0.01);
    assert_eq!(g.dots(319), 5938);
    assert_eq!(track::data_len(5938), 744);
    let t = raster::full_ring(&g, 319);
    let p = t.payload();
    assert_eq!(p.len(), 760);
    assert_eq!(&p[..16], &[0x01, 0x3F, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17, 0x32]);
    assert!(p[16..16 + 742].iter().all(|&b| b == 0xFF));
    assert_eq!(&p[16 + 742..], &[0x03, 0x00]);
    assert_eq!(cdb::print(0xFC, 0x02, 760), vec![0xFC, 0x02, 0, 0, 0, 0, 0, 0x02, 0xF8, 0]);
    assert_eq!(track::last_track_payload(), vec![0, 0, 0, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn command_bytes() {
    assert_eq!(cdb::get_configuration(0xFF33), vec![0x46, 0x02, 0xFF, 0x33, 0, 0, 0, 0x01, 0, 0]);
    assert_eq!(cdb::mode_sense10(0x32), vec![0x5A, 0x08, 0x32, 0, 0, 0, 0, 0, 0xFF, 0]);
    assert_eq!(cdb::mode_select10(64), vec![0x55, 0x10, 0, 0, 0, 0, 0, 0, 0x40, 0]);
    assert_eq!(cdb::read_buffer_capacity(), vec![0x5C, 0, 0, 0, 0, 0, 0, 0, 0x0C, 0]);
    assert_eq!(cdb::start_stop(true), vec![0x1B, 0, 0, 0, 1, 0]);
    assert!(lartisan_core::opcode_allowed(0xFC) && !lartisan_core::opcode_allowed(0x2A));
}

#[test]
fn pitch_change_touches_only_pitch_bytes() {
    let (_, _, page) = caps_and_geom();
    for pitch in [33867u32, 25400] {
        let new = with_track_pitch(&page, pitch);
        let diff: Vec<usize> = (0..page.len()).filter(|&i| page[i] != new[i]).collect();
        assert!(diff.iter().all(|&i| (14..=16).contains(&i)), "{diff:?}");
        let list = mode_select_list(&new);
        assert_eq!(list.len(), 64);
        assert_eq!(&list[..8], &[0x00, 0x3E, 0x70, 0, 0, 0, 0, 0]);
    }
    assert_eq!(&with_track_pitch(&page, 25400)[14..17], &[0x00, 0x63, 0x38]);
}

#[test]
fn f_pattern_matches_the_burned_one() {
    let (_, g, _) = caps_and_geom();
    let f = raster::f_glyph(0.0, -35.0);
    let tracks = raster::rasterize(&g, &f, &RasterOptions { r_min_um: 29_000.0, r_max_um: 41_000.0 });
    assert_eq!((tracks.first().unwrap().n, tracks.last().unwrap().n), (123, 321));
    assert_eq!(tracks.len(), 199);
    assert_eq!(tracks.iter().map(|t| t.bits.count()).sum::<u32>(), 16741);
}

#[test]
fn wedge_densities_follow_targets() {
    let (_, g, _) = caps_and_geom();
    let w = raster::tone_wedge(27.0, 205.0, 235.0);
    let tracks = raster::rasterize(&g, &w, &RasterOptions { r_min_um: 26_500.0, r_max_um: 38_500.0 });
    for band in 1..=10u32 {
        let (mut on, mut tot) = (0u64, 0u64);
        for t in &tracks {
            let r = g.radius_um(t.n) / 1000.0;
            if r < 27.0 + band as f64 + 0.05 || r >= 27.0 + band as f64 + 0.95 {
                continue;
            }
            for i in 0..t.bits.dots {
                let a = (lartisan_core::geometry::ORIGIN_DEG + 360.0 * i as f64 / t.bits.dots as f64).rem_euclid(360.0);
                if (205.0..=235.0).contains(&a) {
                    tot += 1;
                    on += t.bits.get(i) as u64;
                }
            }
        }
        let got = 100.0 * on as f64 / tot as f64;
        assert!((got - band as f64 * 10.0).abs() < 1.5, "band {band}: {got}");
    }
}

#[test]
fn burn_steps_shape() {
    let (_, g, page) = caps_and_geom();
    let ring = raster::full_ring(&g, 319);
    let steps = burn_steps(&feature(), &[ring], None, false);
    let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["prevent", "start", "rbc", "track-319", "last", "drain", "tur", "stop", "allow"]);
    assert_eq!(steps[3].cdb, vec![0xFC, 0x02, 0, 0, 0, 0, 0, 0x02, 0xF8, 0]);
    let list = mode_select_list(&with_track_pitch(&page, 33867));
    let with = burn_steps(&feature(), &[], Some(&list), false);
    assert_eq!(with[1].id, "mode-select");
    assert_eq!(with[1].dir, Dir::Out(list));
}

#[test]
fn fine_wedge_at_best_pitch_follows_targets() {
    let (_, mut g, _) = caps_and_geom();
    g.track_pitch_nm = 25_400.0; // Best
    let (r0, band, levels, a0, a1) = (25.3, 0.5, 21u32, 238.0, 252.0);
    let tracks = raster::rasterize(&g, &raster::tone_wedge_n(r0, band, levels, a0, a1), &RasterOptions { r_min_um: 24_800.0, r_max_um: 36_300.0 });
    let got = raster::wedge_achieved(&tracks, &g, r0, band, levels, a0, a1);
    assert_eq!(got.len(), 21);
    for (k, d) in got.iter().enumerate() {
        let want = 100.0 * k as f64 / 20.0;
        assert!((d - want).abs() < 2.5, "level {k}: {d} vs {want}"); // the first band after 0% starts with a small error-diffusion transient
    }
    assert!(tracks.len() > 350 && tracks.len() < 450, "{}", tracks.len());
}

#[test]
fn best_pitch_tone_curve_is_sane() {
    let c = raster::ToneCurve::best_pitch();
    assert_eq!(c.response.len(), 21);
    assert!(c.response.windows(2).all(|w| w[1] >= w[0]), "monotone");
    assert_eq!(c.density_for(0.0), 0.0);
    assert!((c.density_for(1.0) - 0.95).abs() < 0.051);
    // the label is convex: half the darkness needs clearly more than half the dots
    let d = c.density_for(0.5);
    assert!(d > 0.55 && d < 0.70, "{d}");
    // monotone inverse
    let ds: Vec<f64> = (0..=10).map(|k| c.density_for(k as f64 / 10.0)).collect();
    assert!(ds.windows(2).all(|w| w[1] >= w[0]), "{ds:?}");
}


#[test]
fn image_offset_and_rotation_place_the_image_on_the_disc() {
    use lartisan_core::raster::{GrayImage, Sampler};
    // a 24 x 6 mm black image
    let img = GrayImage::from_gray8(48, 12, &vec![0u8; 48 * 12], 24.0, 1.0).with_offset(0.0, -31.5);
    assert_eq!(img.darkness(0.0, -31.5), 1.0);
    assert_eq!(img.darkness(0.0, -20.0), 0.0);
    assert_eq!(img.darkness(11.0, -31.5), 1.0, "12 mm either side along x");
    let (lo, hi) = img.radius_range_mm();
    assert!((lo - 28.5).abs() < 0.2 && (hi - 36.53).abs() < 0.1, "{lo} {hi}"); // corners at (+-12, -34.5)
    // rotated 90 degrees clockwise: the long side now runs along y
    let r = GrayImage::from_gray8(48, 12, &vec![0u8; 48 * 12], 24.0, 1.0).with_rotation(90.0);
    assert_eq!(r.darkness(0.0, -11.0), 1.0);
    assert_eq!(r.darkness(11.0, 0.0), 0.0);
    // along the tangent at 135 degrees the radial extent stays small
    let t = GrayImage::from_gray8(48, 12, &vec![0u8; 48 * 12], 24.0, 1.0).with_offset(24.0, -24.0).with_rotation(-45.0);
    let (lo, hi) = t.radius_range_mm();
    assert!(hi - lo < 8.0, "{lo} {hi}");
}


#[test]
fn short_labeling_page_layout_and_mode_select_header() {
    // a 44-byte page (length byte 0x2A) is accepted; its MODE SELECT header says 50 (0x32)
    let (_, _, page) = caps_and_geom();
    let mut short = page[..44].to_vec();
    short[1] = 0x2A;
    let caps = DriveCaps::parse(&short).expect("short layout");
    assert_eq!((caps.drive_id, caps.track_pitch_nm), (169, 50_800));
    assert_eq!(&mode_select_list(&short)[..3], &[0x00, 0x32, 0x70]);
    assert_eq!(&mode_select_list(&page)[..3], &[0x00, 0x3E, 0x70]);
    // unknown layouts are refused rather than guessed at
    let mut odd = page.clone();
    odd[1] = 0x40;
    assert!(DriveCaps::parse(&odd).is_none());
    assert!(DriveCaps::parse(&page[..20]).is_none());
}
