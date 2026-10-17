//! SCSI/MMC command descriptor blocks used by the LightScribe flow.

pub fn test_unit_ready() -> Vec<u8> {
    vec![0x00, 0, 0, 0, 0, 0]
}

pub fn request_sense() -> Vec<u8> {
    vec![0x03, 0, 0, 0, 0x12, 0]
}

/// GET CONFIGURATION, RT=2, starting feature `feature`, allocation length 0x100.
pub fn get_configuration(feature: u16) -> Vec<u8> {
    vec![0x46, 0x02, (feature >> 8) as u8, feature as u8, 0, 0, 0, 0x01, 0x00, 0]
}

/// MODE SENSE(10), DBD=1, page control = current, allocation length 255.
pub fn mode_sense10(page: u8) -> Vec<u8> {
    vec![0x5A, 0x08, page, 0, 0, 0, 0, 0, 0xFF, 0]
}

/// MODE SENSE(10) for the changeable-values mask of `page`.
pub fn mode_sense10_changeable(page: u8) -> Vec<u8> {
    vec![0x5A, 0x08, 0x40 | page, 0, 0, 0, 0, 0, 0xFF, 0]
}

/// MODE SELECT(10), PF=1, SP=0; the parameter list length is a big-endian u16 in bytes 7-8.
pub fn mode_select10(len: u16) -> Vec<u8> {
    vec![0x55, 0x10, 0, 0, 0, 0, 0, (len >> 8) as u8, len as u8, 0]
}

pub fn prevent_removal(prevent: bool) -> Vec<u8> {
    vec![0x1E, 0, 0, 0, prevent as u8, 0]
}

/// START STOP UNIT without load/eject (the LightScribe session start/stop).
pub fn start_stop(start: bool) -> Vec<u8> {
    vec![0x1B, 0, 0, 0, start as u8, 0]
}

pub fn read_buffer_capacity() -> Vec<u8> {
    vec![0x5C, 0, 0, 0, 0, 0, 0, 0, 12, 0]
}

/// The vendor print command. `opcode` and `cdb1` come from feature 0xFF33 (0xFC / 0x02 on the 1040r).
pub fn print(opcode: u8, cdb1: u8, len: u32) -> Vec<u8> {
    let l = len.to_be_bytes();
    vec![opcode, cdb1, 0, 0, 0, l[0], l[1], l[2], l[3], 0]
}
