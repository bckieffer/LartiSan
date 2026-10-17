//! Runs a planned burn against any transport.
//!
//! Rules learned on the bench (see docs/ARCHITECTURE.md):
//! * While the drive writes, READ BUFFER CAPACITY, TEST UNIT READY and STOP answer 02/04/08.
//!   That is "busy", not an error: wait and retry.
//! * A STOP during a write is refused, so cleanup waits for the drive to go idle first. TEST UNIT
//!   READY can already be GOOD while STOP is still refused, so cleanup also retries the marker and
//!   STOP themselves for as long as they are answered 02/04/08.
//! * The only MODE SELECT ever sent changes the track pitch; it is always restored and verified.

use crate::{Direction, ScsiResult, ScsiTransport};
use lartisan_core::cdb;
use lartisan_core::pages::{find_page, LightScribeFeature};
use lartisan_core::plan::{Dir, Kind, Step};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub poll: Duration,
    pub drain_interval: Duration,
    pub tur_polls: u32,
    pub space_polls: u32,
    pub drain_polls: u32,
    pub cleanup_polls: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(200),
            drain_interval: Duration::from_secs(1),
            tur_polls: 150,      // ~30 s
            space_polls: 3000,   // ~10 min
            drain_polls: 900,    // ~15 min
            cleanup_polls: 600,  // ~2 min
        }
    }
}

impl Config {
    /// No sleeping, small limits: for tests against the simulator.
    pub fn instant() -> Self {
        Self { poll: Duration::ZERO, drain_interval: Duration::ZERO, tur_polls: 50, space_polls: 500, drain_polls: 200, cleanup_polls: 50 }
    }
}

#[derive(Debug)]
pub enum ExecError {
    Transport(String),
    Command { name: String, result: ScsiResult },
    Mismatch(String),
    Timeout(&'static str),
    Cancelled,
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "{e}"),
            Self::Command { name, result } => write!(f, "{name}: status {:#04x} sense {:x}/{:02x}/{:02x}", result.status, result.sense.key, result.sense.asc, result.sense.ascq),
            Self::Mismatch(m) => write!(f, "mismatch: {m}"),
            Self::Timeout(w) => write!(f, "timed out waiting for {w}"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}
impl std::error::Error for ExecError {}

type Log<'a> = &'a mut dyn FnMut(&str);

fn exec(t: &mut dyn ScsiTransport, log: Log, name: &str, cdb: &[u8], dir: Direction, buf: &mut [u8], timeout_ms: u32) -> Result<ScsiResult, ExecError> {
    let r = t.execute(cdb, dir, buf, timeout_ms).map_err(|e| ExecError::Transport(e.0))?;
    let hexs: Vec<String> = cdb.iter().map(|b| format!("{b:02X}")).collect();
    if r.is_good() {
        log(&format!("[{name}] {} -> GOOD ({} bytes)", hexs.join(" "), r.transferred));
    } else {
        log(&format!("[{name}] {} -> status {:#04x} sense {:x}/{:02x}/{:02x}", hexs.join(" "), r.status, r.sense.key, r.sense.asc, r.sense.ascq));
    }
    Ok(r)
}

fn exec_good(t: &mut dyn ScsiTransport, log: Log, name: &str, cdb: &[u8], dir: Direction, buf: &mut [u8], timeout_ms: u32) -> Result<ScsiResult, ExecError> {
    let r = exec(t, log, name, cdb, dir, buf, timeout_ms)?;
    if r.is_good() { Ok(r) } else { Err(ExecError::Command { name: name.to_string(), result: r }) }
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// What the drive reports about itself and its media (read-only commands).
#[derive(Debug, Clone)]
pub struct DriveInfo {
    pub feature: LightScribeFeature,
    pub page31: Vec<u8>,
    pub page32: Vec<u8>,
    pub buffer_total: u32,
    pub buffer_free: u32,
}

/// Wait until TEST UNIT READY is GOOD, tolerating "becoming ready", one unit attention, and the
/// drive finishing an earlier job (02/04/08).
pub fn wait_ready(t: &mut dyn ScsiTransport, cfg: &Config, log: Log) -> Result<(), ExecError> {
    for _ in 0..cfg.tur_polls {
        let r = exec(t, log, "TUR", &cdb::test_unit_ready(), Direction::None, &mut [], 10_000)?;
        if r.is_good() {
            return Ok(());
        }
        if !(r.is_becoming_ready() || r.is_unit_attention() || r.is_writing()) {
            return Err(ExecError::Command { name: "TEST UNIT READY".into(), result: r });
        }
        std::thread::sleep(cfg.poll);
    }
    Err(ExecError::Timeout("the drive to become ready"))
}

/// The read-only preflight: ready, feature 0xFF33, pages 0x31/0x32, buffer capacity.
pub fn read_drive_info(t: &mut dyn ScsiTransport, cfg: &Config, log: Log) -> Result<DriveInfo, ExecError> {
    wait_ready(t, cfg, log)?;
    let mut buf = vec![0u8; 256];
    let r = exec_good(t, log, "GET CONFIGURATION ff33", &cdb::get_configuration(0xFF33), Direction::In, &mut buf, 10_000)?;
    let feature = LightScribeFeature::parse(&buf[..r.transferred]).ok_or_else(|| ExecError::Mismatch("drive has no LightScribe feature (0xFF33)".into()))?;
    let mut page = |code: u8, name: &str| -> Result<Vec<u8>, ExecError> {
        let mut b = vec![0u8; 256];
        let r = exec_good(t, log, name, &cdb::mode_sense10(code), Direction::In, &mut b, 10_000)?;
        find_page(&b[..r.transferred], code).map(|p| p.to_vec()).ok_or_else(|| ExecError::Mismatch(format!("page 0x{code:02X} missing")))
    };
    // the feature descriptor names the drive's labeling and media pages (0x31 and 0x32 on the 1040r)
    let page32 = page(feature.media_page, "MODE SENSE (media page)")?;
    let page31 = page(feature.labeling_page, "MODE SENSE (labeling page)")?;
    let mut b = [0u8; 12];
    let r = exec_good(t, log, "READ BUFFER CAPACITY", &cdb::read_buffer_capacity(), Direction::In, &mut b, 10_000)?;
    if r.transferred < 12 {
        return Err(ExecError::Mismatch("short READ BUFFER CAPACITY".into()));
    }
    Ok(DriveInfo { feature, page31, page32, buffer_total: be32(&b[4..8]), buffer_free: be32(&b[8..12]) })
}

fn wait_for_space(t: &mut dyn ScsiTransport, cfg: &Config, log: Log, need: u32, cancel: &AtomicBool) -> Result<(), ExecError> {
    let mut b = [0u8; 12];
    for _ in 0..cfg.space_polls {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExecError::Cancelled);
        }
        let r = exec(t, log, "RBC", &cdb::read_buffer_capacity(), Direction::In, &mut b, 10_000)?;
        if r.is_writing() {
            std::thread::sleep(cfg.poll);
            continue;
        }
        if !r.is_good() || r.transferred < 12 {
            return Err(ExecError::Command { name: "READ BUFFER CAPACITY".into(), result: r });
        }
        if be32(&b[8..12]) >= need {
            return Ok(());
        }
        std::thread::sleep(cfg.poll);
    }
    Err(ExecError::Timeout("buffer space"))
}

