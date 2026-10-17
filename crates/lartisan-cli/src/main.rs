//! lartisan: command-line front end.
//!   plan   offline dry run from a probe capture (nothing touches a drive)
//!   probe  read-only: open the drive, run the preflight, print what it reports
//!   burn   live: preflight, show the plan, and (only with --arm and a typed track number) burn

use lartisan_core::geometry::Geometry;
use lartisan_core::media_id::decode_outer_id;
use lartisan_core::media_profile::{profile, PitchSource, ToneSource, CALIBRATED_DRIVE_ID, CALIBRATED_PITCH_NM, MIN_PITCH_NM};
use lartisan_core::pages::*;
use lartisan_core::plan::{burn_steps, Dir, Step};
use lartisan_core::raster::{self, GrayImage, RasterOptions};
use lartisan_core::track::Track;
use lartisan_scsi::executor::{self, DriveInfo};
use lartisan_scsi::guard::Guarded;
use lartisan_scsi::ScsiTransport;
use std::io::Write;
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn usage() -> ! {
    eprintln!(
        "usage:\n  lartisan plan  PATTERN [--capture DIR] [options]\n  lartisan probe\n  lartisan burn  PATTERN [--arm] [options]\n\n\
         PATTERN: ring | f | wedge | band | image\n\
         options: --offset X Y (image centre, mm)  --rotate DEG (clockwise)  --prefill  --band-r0 MM --band-width MM  --sector A0 A1  --ring-n N  --f-center X Y (mm)  --wedge-r0 MM --wedge-band-mm MM --wedge-levels N  --pitch-nm N | --pitch auto  --media-id \"15 hex bytes\" (plan only)  --wedge-sector A0 A1  --image FILE.png  --size-mm N  --gamma G  --tone auto | best (best = Memorex curve; needs a 25400 nm pitch: --pitch-nm 25400, or --pitch auto on Memorex media)\n\n\
         plan is offline. probe only reads. burn fires the laser only with --arm and a typed confirmation."
    );
    exit(2);
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    exit(1);
}

#[derive(Default)]
struct Opts {
    pattern: String,
    capture: Option<String>,
    ring_n: u32,
    pitch: Option<u32>,
    pitch_auto: bool,
    media_id: Option<[u8; 15]>,
    sector: (f64, f64),
    image: Option<String>,
    size_mm: f64,
    offset: (f64, f64),
    rotate: f64,
    gamma: f64,
    arm: bool,
    tone_best: bool,
    tone_auto: bool,
    prefill: bool,
    band: (f64, f64), // r0 mm, width mm
    f_center: (f64, f64),
    wedge: (f64, f64, u32), // r0 mm, band mm, levels
}

fn parse(args: &[String]) -> Opts {
    let mut o = Opts { pattern: "ring".into(), ring_n: 319, sector: (205.0, 235.0), size_mm: 60.0, gamma: 1.0, f_center: (0.0, -35.0), wedge: (27.0, 1.0, 11), band: (30.0, 3.0), ..Default::default() };
    let mut i = 0;
    let next = |i: &mut usize| -> String { *i += 1; args.get(*i).cloned().unwrap_or_else(|| usage()) };
    if let Some(p) = args.first() {
        if !p.starts_with("--") {
            o.pattern = p.clone();
            i = 1;
        }
    }
    while i < args.len() {
        match args[i].as_str() {
            "--capture" => o.capture = Some(next(&mut i)),
            "--ring-n" => o.ring_n = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--pitch" => match next(&mut i).as_str() {
                "auto" => o.pitch_auto = true,
                _ => usage(),
            },
            "--media-id" => {
                let bytes: Vec<u8> = next(&mut i).split(|c: char| c == ' ' || c == ',').filter(|x| !x.is_empty()).map(|x| u8::from_str_radix(x.trim_start_matches("0x"), 16).unwrap_or_else(|_| usage())).collect();
                o.media_id = Some(bytes.try_into().unwrap_or_else(|_| die("--media-id needs exactly 15 hex bytes")));
            }
            "--pitch-nm" => o.pitch = Some(next(&mut i).parse().unwrap_or_else(|_| usage())),
            "--prefill" => o.prefill = true,
            "--band-r0" => o.band.0 = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--band-width" => o.band.1 = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--sector" | "--wedge-sector" => {
                let a = next(&mut i).parse().unwrap_or_else(|_| usage());
                let b = next(&mut i).parse().unwrap_or_else(|_| usage());
                o.sector = (a, b);
            }
            "--wedge-r0" => o.wedge.0 = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--wedge-band-mm" => o.wedge.1 = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--wedge-levels" => o.wedge.2 = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--f-center" => {
                let x = next(&mut i).parse().unwrap_or_else(|_| usage());
                let y = next(&mut i).parse().unwrap_or_else(|_| usage());
                o.f_center = (x, y);
            }
            "--image" => o.image = Some(next(&mut i)),
            "--rotate" => o.rotate = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--offset" => {
                let x = next(&mut i).parse().unwrap_or_else(|_| usage());
                let y = next(&mut i).parse().unwrap_or_else(|_| usage());
                o.offset = (x, y);
            }
            "--size-mm" => o.size_mm = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--gamma" => o.gamma = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--arm" => o.arm = true,
            "--tone" => match next(&mut i).as_str() {
                "best" => o.tone_best = true,
                "auto" => o.tone_auto = true,
                _ => usage(),
            },
            _ => usage(),
        }
        i += 1;
    }
    o
}

