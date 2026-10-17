//! Executor + guard against the simulated drive, using the real capture bytes.

use lartisan_core::geometry::Geometry;
use lartisan_core::pages::*;
use lartisan_core::plan::burn_steps;
use lartisan_core::raster::{self, RasterOptions};
use lartisan_scsi::executor::{read_drive_info, run, Config};
use lartisan_scsi::guard::Guarded;
use lartisan_scsi::sim::SimDrive;
use lartisan_scsi::{Direction, ScsiTransport};
use std::sync::atomic::AtomicBool;

fn td(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../testdata/{}", env!("CARGO_MANIFEST_DIR"), name)).unwrap()
}

fn sim() -> SimDrive {
    let cfg = td("get_configuration_ff33.bin");
    let ms = td("mode_sense_all_current.bin");
    let mut f = [0u8; 8];
    f.copy_from_slice(&cfg[8..16]);
    SimDrive::new(f, find_page(&ms, 0x31).unwrap().to_vec(), find_page(&ms, 0x32).unwrap().to_vec())
}

fn geom(p31: &[u8]) -> Geometry {
    let c = DriveCaps::parse(p31).unwrap();
    Geometry { start_um: c.start_radius_um as f64, mark_pitch_nm: c.mark_pitch_nm as f64, track_pitch_nm: c.track_pitch_nm as f64, drive_inner_um: c.inner_radius_um as f64, drive_outer_um: c.outer_radius_um as f64 }
}

#[test]
fn preflight_reads_the_drive() {
    let mut g = Guarded::new(sim());
    let mut lines = vec![];
    let info = read_drive_info(&mut g, &Config::instant(), &mut |s| lines.push(s.to_string())).unwrap();
    assert!(info.feature.current);
    assert_eq!(info.buffer_total, 1_611_920);
    assert_eq!(MediaParams::parse(&info.page32).unwrap().outer_id, KNOWN_MEDIA_ID_104);
}

#[test]
fn preflight_tolerates_becoming_ready_and_unit_attention() {
    let mut s = sim();
    s.ready_polls = 3;
    s.unit_attention_once = true;
    let mut g = Guarded::new(s);
    assert!(read_drive_info(&mut g, &Config::instant(), &mut |_| {}).is_ok());
}

#[test]
fn guard_refuses_unlisted_opcodes_and_unarmed_writes() {
    let mut g = Guarded::new(sim());
    let e = g.execute(&[0x2A, 0, 0, 0, 0, 0, 0, 0, 1, 0], Direction::None, &mut [], 1000).unwrap_err();
    assert!(e.0.contains("allowlist"), "{e}");
    let mut payload = vec![0u8; 16];
    let e = g.execute(&[0xFC, 2, 0, 0, 0, 0, 0, 0, 16, 0], Direction::Out, &mut payload, 1000).unwrap_err();
    assert!(e.0.contains("armed"), "{e}");
    g.arm();
    assert!(g.execute(&[0xFC, 2, 0, 0, 0, 0, 0, 0, 16, 0], Direction::Out, &mut payload, 1000).is_ok());
}

#[test]
fn burns_the_f_through_the_busy_writing_phases() {
    let s = sim();
    let (feat, p31) = (LightScribeFeature::parse(&{ let mut v = vec![0, 0, 0, 12, 0, 0, 0xFF, 0xFF]; v.extend_from_slice(&s.feature); v }).unwrap(), s.page31.clone());
    let g = geom(&p31);
    let tracks = raster::rasterize(&g, &raster::f_glyph(0.0, -35.0), &RasterOptions { r_min_um: 29_000.0, r_max_um: 41_000.0 });
    let steps = burn_steps(&feat, &tracks, None, false);
    let mut guarded = Guarded::new(s);
    guarded.arm();
    let mut seen = 0;
    run(&mut guarded, &Config::instant(), &steps, None, None, &AtomicBool::new(false), &mut |_| {}, &mut |done, total| {
        seen = done;
        assert!(done <= total);
    })
    .unwrap();
    assert_eq!(seen, 199);
    let sim = guarded.into_inner();
    assert_eq!(sim.tracks.len(), 200); // 199 tracks + the last-track marker
    assert!(sim.tracks.last().unwrap().2);
    assert!(!sim.started && !sim.locked, "STOP and ALLOW must run");
}

#[test]
fn error_mid_burn_still_stops_restores_and_allows() {
    let mut s = sim();
    s.fail_at_track = Some(5);
    let feat = LightScribeFeature::parse(&{ let mut v = vec![0, 0, 0, 12, 0, 0, 0xFF, 0xFF]; v.extend_from_slice(&s.feature); v }).unwrap();
    let orig = s.page31.clone();
    let g = geom(&orig);
    let list = mode_select_list(&with_track_pitch(&orig, 33867));
    let restore = mode_select_list(&orig);
    let expect = with_track_pitch(&orig, 33867);
    let tracks = raster::rasterize(&g, &raster::f_glyph(0.0, -35.0), &RasterOptions { r_min_um: 29_000.0, r_max_um: 41_000.0 });
    let steps = burn_steps(&feat, &tracks, Some(&list), false);
    let mut guarded = Guarded::new(s);
    guarded.arm();
    let mut log = vec![];
    let r = run(&mut guarded, &Config::instant(), &steps, Some(&restore), Some(&expect), &AtomicBool::new(false), &mut |l| log.push(l.to_string()), &mut |_, _| {});
    assert!(r.is_err());
    let sim = guarded.into_inner();
    assert_eq!(sim.page31, orig, "original page 0x31 must be restored");
    assert!(sim.tracks.last().unwrap().2, "an unfinished job must be closed with the last-track marker");
    assert!(!sim.started && !sim.locked);
    assert!(log.iter().any(|l| l.contains("restored to the original")));
}

