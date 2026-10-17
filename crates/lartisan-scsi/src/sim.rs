//! A simulated 1040r for tests and dry runs. Models what the real drive did on the bench:
//! commands are refused with 02/04/08 while it writes, the buffer drains afterwards, and
//! MODE SELECT updates page 0x31.

use crate::{Direction, ScsiResult, ScsiTransport, TransportError};
use lartisan_core::PRINT_OPCODE;

pub struct SimDrive {
    pub feature: [u8; 8],
    pub page31: Vec<u8>,
    pub page32: Vec<u8>,
    pub buffer_total: u32,
    pub buffer_free: u32,
    pub started: bool,
    pub locked: bool,
    pub writing: bool,
    pub polls_left: u32,
    /// Start "writing" (busy) after this many tracks have been received, as the real drive did.
    pub busy_after_tracks: usize,
    /// How many busy polls each writing phase lasts.
    pub busy_polls: u32,
    /// Fail the print command carrying this (0-based) track index with a write error.
    pub fail_at_track: Option<usize>,
    pub unit_attention_once: bool,
    /// Answer TEST UNIT READY GOOD even while writing (seen on the bench: STOP was still refused).
    pub tur_ready_while_writing: bool,
    pub ready_polls: u32,
    pub tracks: Vec<(u16, u16, bool)>, // (track number, dots, last)
    pub commands: Vec<Vec<u8>>,
}

impl SimDrive {
    /// Build from probe captures (the bytes of `testdata/`).
    pub fn new(feature: [u8; 8], page31: Vec<u8>, page32: Vec<u8>) -> Self {
        Self {
            feature,
            page31,
            page32,
            buffer_total: 1_611_920,
            buffer_free: 1_611_920,
            started: false,
            locked: false,
            writing: false,
            polls_left: 0,
            busy_after_tracks: 11,
            busy_polls: 5,
            fail_at_track: None,
            unit_attention_once: false,
            tur_ready_while_writing: false,
            ready_polls: 0,
            tracks: vec![],
            commands: vec![],
        }
    }

    fn tick(&mut self) {
        if self.writing {
            if self.polls_left > 0 {
                self.polls_left -= 1;
            }
            if self.polls_left == 0 {
                self.writing = false;
                self.buffer_free = self.buffer_total;
            }
        }
    }

    fn out(buf: &mut [u8], bytes: &[u8]) -> usize {
        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        n
    }
}

impl ScsiTransport for SimDrive {
    fn execute(&mut self, cdb: &[u8], dir: Direction, data: &mut [u8], _timeout_ms: u32) -> Result<ScsiResult, TransportError> {
        self.commands.push(cdb.to_vec());
        let op = cdb[0];
        // While writing, everything except INQUIRY-class reads is refused, as on the real drive.
        let busy_op = matches!(op, 0x5C | 0x1B | PRINT_OPCODE | 0x55) || (op == 0x00 && !self.tur_ready_while_writing);
        if self.writing && busy_op {
            self.tick();
            if self.writing {
                return Ok(ScsiResult::check(2, 4, 8));
            }
        }
        Ok(match op {
            0x00 => {
                if self.ready_polls > 0 {
                    self.ready_polls -= 1;
                    ScsiResult::check(2, 4, 1)
                } else if self.unit_attention_once {
                    self.unit_attention_once = false;
                    ScsiResult::check(6, 0x28, 0)
                } else {
                    ScsiResult::good(0)
                }
            }
            0x46 => {
                let mut r = vec![0, 0, 0, 12, 0, 0, 0xFF, 0xFF];
                r.extend_from_slice(&self.feature);
                ScsiResult::good(Self::out(data, &r))
            }
            0x5A => {
                let code = cdb[2] & 0x3F;
                let changeable = cdb[2] >> 6 == 1;
                // the page codes come from the feature descriptor (bytes 5 and 6), as on a real drive
                let (labeling, media) = (self.feature[5], self.feature[6]);
                let mut page = if code == labeling {
                    self.page31.clone()
                } else if code == media {
                    self.page32.clone()
                } else {
                    return Ok(ScsiResult::check(5, 0x24, 0));
                };
                if changeable && code == labeling {
                    let len = page[1];
                    page.iter_mut().for_each(|b| *b = 0);
                    page[0] = labeling;
                    page[1] = len;
                    page[14..17].fill(0xFF);
                }
                let mut r = vec![0u8; 8];
                let len = (page.len() + 6) as u16;
                r[0..2].copy_from_slice(&len.to_be_bytes());
                r.extend_from_slice(&page);
                ScsiResult::good(Self::out(data, &r))
            }
            0x55 => {
                // header (8) + the labeling page in the layout the drive reported (56 or 44 bytes)
                if data.len() != 8 + self.page31.len() || data[8] & 0x3F != self.feature[5] || data[9] as usize + 2 != self.page31.len() {
                    return Ok(ScsiResult::check(5, 0x26, 0));
                }
                self.page31 = data[8..].to_vec();
                ScsiResult::good(data.len())
            }
            0x1E => {
                self.locked = cdb[4] & 1 != 0;
                ScsiResult::good(0)
            }
            0x1B => {
                self.started = cdb[4] & 1 != 0;
                ScsiResult::good(0)
            }
            0x5C => {
                let mut r = vec![0u8; 12];
                r[4..8].copy_from_slice(&self.buffer_total.to_be_bytes());
                r[8..12].copy_from_slice(&self.buffer_free.to_be_bytes());
                ScsiResult::good(Self::out(data, &r))
            }
            0x03 => {
                let mut r = vec![0u8; 18];
                r[0] = 0x70;
                r[15] = 0x80;
                if let Some(t) = self.tracks.last() {
                    r[16..18].copy_from_slice(&t.0.to_be_bytes());
                }
                ScsiResult::good(Self::out(data, &r))
            }
            PRINT_OPCODE => {
                if dir != Direction::Out || data.len() < 16 {
                    return Ok(ScsiResult::check(5, 0x24, 0));
                }
                if data.len() as u32 > self.buffer_free {
                    return Ok(ScsiResult::check(5, 0x24, 0));
                }
                let (n, dots, last) = (u16::from_be_bytes([data[0], data[1]]), u16::from_be_bytes([data[14], data[15]]), data[3] & 0x80 != 0);
                if Some(self.tracks.len()) == self.fail_at_track {
                    self.fail_at_track = None; // a one-off write error
                    return Ok(ScsiResult::check(3, 0x0C, 0));
                }
                self.tracks.push((n, dots, last));
                self.buffer_free -= data.len() as u32;
                if last || (!last && self.tracks.len() % self.busy_after_tracks.max(1) == 0) {
                    self.writing = true;
                    self.polls_left = self.busy_polls;
                }
                ScsiResult::good(data.len())
            }
            _ => ScsiResult::check(5, 0x20, 0),
        })
    }
}