fn load_png(path: &str, size_mm: f64, gamma: f64, tone: Option<raster::ToneCurve>, offset: (f64, f64), rotate: f64) -> GrayImage {
    let file = std::fs::File::open(path).unwrap_or_else(|e| die(format!("cannot open {path}: {e}")));
    let mut dec = png::Decoder::new(std::io::BufReader::new(file));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().unwrap_or_else(|e| die(format!("bad PNG: {e}")));
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap_or_else(|e| die(format!("bad PNG: {e}")));
    let (w, h) = (info.width as usize, info.height as usize);
    let ch = info.color_type.samples();
    let gray: Vec<u8> = (0..w * h)
        .map(|p| {
            let px = &buf[p * ch..p * ch + ch];
            let (g, a) = match info.color_type {
                png::ColorType::Grayscale => (px[0] as f32, 255.0),
                png::ColorType::GrayscaleAlpha => (px[0] as f32, px[1] as f32),
                png::ColorType::Rgb => (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32, 255.0),
                png::ColorType::Rgba => (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32, px[3] as f32),
                png::ColorType::Indexed => (px[0] as f32, 255.0),
            };
            // composite over white: transparent areas stay unmarked
            (g * a / 255.0 + 255.0 * (1.0 - a / 255.0)).round() as u8
        })
        .collect();
    let img = GrayImage::from_gray8(w, h, &gray, size_mm, gamma).with_offset(offset.0, offset.1).with_rotation(rotate);
    match tone {
        Some(t) => img.with_tone(t, &gray),
        None => img,
    }
}

struct Prepared {
    feat: LightScribeFeature,
    caps: DriveCaps,
    geom: Geometry,
    tracks: Vec<Track>,
    steps: Vec<Step>,
    restore: Option<Vec<u8>>,
    expect: Option<Vec<u8>>,
}