fn drain(t: &mut dyn ScsiTransport, cfg: &Config, log: Log, cancel: &AtomicBool) -> Result<(), ExecError> {
    let mut b = [0u8; 12];
    for _ in 0..cfg.drain_polls {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExecError::Cancelled);
        }
        std::thread::sleep(cfg.drain_interval);
        let r = exec(t, log, "RBC", &cdb::read_buffer_capacity(), Direction::In, &mut b, 10_000)?;
        if r.is_writing() {
            continue;
        }
        if !r.is_good() || r.transferred < 12 {
            return Err(ExecError::Command { name: "READ BUFFER CAPACITY".into(), result: r });
        }
        let mut s = [0u8; 18];
        if let Ok(q) = t.execute(&cdb::request_sense(), Direction::In, &mut s, 10_000) {
            if q.is_good() && s[15] & 0x80 != 0 {
                log(&format!("progress: current track {}", u16::from_be_bytes([s[16], s[17]])));
            }
        }
        if be32(&b[8..12]) == be32(&b[4..8]) {
            return Ok(());
        }
    }
    Err(ExecError::Timeout("the label buffer to drain"))
}

/// Send a cleanup command, repeating it while the drive answers 02/04/08 (busy writing).
fn exec_until_idle(t: &mut dyn ScsiTransport, cfg: &Config, log: Log, name: &str, cdb: &[u8], dir: Direction, data: &[u8], timeout_ms: u32) {
    for _ in 0..cfg.cleanup_polls {
        let mut buf = data.to_vec();
        match exec(t, log, name, cdb, dir, &mut buf, timeout_ms) {
            Ok(r) if r.is_writing() => std::thread::sleep(cfg.poll),
            _ => return,
        }
    }
    log(&format!("WARNING: {name} still refused as busy after {} tries", cfg.cleanup_polls));
}

