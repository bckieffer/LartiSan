//! SCSI transports, safety guard, drive simulator and burn executor.

pub mod executor;
pub mod guard;
pub mod sim;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    None,
    In,
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sense {
    pub key: u8,
    pub asc: u8,
    pub ascq: u8,
}

/// Result of one command. `status` is the SCSI status byte (0 = GOOD, 2 = CHECK CONDITION).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScsiResult {
    pub status: u8,
    pub sense: Sense,
    pub transferred: usize,
}

impl ScsiResult {
    pub fn good(transferred: usize) -> Self {
        Self { status: 0, sense: Sense::default(), transferred }
    }
    pub fn check(key: u8, asc: u8, ascq: u8) -> Self {
        Self { status: 2, sense: Sense { key, asc, ascq }, transferred: 0 }
    }
    pub fn is_good(&self) -> bool {
        self.status == 0
    }
    /// 02/04/08: the drive is writing (LONG WRITE IN PROGRESS). Not an error: wait and retry.
    pub fn is_writing(&self) -> bool {
        self.status == 2 && (self.sense.key, self.sense.asc, self.sense.ascq) == (2, 4, 8)
    }
    /// 02/04/01: becoming ready.
    pub fn is_becoming_ready(&self) -> bool {
        self.status == 2 && (self.sense.key, self.sense.asc, self.sense.ascq) == (2, 4, 1)
    }
    /// 06/28/00: media may have changed (reported once after a reload).
    pub fn is_unit_attention(&self) -> bool {
        self.status == 2 && self.sense.key == 6
    }
}

#[derive(Debug)]
pub struct TransportError(pub String);

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "transport error: {}", self.0)
    }
}
impl std::error::Error for TransportError {}

/// Executes one SCSI command. `data` is read for `Out` and filled for `In`.
pub trait ScsiTransport {
    fn execute(&mut self, cdb: &[u8], dir: Direction, data: &mut [u8], timeout_ms: u32) -> Result<ScsiResult, TransportError>;
}

impl<T: ScsiTransport + ?Sized> ScsiTransport for Box<T> {
    fn execute(&mut self, cdb: &[u8], dir: Direction, data: &mut [u8], timeout_ms: u32) -> Result<ScsiResult, TransportError> {
        (**self).execute(cdb, dir, data, timeout_ms)
    }
}

/// Open the first optical drive on this OS with exclusive access.
pub fn open_default() -> Result<Box<dyn ScsiTransport>, TransportError> {
    #[cfg(target_os = "macos")]
    return Ok(Box::new(macos::MacTransport::open()?));
    #[cfg(target_os = "linux")]
    return Ok(Box::new(linux::SgTransport::open_first()?));
    #[cfg(target_os = "windows")]
    return Ok(Box::new(windows::WinTransport::open_first()?));
    #[allow(unreachable_code)]
    Err(TransportError("no SCSI transport for this operating system".into()))
}
