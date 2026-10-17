//! The ordered command sequence for a burn, independent of any transport.

use crate::cdb;
use crate::pages::LightScribeFeature;
use crate::track::{self, Track};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dir {
    None,
    In(usize),
    Out(Vec<u8>),
}

/// What a step means to the executor (it needs different handling for polling and tracks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    /// Needs `payload.len()` bytes free in the drive buffer first (READ BUFFER CAPACITY).
    Track,
    /// After the last-track marker: poll buffer + progress until empty.
    Drain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub id: String,
    pub name: &'static str,
    pub cdb: Vec<u8>,
    pub dir: Dir,
    pub kind: Kind,
}

fn step(id: &str, name: &'static str, cdb: Vec<u8>, dir: Dir, kind: Kind) -> Step {
    Step { id: id.into(), name, cdb, dir, kind }
}

/// Steps that run after the preflight (which reads feature, pages and buffer, writing nothing):
/// prevent removal, [optional MODE SELECT], start, tracks, last marker, drain, stop, allow.
///
/// With `prefill`, every track is queued in the drive's buffer before START STOP UNIT, so the drive
/// starts writing with the whole job in its buffer instead of being fed batch by batch (used to
/// test whether feed stalls leave marks on the label). The caller must make sure the data fits.
pub fn burn_steps(feat: &LightScribeFeature, tracks: &[Track], pitch_change: Option<&[u8]>, prefill: bool) -> Vec<Step> {
    let mut s = vec![step("prevent", "PREVENT MEDIUM REMOVAL", cdb::prevent_removal(true), Dir::None, Kind::Plain)];
    if let Some(list) = pitch_change {
        s.push(step("mode-select", "MODE SELECT(10): track pitch only", cdb::mode_select10(list.len() as u16), Dir::Out(list.to_vec()), Kind::Plain));
        s.push(step("mode-verify", "MODE SENSE(10) page 0x31 (verify)", cdb::mode_sense10(feat.labeling_page), Dir::In(0x100), Kind::Plain));
    }
    let start = step("start", "START STOP UNIT (start)", cdb::start_stop(true), Dir::None, Kind::Plain);
    if !prefill {
        s.push(start.clone());
    }
    s.push(step("rbc", "READ BUFFER CAPACITY", cdb::read_buffer_capacity(), Dir::In(12), Kind::Plain));
    for t in tracks {
        let p = t.payload();
        s.push(step(&format!("track-{}", t.n), "PRINT: track", cdb::print(feat.print_opcode, feat.print_cdb1, p.len() as u32), Dir::Out(p), Kind::Track));
    }
    if prefill {
        s.push(start);
    }
    let last = track::last_track_payload();
    s.push(step("last", "PRINT: last-track marker", cdb::print(feat.print_opcode, feat.print_cdb1, last.len() as u32), Dir::Out(last), Kind::Track));
    s.push(step("drain", "poll READ BUFFER CAPACITY + REQUEST SENSE", cdb::read_buffer_capacity(), Dir::In(12), Kind::Drain));
    s.push(step("tur", "TEST UNIT READY", cdb::test_unit_ready(), Dir::None, Kind::Plain));
    s.push(step("stop", "START STOP UNIT (stop)", cdb::start_stop(false), Dir::None, Kind::Plain));
    s.push(step("allow", "ALLOW MEDIUM REMOVAL", cdb::prevent_removal(false), Dir::None, Kind::Plain));
    s
}