/// Validate the drive/media and build the plan. Refuses anything not yet characterised.
fn prepare(info: &DriveInfo, o: &Opts) -> Prepared {
    let media = MediaParams::parse(&info.page32).unwrap_or_else(|| die("the drive's media page has an unexpected layout; refusing to guess"));
    let caps = DriveCaps::parse(&info.page31).unwrap_or_else(|| die("the drive's labeling page has an unexpected layout (neither the 56-byte nor the 44-byte form); refusing to guess"));
    if !info.feature.current || !media.media_present {
        die("no LightScribe media detected (is the disc label side down?)");
    }
    if info.feature.print_opcode != lartisan_core::PRINT_OPCODE {
        die(format!(
            "refusing: the drive's print opcode is 0x{:02X}; only 0x{:02X} is supported (and allowed by the safety guard)",
            info.feature.print_opcode,
            lartisan_core::PRINT_OPCODE
        ));
    }
    if !media.outer_id_valid {
        die("the disc's outer media ID is not marked valid");
    }
    let decoded = decode_outer_id(&media.outer_id).unwrap_or_else(|e| die(format!("media ID could not be decoded ({e:?}); reload the disc and retry")));
    let mid = &decoded.id;
    println!(
        "media ID decoded: imaging parameter {}, manufacturer {}, laser type {}, write power window {:.0}-{:.0} mW at 250 mm/s{}",
        mid.imaging_parameter,
        mid.manufacturer,
        mid.laser_type,
        mid.power_window_mw(250.0).0,
        mid.power_window_mw(250.0).1,
        if decoded.erasures + decoded.corrected > 0 { format!(" ({} damaged symbols repaired)", decoded.erasures + decoded.corrected) } else { String::new() }
    );
    if mid.label_shape != 1 {
        die(format!("refusing: label shape {} is not supported yet (only shape 1: 23.8-58.7 mm)", mid.label_shape));
    }
    // Safety rests on the media's own power window and the drive's defaults (checked below). What
    // can differ is only how dark the label comes out, so say when the curve is an estimate.
    if profile(caps.drive_id, mid).tone_source == ToneSource::Estimated {
        println!("note: no tone measurement for media type {} on this drive; --tone auto uses an estimated curve, so darkness may differ", mid.imaging_parameter);
    }
    // Drive checks. Any drive is allowed: what protects the laser is the disc's own power window,
    // which needs the drive's write-power conversion (HP's table lists 123 drives).
    let power_mw = caps.write_power_mw().unwrap_or_else(|| {
        die(format!("DriveID {} is not in HP's drive table, so its write-power units cannot be converted and the disc's power window cannot be checked", caps.drive_id))
    });
    if caps.drive_id != CALIBRATED_DRIVE_ID {
        println!(
            "note: DriveID {} has not been used with this tool. The pitches and tone curves were measured on DriveID {CALIBRATED_DRIVE_ID} (HP DVD Writer 1040r), so here they are estimates; burn a test wedge first.",
            caps.drive_id
        );
    }
    let speed_mm_s = caps.spindle_um_s as f64 / 1000.0;
    if speed_mm_s < mid.low_velocity_mm_s as f64 * 0.9 || speed_mm_s > mid.high_velocity_mm_s as f64 * 1.1 {
        die(format!("refusing: the drive's labeling speed {speed_mm_s:.0} mm/s is outside the disc's range {}-{} mm/s", mid.low_velocity_mm_s, mid.high_velocity_mm_s));
    }
    let (pmin, pmax) = mid.power_window_mw(speed_mm_s);
    if power_mw < pmin || power_mw > pmax {
        die(format!("refusing: write power {power_mw:.2} mW at {speed_mm_s:.0} mm/s is outside the disc's window {pmin:.1}-{pmax:.1} mW"));
    }
    if info.buffer_free != info.buffer_total || info.buffer_total < 65_536 {
        die("refusing: unexpected label buffer state (not empty, or implausibly small)");
    }
    let mut pitch = o.pitch;
    if o.pitch_auto {
        let c = profile(caps.drive_id, mid).pitch;
        let how = match c.source {
            PitchSource::Calibrated => "the pitch at which this media's tone curve was measured".to_string(),
            PitchSource::Table { best_tpi } => format!("HP's table gives Best = {best_tpi} tpi for this media on this drive, scaled against Memorex's Best (calibrated at 25400 nm)"),
            PitchSource::Formula { pmax_mw } => format!("ESTIMATE: imaging parameter not in HP's table; from the reported maximum power {pmax_mw} mW (a fit to three discs, unvalidated)"),
        };
        println!("track pitch {} nm ({:.0} tpi): {how}", c.pitch_nm, 25_400_000.0 / c.pitch_nm as f64);
        pitch = Some(c.pitch_nm);
    }
    let mut geom = Geometry {
        start_um: caps.start_radius_um as f64,
        mark_pitch_nm: caps.mark_pitch_nm as f64,
        track_pitch_nm: caps.track_pitch_nm as f64,
        drive_inner_um: caps.inner_radius_um as f64,
        drive_outer_um: caps.outer_radius_um as f64,
    };
    let (mut restore, mut expect, mut list) = (None, None, None);
    if let Some(p) = pitch {
        if !o.pitch_auto && !(MIN_PITCH_NM..=50_800).contains(&p) {
            die(format!("pitch must be {MIN_PITCH_NM}..50800 nm (the finest pitch burned so far .. the 1040r's Draft default)"));
        }
        let newpage = with_track_pitch(&info.page31, p);
        list = Some(mode_select_list(&newpage));
        restore = Some(mode_select_list(&info.page31));
        expect = Some(newpage);
        geom.track_pitch_nm = p as f64;
    }
    let tracks: Vec<Track> = match o.pattern.as_str() {
        "ring" => {
            if !geom.in_band(o.ring_n) {
                let band: Vec<u32> = (0..10_000).filter(|&n| geom.in_band(n)).collect();
                let (lo, hi) = (band.first().copied().unwrap_or(0), band.last().copied().unwrap_or(0));
                die(format!("--ring-n {} is outside the drive's label band (tracks {lo}..{hi} at {} nm)", o.ring_n, geom.track_pitch_nm));
            }
            vec![raster::full_ring(&geom, o.ring_n)]
        }
        "f" => {
            let (cx, cy) = o.f_center;
            let cr = cx.hypot(cy) * 1000.0;
            // the 7 x 10 mm F reaches at most ~6.2 mm from its centre
            raster::rasterize(&geom, &raster::f_glyph(cx, cy), &RasterOptions { r_min_um: (cr - 6_500.0).max(geom.start_um), r_max_um: (cr + 6_500.0).min(geom.drive_outer_um - 100.0) })
        }
        "band" => {
            let (r0, w) = o.band;
            raster::rasterize(&geom, &raster::solid_band(r0, w, o.sector.0, o.sector.1), &RasterOptions { r_min_um: ((r0 - 0.3) * 1000.0).max(geom.start_um), r_max_um: ((r0 + w + 0.3) * 1000.0).min(geom.drive_outer_um - 100.0) })
        }
        "wedge" => {
            let (r0, band, levels) = o.wedge;
            if levels < 2 || band <= 0.0 {
                die("--wedge-levels must be at least 2 and --wedge-band-mm positive");
            }
            raster::rasterize(&geom, &raster::tone_wedge_n(r0, band, levels, o.sector.0, o.sector.1), &RasterOptions { r_min_um: ((r0 - 0.5) * 1000.0).max(geom.start_um), r_max_um: ((r0 + band * levels as f64 + 0.5) * 1000.0).min(geom.drive_outer_um - 100.0) })
        }
        "image" => {
            let path = o.image.as_deref().unwrap_or_else(|| die("--image FILE.png is required for the image pattern"));
            let prof = profile(caps.drive_id, mid);
            let tone = if o.tone_auto {
                if !o.pitch_auto {
                    die("--tone auto goes with --pitch auto (the curve belongs to the media's own pitch)");
                }
                println!("tone curve: {} for media type {}", if prof.tone_source == ToneSource::Measured { "measured" } else { "ESTIMATED (Memorex shape; darkness may differ)" }, mid.imaging_parameter);
                Some(prof.tone)
            } else if o.tone_best {
                if pitch != Some(CALIBRATED_PITCH_NM) {
                    die(format!("--tone best is the Memorex curve, measured at {CALIBRATED_PITCH_NM} nm only: use --pitch-nm {CALIBRATED_PITCH_NM}, or --tone auto with --pitch auto for this disc"));
                }
                Some(raster::ToneCurve::best_pitch())
            } else {
                None
            };
            let img = load_png(path, o.size_mm, o.gamma, tone, o.offset, o.rotate);
            let (rlo, rhi) = img.radius_range_mm();
            raster::rasterize(&geom, &img, &RasterOptions { r_min_um: ((rlo - 0.3) * 1000.0).max(geom.start_um), r_max_um: ((rhi + 0.3) * 1000.0).min(geom.drive_outer_um - 100.0) })
        }
        _ => usage(),
    };
    if tracks.is_empty() {
        die("the pattern produced no marks");
    }
    let marked: u32 = tracks.iter().map(|t| t.bits.count()).sum();
    if marked > 1_500_000 {
        die("refusing: laser-on budget exceeded");
    }
    if o.prefill {
        let bytes: usize = tracks.iter().map(|t| t.payload().len()).sum();
        if bytes + 4096 > info.buffer_total as usize {
            die(format!("--prefill needs the whole job ({bytes} bytes) to fit in the drive's {} byte buffer", info.buffer_total));
        }
    }
    let steps = burn_steps(&info.feature, &tracks, list.as_deref(), o.prefill);
    Prepared { feat: info.feature.clone(), caps, geom, tracks, steps, restore, expect }
}

