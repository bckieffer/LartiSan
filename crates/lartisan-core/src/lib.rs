//! Portable LightScribe core: command builders, mode-page codecs, geometry, rasterizer and
//! burn planning. Nothing here touches the operating system; transports live elsewhere.
//!
//! Facts in this crate come from protocol notes and calibration burns that are not part of this
//! repository (see `docs/ARCHITECTURE.md` for the summary). Items marked "calibrated" were
//! measured on an HP 1040r (DriveID 169) with Memorex media.

pub mod cdb;
pub mod geometry;
pub mod media_id;
pub mod media_profile;
pub mod pages;
pub mod plan;
pub mod raster;
pub mod track;

/// The vendor print opcode. Feature 0xFF33 reports it per drive; only this value is supported,
/// and a drive reporting another one is refused before anything is written.
pub const PRINT_OPCODE: u8 = 0xFC;

/// Opcodes the burner may ever send. Transports must refuse anything else.
pub const ALLOWED_OPCODES: [u8; 10] = [0x00, 0x03, 0x12, 0x1B, 0x1E, 0x46, 0x55, 0x5A, 0x5C, PRINT_OPCODE];

pub fn opcode_allowed(op: u8) -> bool {
    ALLOWED_OPCODES.contains(&op)
}
