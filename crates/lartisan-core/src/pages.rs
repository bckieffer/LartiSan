//! Decoding of the GET CONFIGURATION feature 0xFF33 descriptor and mode pages 0x31 / 0x32.

/// Find mode page `code` in a MODE SENSE(10) response (block descriptors skipped).
pub fn find_page(resp: &[u8], code: u8) -> Option<&[u8]> {
    if resp.len() < 8 {
        return None;
    }
    let total = (u16::from_be_bytes([resp[0], resp[1]]) as usize + 2).min(resp.len());
    let mut off = 8 + u16::from_be_bytes([resp[6], resp[7]]) as usize;
    while off + 2 <= total {
        let pg = resp[off] & 0x3F;
        let len = resp[off + 1] as usize + 2;
        if pg == code {
            return resp.get(off..off + len);
        }
        off += len;
    }
    None
}

/// The drive's LightScribe feature (code 0xFF33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightScribeFeature {
    pub version: u8,
    /// "Current" bit: set when LightScribe media is loaded label side down.
    pub current: bool,
    pub print_opcode: u8,
    pub labeling_page: u8,
    pub media_page: u8,
    pub print_cdb1: u8,
    pub raw: [u8; 8],
}

impl LightScribeFeature {
    /// Parse a GET CONFIGURATION response and return feature 0xFF33 if present.
    pub fn parse(resp: &[u8]) -> Option<Self> {
        if resp.len() < 8 {
            return None;
        }
        let total = (u32::from_be_bytes([resp[0], resp[1], resp[2], resp[3]]) as usize + 4).min(resp.len());
        let mut off = 8;
        while off + 4 <= total {
            let code = u16::from_be_bytes([resp[off], resp[off + 1]]);
            let add = resp[off + 3] as usize;
            if code == 0xFF33 && add >= 4 && off + 8 <= resp.len() {
                let d = &resp[off..off + 8];
                let mut raw = [0u8; 8];
                raw.copy_from_slice(d);
                return Some(Self {
                    version: (d[2] & 0x3C) >> 2,
                    current: d[2] & 1 != 0,
                    print_opcode: d[4],
                    labeling_page: d[5],
                    media_page: d[6],
                    print_cdb1: d[7],
                    raw,
                });
            }
            off += 4 + add;
        }
        None
    }
}

/// Page 0x31: drive capabilities and current labeling parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveCaps {
    pub page_version: u16,
    pub drive_id: u16,
    pub focus_offset_um: i8,
    pub max_read_power_dmw: u8,
    pub write_power_units: u8,
    pub mark_pitch_nm: u32,
    pub track_pitch_nm: u32,
    pub spindle_um_s: u32,
    pub start_radius_um: u32,
    pub inner_radius_um: u32,
    pub outer_radius_um: u32,
    pub flags: u8,
}

fn u24(b: &[u8]) -> u32 {
    ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32
}

impl DriveCaps {
    pub const PAGE: u8 = 0x31;
    pub const LEN: usize = 56;

    /// Parse the labeling page. Accepts the long layout (length byte 0x36, page version 4 and
    /// later) and the short one (0x2A); every field used here exists in both. Anything else is
    /// refused so an unknown layout is never guessed at.
    pub fn parse(page: &[u8]) -> Option<Self> {
        if !matches!(page.get(1), Some(0x36) | Some(0x2A)) || page.len() < page[1] as usize + 2 || page.len() < 40 {
            return None;
        }
        Some(Self {
            page_version: page[2] as u16,
            drive_id: u16::from_be_bytes([page[3], page[4]]),
            focus_offset_um: page[8] as i8,
            max_read_power_dmw: page[9],
            write_power_units: page[10],
            mark_pitch_nm: u24(&page[11..14]),
            track_pitch_nm: u24(&page[14..17]),
            spindle_um_s: u24(&page[17..20]),
            start_radius_um: u16::from_be_bytes([page[20], page[21]]) as u32,
            inner_radius_um: u16::from_be_bytes([page[36], page[37]]) as u32,
            outer_radius_um: u16::from_be_bytes([page[38], page[39]]) as u32,
            flags: page[7] & 0x0F,
        })
    }

    /// Write power in mW, using the drive's own conversion from HP's table (units = mW * slope +
    /// offset / 1000; DriveID 169: slope 1.744). `None` for a DriveID that is not in the table, in
    /// which case the disc's power window cannot be checked.
    pub fn write_power_mw(&self) -> Option<f64> {
        let (slope, offset) = crate::media_profile::power_conversion(self.drive_id)?;
        Some((self.write_power_units as f64 - offset) / slope)
    }
}

/// A copy of page 0x31 with only the track-pitch bytes (14-16) replaced. This is the single
/// MODE SELECT edit the tool ever makes.
pub fn with_track_pitch(page: &[u8], pitch_nm: u32) -> Vec<u8> {
    let mut p = page.to_vec();
    p[14] = (pitch_nm >> 16) as u8;
    p[15] = (pitch_nm >> 8) as u8;
    p[16] = pitch_nm as u8;
    p
}

/// MODE SELECT parameter list: 8-byte header (mode data length = 6 + page length, medium type
/// 0x70) + page. For the 56-byte page that is 62 (0x3E); for the short 44-byte page, 50.
pub fn mode_select_list(page: &[u8]) -> Vec<u8> {
    let len = (6 + page.len()) as u16;
    let mut v = vec![(len >> 8) as u8, len as u8, 0x70, 0, 0, 0, 0, 0];
    v.extend_from_slice(page);
    v
}

/// Page 0x32: media parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaParams {
    pub version: u8,
    pub media_present: bool,
    /// Page 0x32 byte 3 bit 0: the outer media ID field is valid.
    pub outer_id_valid: bool,
    pub spokes_per_rev: u16,
    pub outer_id: [u8; 15],
}

impl MediaParams {
    pub const PAGE: u8 = 0x32;
    pub const LEN: usize = 70;

    pub fn parse(page: &[u8]) -> Option<Self> {
        if page.len() < Self::LEN || page[1] != 0x44 {
            return None;
        }
        let mut outer_id = [0u8; 15];
        outer_id.copy_from_slice(&page[8..23]);
        Some(Self {
            version: page[2],
            media_present: page[3] & 0x08 != 0,
            outer_id_valid: page[3] & 0x01 != 0,
            spokes_per_rev: u16::from_be_bytes([page[4], page[5]]),
            outer_id,
        })
    }
}

/// Raw outer media ID of the Memorex LightScribe CD-R seen on every probed disc (decodes to
/// imaging parameter 104). Kept as a test vector; the CLI now decodes IDs with `media_id`.
pub const KNOWN_MEDIA_ID_104: [u8; 15] = [0x07, 0x1d, 0x6b, 0x61, 0x1d, 0x1b, 0x65, 0x27, 0x25, 0x0f, 0x0f, 0x66, 0x2a, 0x07, 0x27];