#[test]
fn cancel_runs_cleanup() {
    let s = sim();
    let feat = LightScribeFeature::parse(&{ let mut v = vec![0, 0, 0, 12, 0, 0, 0xFF, 0xFF]; v.extend_from_slice(&s.feature); v }).unwrap();
    let g = geom(&s.page31.clone());
    let tracks = vec![raster::full_ring(&g, 319)];
    let steps = burn_steps(&feat, &tracks, None, false);
    let mut guarded = Guarded::new(s);
    guarded.arm();
    let cancel = AtomicBool::new(true);
    let r = run(&mut guarded, &Config::instant(), &steps, None, None, &cancel, &mut |_| {}, &mut |_, _| {});
    assert!(matches!(r, Err(lartisan_scsi::executor::ExecError::Cancelled)));
    assert!(!guarded.into_inner().locked);
}

#[test]
fn start_refused_because_the_drive_is_already_writing_is_not_fatal() {
    let s = sim();
    let feat = LightScribeFeature::parse(&{ let mut v = vec![0, 0, 0, 12, 0, 0, 0xFF, 0xFF]; v.extend_from_slice(&s.feature); v }).unwrap();
    let g = geom(&s.page31.clone());
    let tracks = raster::rasterize(&g, &raster::f_glyph(0.0, -35.0), &RasterOptions { r_min_um: 29_000.0, r_max_um: 41_000.0 });
    // prefill order: START comes after the tracks, when the simulated drive is already busy writing
    let steps = burn_steps(&feat, &tracks, None, true);
    let mut guarded = Guarded::new(s);
    guarded.arm();
    run(&mut guarded, &Config::instant(), &steps, None, None, &AtomicBool::new(false), &mut |_| {}, &mut |_, _| {}).unwrap();
    assert!(guarded.into_inner().tracks.last().unwrap().2);
}

#[test]
fn cleanup_retries_the_marker_and_stop_while_the_drive_is_busy() {
    // As on the bench: TEST UNIT READY is GOOD while the drive still refuses STOP as busy.
    let mut s = sim();
    s.tur_ready_while_writing = true;
    let feat = LightScribeFeature::parse(&{ let mut v = vec![0, 0, 0, 12, 0, 0, 0xFF, 0xFF]; v.extend_from_slice(&s.feature); v }).unwrap();
    let g = geom(&s.page31.clone());
    let tracks = raster::rasterize(&g, &raster::f_glyph(0.0, -35.0), &RasterOptions { r_min_um: 29_000.0, r_max_um: 41_000.0 });
    let steps = burn_steps(&feat, &tracks, None, false);
    let mut guarded = Guarded::new(s);
    guarded.arm();
    // cancel right after the 11th track, when the simulated drive has started writing
    let cancel = AtomicBool::new(false);
    let r = run(&mut guarded, &Config::instant(), &steps, None, None, &cancel, &mut |_| {}, &mut |done, _| {
        if done == 11 {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });
    assert!(matches!(r, Err(lartisan_scsi::executor::ExecError::Cancelled)));
    let sim = guarded.into_inner();
    assert!(sim.tracks.last().unwrap().2, "the marker must get through once the drive is idle");
    assert!(!sim.started && !sim.locked, "STOP and ALLOW must get through");
}

#[test]
fn pitch_change_on_the_short_labeling_page_is_restored() {
    let mut s = sim();
    s.page31.truncate(44);
    s.page31[1] = 0x2A;
    let orig = s.page31.clone();
    let feat = LightScribeFeature::parse(&{ let mut v = vec![0, 0, 0, 12, 0, 0, 0xFF, 0xFF]; v.extend_from_slice(&s.feature); v }).unwrap();
    let g = geom(&orig);
    let (list, restore, expect) = (mode_select_list(&with_track_pitch(&orig, 33867)), mode_select_list(&orig), with_track_pitch(&orig, 33867));
    let tracks = vec![raster::full_ring(&g, 319)];
    let steps = burn_steps(&feat, &tracks, Some(&list), false);
    let mut guarded = Guarded::new(s);
    guarded.arm();
    let mut log = vec![];
    run(&mut guarded, &Config::instant(), &steps, Some(&restore), Some(&expect), &AtomicBool::new(false), &mut |l| log.push(l.to_string()), &mut |_, _| {}).unwrap();
    assert!(log.iter().any(|l| l.contains("differs from the original only in the pitch bytes")));
    assert_eq!(guarded.into_inner().page31, orig);
    assert!(log.iter().any(|l| l.contains("restored to the original")));
}