fn summarize(p: &Prepared, o: &Opts) {
    let marked: u32 = p.tracks.iter().map(|t| t.bits.count()).sum();
    let secs: f64 = 40.0 + p.tracks.iter().map(|t| p.geom.rev_seconds(t.n, p.caps.spindle_um_s as f64)).sum::<f64>();
    println!("pattern {}: {} tracks (n={}..{}), {marked} dots marked, track pitch {} nm, about {:.1} min", o.pattern, p.tracks.len(), p.tracks[0].n, p.tracks.last().unwrap().n, p.geom.track_pitch_nm, secs / 60.0);
    println!("print opcode {:02X} {:02X}; write power {:.2} mW", p.feat.print_opcode, p.feat.print_cdb1, p.caps.write_power_mw().unwrap_or(f64::NAN));
    if o.pattern == "wedge" {
        let (r0, band, levels) = o.wedge;
        println!("tone wedge: {levels} bands of {band} mm from r={r0} mm, sector {}..{} deg (clockwise from the mark); achieved density per band:", o.sector.0, o.sector.1);
        let got = raster::wedge_achieved(&p.tracks, &p.geom, r0, band, levels, o.sector.0, o.sector.1);
        let line: Vec<String> = got.iter().enumerate().map(|(k, g)| format!("{:>3.0}%->{:>5.1}%", 100.0 * k as f64 / (levels - 1) as f64, g)).collect();
        for chunk in line.chunks(7) {
            println!("  {}", chunk.join("  "));
        }
    }
    let n_track = p.steps.iter().filter(|s| s.id.starts_with("track-")).count();
    let mut shown = 0;
    for s in &p.steps {
        if s.id.starts_with("track-") {
            shown += 1;
            if n_track > 4 && shown > 2 && shown <= n_track - 2 {
                if shown == 3 {
                    println!("  ... ({} more track steps) ...", n_track - 4);
                }
                continue;
            }
        }
        println!("{:<12} {:<44} CDB {}", s.id, s.name, hex(&s.cdb));
        if let Dir::Out(d) = &s.dir {
            println!("             -> {} bytes: {}{}", d.len(), hex(&d[..d.len().min(16)]), if d.len() > 16 { " ..." } else { "" });
        }
    }
}

