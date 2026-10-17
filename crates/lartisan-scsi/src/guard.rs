//! Safety wrapper around any transport: opcode allowlist, arming, and a full command log.

use crate::{Direction, ScsiResult, ScsiTransport, TransportError};
use lartisan_core::{opcode_allowed, PRINT_OPCODE};

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub cdb: Vec<u8>,
    pub dir: Direction,
    pub len: usize,
    pub result: Result<ScsiResult, String>,
}

/// Commands that can change drive or disc state. They are refused until `arm()` is called.
fn needs_arming(op: u8) -> bool {
    op == PRINT_OPCODE || op == 0x55
}

pub struct Guarded<T: ScsiTransport> {
    inner: T,
    armed: bool,
    pub log: Vec<LogEntry>,
}

impl<T: ScsiTransport> Guarded<T> {
    pub fn new(inner: T) -> Self {
        Self { inner, armed: false, log: Vec::new() }
    }
    pub fn arm(&mut self) {
        self.armed = true;
    }
    pub fn disarm(&mut self) {
        self.armed = false;
    }
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: ScsiTransport> ScsiTransport for Guarded<T> {
    fn execute(&mut self, cdb: &[u8], dir: Direction, data: &mut [u8], timeout_ms: u32) -> Result<ScsiResult, TransportError> {
        let op = *cdb.first().ok_or_else(|| TransportError("empty CDB".into()))?;
        if !opcode_allowed(op) {
            return Err(TransportError(format!("REFUSED: opcode 0x{op:02X} is not on the allowlist")));
        }
        if needs_arming(op) && !self.armed {
            return Err(TransportError(format!("REFUSED: opcode 0x{op:02X} needs the burn to be armed")));
        }
        let r = self.inner.execute(cdb, dir, data, timeout_ms);
        self.log.push(LogEntry { cdb: cdb.to_vec(), dir, len: data.len(), result: r.as_ref().map(|x| *x).map_err(|e| e.0.clone()) });
        r
    }
}
