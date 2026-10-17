//! Linux transport: `ioctl(SG_IO)` on a CD/DVD block device (e.g. /dev/sr0).
//! The user needs read/write access to the device (usually the `cdrom` group), and no volume may
//! be mounted from it (the O_EXCL open fails otherwise).
//! NOT yet tested on real hardware.

use crate::{Direction, ScsiResult, ScsiTransport, Sense, TransportError};
use libc::{c_int, c_void};
use std::ffi::CString;

const SG_IO: libc::c_ulong = 0x2285;
const SG_DXFER_NONE: c_int = -1;
const SG_DXFER_TO_DEV: c_int = -2;
const SG_DXFER_FROM_DEV: c_int = -3;

#[repr(C)]
struct SgIoHdr {
    interface_id: c_int,
    dxfer_direction: c_int,
    cmd_len: u8,
    mx_sb_len: u8,
    iovec_count: u16,
    dxfer_len: u32,
    dxferp: *mut c_void,
    cmdp: *mut u8,
    sbp: *mut u8,
    timeout: u32,
    flags: u32,
    pack_id: c_int,
    usr_ptr: *mut c_void,
    status: u8,
    masked_status: u8,
    msg_status: u8,
    sb_len_wr: u8,
    host_status: u16,
    driver_status: u16,
    resid: c_int,
    duration: u32,
    info: u32,
}

pub struct SgTransport {
    fd: c_int,
}

impl SgTransport {
    pub fn open(path: &str) -> Result<Self, TransportError> {
        let c = CString::new(path).map_err(|e| TransportError(e.to_string()))?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK | libc::O_EXCL) };
        if fd < 0 {
            return Err(TransportError(format!("cannot open {path}: {} (unmount the disc; add yourself to the cdrom group)", std::io::Error::last_os_error())));
        }
        Ok(Self { fd })
    }

    /// Open the first /dev/srN that answers INQUIRY as a CD/DVD device (peripheral type 5).
    pub fn open_first() -> Result<Self, TransportError> {
        let mut last = TransportError("no /dev/sr* device found".into());
        for i in 0..16 {
            let path = format!("/dev/sr{i}");
            if !std::path::Path::new(&path).exists() {
                continue;
            }
            match Self::open(&path) {
                Ok(mut t) => {
                    let mut inq = [0u8; 36];
                    if let Ok(r) = t.execute(&[0x12, 0, 0, 0, 36, 0], Direction::In, &mut inq, 5000) {
                        if r.is_good() && inq[0] & 0x1F == 5 {
                            return Ok(t);
                        }
                    }
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

impl Drop for SgTransport {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

impl ScsiTransport for SgTransport {
    fn execute(&mut self, cdb: &[u8], dir: Direction, data: &mut [u8], timeout_ms: u32) -> Result<ScsiResult, TransportError> {
        let mut sense = [0u8; 32];
        let mut cdb_copy = cdb.to_vec();
        let mut hdr = SgIoHdr {
            interface_id: b'S' as c_int,
            dxfer_direction: match dir {
                Direction::None => SG_DXFER_NONE,
                Direction::In => SG_DXFER_FROM_DEV,
                Direction::Out => SG_DXFER_TO_DEV,
            },
            cmd_len: cdb.len() as u8,
            mx_sb_len: sense.len() as u8,
            iovec_count: 0,
            dxfer_len: if dir == Direction::None { 0 } else { data.len() as u32 },
            dxferp: if dir == Direction::None { std::ptr::null_mut() } else { data.as_mut_ptr() as *mut c_void },
            cmdp: cdb_copy.as_mut_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: timeout_ms,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        };
        let rc = unsafe { libc::ioctl(self.fd, SG_IO, &mut hdr as *mut SgIoHdr) };
        if rc < 0 {
            return Err(TransportError(format!("SG_IO failed: {}", std::io::Error::last_os_error())));
        }
        if hdr.host_status != 0 {
            return Err(TransportError(format!("SG_IO host status {:#x}", hdr.host_status)));
        }
        let s = if hdr.sb_len_wr >= 14 { Sense { key: sense[2] & 0x0F, asc: sense[12], ascq: sense[13] } } else { Sense::default() };
        Ok(ScsiResult { status: hdr.status, sense: s, transferred: (hdr.dxfer_len as i64 - hdr.resid as i64).max(0) as usize })
    }
}