/// Always run after a burn, successful or not: wait for the drive to go idle, STOP, restore the
/// original labeling page if a MODE SELECT was sent (and verify), ALLOW MEDIUM REMOVAL.
fn cleanup(t: &mut dyn ScsiTransport, cfg: &Config, log: Log, restore: Option<&[u8]>, finish: Option<&Step>) {
    exec_until_idle(t, cfg, log, "TUR(cleanup)", &cdb::test_unit_ready(), Direction::None, &[], 10_000);
    // A job that stopped before its last-track marker would leave the drive waiting for more data
    // (and the tray locked), so close it properly first.
    if let Some(Step { cdb, dir: Dir::Out(d), .. }) = finish {
        exec_until_idle(t, cfg, log, "last-track marker(cleanup)", cdb, Direction::Out, d, 30_000);
        exec_until_idle(t, cfg, log, "TUR(cleanup)", &cdb::test_unit_ready(), Direction::None, &[], 10_000);
    }
    exec_until_idle(t, cfg, log, "STOP(cleanup)", &cdb::start_stop(false), Direction::None, &[], 30_000);
    if let Some(list) = restore {
        let mut l = list.to_vec();
        let ok = exec(t, log, "MODE SELECT(restore)", &cdb::mode_select10(l.len() as u16), Direction::Out, &mut l, 30_000).map(|r| r.is_good()).unwrap_or(false);
        let mut b = vec![0u8; 256];
        let code = list[8] & 0x3F;
        let verified = exec(t, log, "MODE SENSE labeling page (verify restore)", &cdb::mode_sense10(code), Direction::In, &mut b, 10_000)
            .ok()
            .and_then(|r| find_page(&b[..r.transferred], code).map(|p| p == &list[8..]))
            .unwrap_or(false);
        if ok && verified {
            log(&format!("page 0x{code:02X} restored to the original"));
        } else {
            log(&format!("WARNING: could not confirm that page 0x{code:02X} was restored"));
        }
    }
    let _ = exec(t, log, "ALLOW(cleanup)", &cdb::prevent_removal(false), Direction::None, &mut [], 10_000);
}

/// Execute `steps` (from `plan::burn_steps`). `restore` is the original MODE SELECT list if the
/// plan changes the pitch; `expect_page` is the page 0x31 the drive must report after it.
pub fn run(
    t: &mut dyn ScsiTransport,
    cfg: &Config,
    steps: &[Step],
    restore: Option<&[u8]>,
    expect_page: Option<&[u8]>,
    cancel: &AtomicBool,
    log: Log,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<(), ExecError> {
    let mut pitch_sent = false;
    let total = steps.iter().filter(|s| s.id.starts_with("track-")).count();
    let mut sent = 0usize;
    let mut last_sent = false;
    let res = (|| -> Result<(), ExecError> {
        for s in steps {
            if cancel.load(Ordering::Relaxed) {
                return Err(ExecError::Cancelled);
            }
            match s.kind {
                Kind::Track => {
                    let Dir::Out(d) = &s.dir else { unreachable!() };
                    wait_for_space(t, cfg, log, d.len() as u32, cancel)?;
                    let mut data = d.clone();
                    exec_good(t, log, s.name, &s.cdb, Direction::Out, &mut data, 30_000)?;
                    if s.id == "last" {
                        last_sent = true;
                    }
                    if s.id.starts_with("track-") {
                        sent += 1;
                        progress(sent, total);
                    }
                }
                Kind::Drain => drain(t, cfg, log, cancel)?,
                Kind::Plain => match &s.dir {
                    Dir::None => {
                        if s.id == "start" {
                            // The drive starts writing by itself after ~11 tracks; a START that arrives
                            // later is answered 02/04/08 and can be ignored.
                            let r = exec(t, log, s.name, &s.cdb, Direction::None, &mut [], 60_000)?;
                            if !r.is_good() && !r.is_writing() {
                                return Err(ExecError::Command { name: s.name.to_string(), result: r });
                            }
                        } else {
                            exec_good(t, log, s.name, &s.cdb, Direction::None, &mut [], 30_000)?;
                        }
                    }
                    Dir::Out(d) => {
                        if s.id == "mode-select" {
                            pitch_sent = true;
                        }
                        let mut data = d.clone();
                        exec_good(t, log, s.name, &s.cdb, Direction::Out, &mut data, 30_000)?;
                    }
                    Dir::In(n) => {
                        let mut b = vec![0u8; *n];
                        let r = exec_good(t, log, s.name, &s.cdb, Direction::In, &mut b, 10_000)?;
                        if s.id == "mode-verify" {
                            let code = expect_page.map(|e| e[0] & 0x3F).unwrap_or(0x31);
                            let p = find_page(&b[..r.transferred], code);
                            if p != expect_page {
                                return Err(ExecError::Mismatch(format!("page 0x{code:02X} after MODE SELECT is not the expected one")));
                            }
                            log(&format!("page 0x{code:02X} now differs from the original only in the pitch bytes"));
                        }
                    }
                },
            }
        }
        Ok(())
    })();
    let finish = if sent > 0 && !last_sent { steps.iter().find(|x| x.id == "last") } else { None };
    cleanup(t, cfg, log, if pitch_sent { restore } else { None }, finish);
    res
}
