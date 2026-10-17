//! Track header and payload (the data-out of one print command).

pub const HEADER_LEN: usize = 16;

/// 16-byte track header, big-endian.
/// [0..2] track number, [3] bit 7 = last track, [5..7] track shift (spokes),
/// [12..14] spiral length (0 for circular), [14..16] dots in this track.
pub fn header(track_no: u16, last: bool, dots: u16, shift_spokes: u16) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[0..2].copy_from_slice(&track_no.to_be_bytes());
    if last {
        h[3] = 0x80;
    }
    h[5..7].copy_from_slice(&shift_spokes.to_be_bytes());
    h[14..16].copy_from_slice(&dots.to_be_bytes());
    h
}

/// Bytes needed for `dots` bits, padded to 32-bit words.
pub fn data_len(dots: u32) -> usize {
    ((dots as usize + 31) / 32) * 4
}

/// Bit-packed track data: dot n -> byte n>>3, bit n&7 (LSB first); 1 = mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bits {
    pub dots: u32,
    pub bytes: Vec<u8>,
}

impl Bits {
    pub fn new(dots: u32) -> Self {
        Self { dots, bytes: vec![0; data_len(dots)] }
    }
    pub fn set(&mut self, i: u32) {
        self.bytes[(i >> 3) as usize] |= 1 << (i & 7);
    }
    pub fn get(&self, i: u32) -> bool {
        i < self.dots && self.bytes[(i >> 3) as usize] >> (i & 7) & 1 != 0
    }
    pub fn count(&self) -> u32 {
        self.bytes.iter().map(|b| b.count_ones()).sum()
    }
}

/// One track ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub n: u32,
    pub shift: u16,
    pub bits: Bits,
}

impl Track {
    /// header ++ data, exactly what goes after the print CDB.
    pub fn payload(&self) -> Vec<u8> {
        let mut v = header(self.n as u16, false, self.bits.dots as u16, self.shift).to_vec();
        v.extend_from_slice(&self.bits.bytes);
        v
    }
}

/// The last-track marker: header only, track 0, last flag set.
pub fn last_track_payload() -> Vec<u8> {
    header(0, true, 0, 0).to_vec()
}
