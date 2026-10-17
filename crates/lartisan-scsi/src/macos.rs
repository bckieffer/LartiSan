//! macOS transport: IOKit SCSITask user client through a small C shim (csrc/macos_shim.c).
//! The drive must not have a mounted volume; label-side-down LightScribe discs have none.

use crate::{Direction, ScsiResult, ScsiTransport, Sense, TransportError};
use std::ffi::c_void;
use std::os::raw::{c_char, c_int};

extern "C" {
    fn ls_open(err: *mut c_char, errlen: usize) -> *mut c_void;
    fn ls_close(d: *mut c_void);
    fn ls_exec(
        d: *mut c_void,
        cdb: *const u8,
        cdblen: u8,
        dir: c_int,
        buf: *mut u8,
        buflen: u32,
        timeout_ms: u32,
        status: *mut u8,
        sense: *mut u8,
        transferred: *mut u64,
    ) -> c_int;
}

pub struct MacTransport {
    dev: *mut c_void,
}

impl MacTransport {
    pub fn open() -> Result<Self, TransportError> {
        let mut err = [0 as c_char; 256];
        let dev = unsafe { ls_open(err.as_mut_ptr(), err.len()) };
        if dev.is_null() {
            let msg = unsafe { std::ffi::CStr::from_ptr(err.as_ptr()) }.to_string_lossy().into_owned();
            return Err(TransportError(msg));
        }
        Ok(Self { dev })
    }
}

impl Drop for MacTransport {
    fn drop(&mut self) {
        unsafe { ls_close(self.dev) }
    }
}

impl ScsiTransport for MacTransport {
    fn execute(&mut self, cdb: &[u8], dir: Direction, data: &mut [u8], timeout_ms: u32) -> Result<ScsiResult, TransportError> {
        let d = match dir {
            Direction::None => 0,
            Direction::In => 1,
            Direction::Out => 2,
        };
        let (mut status, mut sense, mut got) = (0u8, [0u8; 18], 0u64);
        let kr = unsafe { ls_exec(self.dev, cdb.as_ptr(), cdb.len() as u8, d, data.as_mut_ptr(), data.len() as u32, timeout_ms, &mut status, sense.as_mut_ptr(), &mut got) };
        if kr != 0 {
            return Err(TransportError(format!("IOKit error {kr:#010x}")));
        }
        Ok(ScsiResult { status, sense: Sense { key: sense[2] & 0x0F, asc: sense[12], ascq: sense[13] }, transferred: got as usize })
    }
}