fn capture_info(dir: &str, media_id: Option<[u8; 15]>) -> DriveInfo {
    let read = |n: &str| std::fs::read(format!("{dir}/{n}")).unwrap_or_else(|e| die(format!("cannot read {dir}/{n}: {e}")));
    let feature = LightScribeFeature::parse(&read("get_configuration_ff33.bin")).unwrap_or_else(|| die("no feature 0xFF33 in the capture"));
    let ms = read("mode_sense_all_current.bin");
    let mut page32 = find_page(&ms, feature.media_page).unwrap_or_else(|| die("capture lacks the media page")).to_vec();
    if let Some(id) = media_id {
        page32[8..23].copy_from_slice(&id); // pretend this disc is loaded (offline plans only)
    }
    let page31 = find_page(&ms, feature.labeling_page).unwrap_or_else(|| die("capture lacks the labeling page")).to_vec();
    // the captured buffer state, or the 1040r's empty 1 611 920-byte buffer if it was not captured
    let (buffer_total, buffer_free) = match std::fs::read(format!("{dir}/read_buffer_capacity.bin")) {
        Ok(b) if b.len() >= 12 => (u32::from_be_bytes([b[4], b[5], b[6], b[7]]), u32::from_be_bytes([b[8], b[9], b[10], b[11]])),
        _ => (1_611_920, 1_611_920),
    };
    DriveInfo { feature, page31, page32, buffer_total, buffer_free }
}

fn log_to(file: &mut std::fs::File) -> impl FnMut(&str) + '_ {
    let t0 = std::time::Instant::now();
    move |s: &str| {
        println!("{s}");
        let _ = writeln!(file, "{:9.3} {s}", t0.elapsed().as_secs_f64());
    }
}

fn open_log() -> (std::fs::File, String) {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = format!("lartisan-{ts}.log");
    (std::fs::File::create(&path).unwrap_or_else(|e| die(format!("cannot create {path}: {e}"))), path)
}

