//! Windows transport: `DeviceIoControl(IOCTL_SCSI_PASS_THROUGH_DIRECT)` on `\\.\X:`.
//! Needs an elevated (administrator) process for the vendor commands.
//! NOT yet tested on real hardware.

use crate::{Direction, ScsiResult, ScsiTransport, Sense, TransportError};
use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, GetDriveTypeW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::Storage::IscsiDisc::{IOCTL_SCSI_PASS_THROUGH_DIRECT, SCSI_IOCTL_DATA_IN, SCSI_IOCTL_DATA_OUT, SCSI_IOCTL_DATA_UNSPECIFIED, SCSI_PASS_THROUGH_DIRECT};
use windows_sys::Win32::System::IO::DeviceIoControl;

const DRIVE_CDROM: u32 = 5;

#[repr(C)]
struct Sptdwb {
    sptd: SCSI_PASS_THROUGH_DIRECT,
    filler: u32,
    sense: [u8; 32],
}

pub struct WinTransport {
    h: HANDLE,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl WinTransport {
    pub fn open(letter: char) -> Result<Self, TransportError> {
        let path = wide(&format!("\\\\.\\{}:", letter.to_ascii_uppercase()));
        let h = unsafe { CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
        if h == INVALID_HANDLE_VALUE {
            return Err(TransportError(format!("cannot open drive {letter}: ({}); run from an administrator prompt", std::io::Error::last_os_error())));
        }
        Ok(Self { h })
    }

    /// Open the first optical drive letter that answers INQUIRY as a CD/DVD device.
    pub fn open_first() -> Result<Self, TransportError> {
        let mut last = TransportError("no optical drive found".into());
        for l in 'D'..='Z' {
            let root = wide(&format!("{l}:\\"));
            if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_CDROM {
                continue;
            }
            match Self::open(l) {
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

impl Drop for WinTransport {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.h) };
    }
}

impl ScsiTransport for WinTransport {
    fn execute(&mut self, cdb: &[u8], dir: Direction, data: &mut [u8], timeout_ms: u32) -> Result<ScsiResult, TransportError> {
        let mut b: Sptdwb = unsafe { std::mem::zeroed() };
        b.sptd.Length = std::mem::size_of::<SCSI_PASS_THROUGH_DIRECT>() as u16;
        b.sptd.CdbLength = cdb.len() as u8;
        b.sptd.SenseInfoLength = b.sense.len() as u8;
        b.sptd.DataIn = match dir {
            Direction::None => SCSI_IOCTL_DATA_UNSPECIFIED,
            Direction::In => SCSI_IOCTL_DATA_IN,
            Direction::Out => SCSI_IOCTL_DATA_OUT,
        } as u8;
        b.sptd.DataTransferLength = if dir == Direction::None { 0 } else { data.len() as u32 };
        b.sptd.TimeOutValue = (timeout_ms / 1000).max(1);
        b.sptd.DataBuffer = if dir == Direction::None { std::ptr::null_mut() } else { data.as_mut_ptr() as *mut _ };
        b.sptd.SenseInfoOffset = std::mem::offset_of!(Sptdwb, sense) as u32;
        b.sptd.Cdb[..cdb.len()].copy_from_slice(cdb);
        let size = std::mem::size_of::<Sptdwb>() as u32;
        let mut returned = 0u32;
        let ok = unsafe { DeviceIoControl(self.h, IOCTL_SCSI_PASS_THROUGH_DIRECT, &mut b as *mut _ as *mut _, size, &mut b as *mut _ as *mut _, size, &mut returned, std::ptr::null_mut()) };
        if ok == 0 {
            return Err(TransportError(format!("IOCTL_SCSI_PASS_THROUGH_DIRECT failed: {}", std::io::Error::last_os_error())));
        }
        let s = if b.sptd.SenseInfoLength >= 14 { Sense { key: b.sense[2] & 0x0F, asc: b.sense[12], ascq: b.sense[13] } } else { Sense::default() };
        Ok(ScsiResult { status: b.sptd.ScsiStatus, sense: s, transferred: b.sptd.DataTransferLength as usize })
    }
}