fn live_info(t: &mut dyn ScsiTransport, log: &mut dyn FnMut(&str)) -> DriveInfo {
    executor::read_drive_info(t, &executor::Config::default(), log).unwrap_or_else(|e| die(format!("preflight failed: {e}")))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().cloned() else { usage() };
    match cmd.as_str() {
        "plan" => {
            let o = parse(&args[1..]);
            let info = capture_info(o.capture.as_deref().unwrap_or("testdata"), o.media_id);
            summarize(&prepare(&info, &o), &o);
        }
        "probe" => {
            let t = lartisan_scsi::open_default().unwrap_or_else(|e| die(e));
            let mut g = Guarded::new(t);
            let (mut f, path) = open_log();
            let mut log = log_to(&mut f);
            let info = live_info(&mut g, &mut log);
            let caps = DriveCaps::parse(&info.page31);
            let media = MediaParams::parse(&info.page32);
            println!("\nfeature 0xFF33: {}  (LightScribe media {})", hex(&info.feature.raw), if info.feature.current { "detected" } else { "not detected" });
            if let Some(c) = caps {
                println!("DriveID {}  pitch {} nm  mark {} nm  write {:.2} mW  radii {}..{} um", c.drive_id, c.track_pitch_nm, c.mark_pitch_nm, c.write_power_mw().unwrap_or(f64::NAN), c.inner_radius_um, c.outer_radius_um);
            }
            if let Some(m) = media {
                println!("media present {}  spokes {}  outer media ID {}", m.media_present, m.spokes_per_rev, hex(&m.outer_id));
            }
            if let Some(m) = MediaParams::parse(&info.page32) {
                match decode_outer_id(&m.outer_id) {
                    Ok(d) => {
                        let i = &d.id;
                        let (pmin, pmax) = i.power_window_mw(250.0);
                        println!(
                            "media ID decoded ({} repaired): imaging parameter {}, manufacturer {}, laser type {}, media class {}, label shape {}\n  write power low-speed {}-{} mW, high-speed {}-{} mW; velocity {}..{} mm/s; ratio {}\n  window at 250 mm/s: {:.1}-{:.1} mW",
                            d.erasures + d.corrected, i.imaging_parameter, i.manufacturer, i.laser_type, i.media_class, i.label_shape,
                            i.low_speed_min_power_mw, i.low_speed_max_power_mw, i.high_speed_min_power_mw, i.high_speed_max_power_mw,
                            i.low_velocity_mm_s, i.high_velocity_mm_s, i.write_read_ratio, pmin, pmax
                        );
                    }
                    Err(e) => println!("media ID could not be decoded: {e:?}"),
                }
            }
            println!("buffer {} / {} bytes free\nlog: {path}", info.buffer_free, info.buffer_total);
        }
        "burn" => {
            let o = parse(&args[1..]);
            if o.media_id.is_some() || o.capture.is_some() {
                die("--media-id and --capture are for plan only; burn always reads the loaded disc");
            }
            let t = lartisan_scsi::open_default().unwrap_or_else(|e| die(e));
            let mut g = Guarded::new(t);
            let (mut f, path) = open_log();
            let mut log = log_to(&mut f);
            let info = live_info(&mut g, &mut log);
            let p = prepare(&info, &o);
            println!();
            summarize(&p, &o);
            if !o.arm {
                println!("\nNot armed: nothing was written. Re-run with --arm to burn. Log: {path}");
                return;
            }
            let first = p.tracks[0].n;
            print!("\nThis will FIRE THE LASER ({} tracks, first track {first}). Type {first} to arm: ", p.tracks.len());
            std::io::stdout().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).ok();
            if line.trim().parse::<u32>().ok() != Some(first) {
                println!("Not armed: nothing was written.");
                return;
            }
            g.arm();
            let cancel = Arc::new(AtomicBool::new(false));
            let c2 = cancel.clone();
            ctrlc::set_handler(move || c2.store(true, Ordering::Relaxed)).ok();
            let r = executor::run(&mut g, &executor::Config::default(), &p.steps, p.restore.as_deref(), p.expect.as_deref(), &cancel, &mut log, &mut |done, total| {
                if done % 25 == 0 || done == total {
                    println!("tracks sent: {done}/{total}");
                }
            });
            match r {
                Ok(()) => println!("burn sequence complete. Log: {path}"),
                Err(e) => die(format!("burn failed: {e} (cleanup ran). Log: {path}")),
            }
        }
        _ => usage(),
    }
}
