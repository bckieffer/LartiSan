//! LightScribe media ID: Reed-Solomon RS(15,11) over GF(128), the byte-to-symbol code and the
//! field layout of the decoded 10 data bytes.
//!
//! Verified three ways: field arithmetic and encode/decode round trips with errors and erasures
//! (tests); all four syndromes are zero for the media ID read from every probed disc; and the
//! decoded fields equal the values measured on those discs (imaging parameter 104, 29/43/23/32 mW,
//! 250/400 mm/s, ...).
//!
//! Provenance of the byte-to-symbol code: HP's library holds a 256-entry table mapping each raw ID
//! byte to a 7-bit symbol (or "invalid"). Inspecting that table shows it is exactly this rule:
//! the symbol is the rank, in increasing numeric order, of the byte among all bytes with 3, 4 or 5
//! bits set, and only ranks 0..=127 are valid. The rule below reproduces all 256 entries; no table
//! data is copied. The 11 data symbols are packed into the 10 data bytes as a little-endian
//! bitstream, symbol 0 in the lowest 7 bits.

/// GF(2^7) with primitive polynomial x^7 + x^3 + 1 (0x89), alpha = x.
pub struct Gf128 {
    exp: [u8; 254],
    log: [u8; 128],
}

impl Gf128 {
    pub fn new() -> Self {
        let (mut exp, mut log) = ([0u8; 254], [0u8; 128]);
        let mut x: u8 = 1;
        for i in 0..127 {
            exp[i] = x;
            exp[i + 127] = x;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x80 != 0 {
                x ^= 0x89;
            }
        }
        Self { exp, log }
    }
    pub fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 { 0 } else { self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize] }
    }
    pub fn inv(&self, a: u8) -> u8 {
        assert!(a != 0);
        self.exp[127 - self.log[a as usize] as usize]
    }
    pub fn div(&self, a: u8, b: u8) -> u8 {
        self.mul(a, self.inv(b))
    }
    /// alpha^k for any integer k.
    pub fn alpha(&self, k: i32) -> u8 {
        self.exp[k.rem_euclid(127) as usize]
    }
    /// Evaluate a polynomial (coefficients low degree first) at x.
    fn eval(&self, p: &[u8], x: u8) -> u8 {
        p.iter().rev().fold(0, |acc, &c| self.mul(acc, x) ^ c)
    }
    fn poly_mul(&self, a: &[u8], b: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; a.len() + b.len() - 1];
        for (i, &x) in a.iter().enumerate() {
            for (j, &y) in b.iter().enumerate() {
                r[i + j] ^= self.mul(x, y);
            }
        }
        r
    }
}

impl Default for Gf128 {
    fn default() -> Self {
        Self::new()
    }
}

pub const N: usize = 15;
pub const K: usize = 11;
pub const PARITY: usize = 4;

#[derive(Debug, PartialEq, Eq)]
pub enum RsError {
    TooManyErrors,
    InvalidSymbol,
}

/// RS(15,11): codeword `c[0..15]`, symbol 0 is the highest-degree coefficient, roots alpha^1..alpha^4.
pub struct Rs {
    gf: Gf128,
}

impl Rs {
    pub fn new() -> Self {
        Self { gf: Gf128::new() }
    }

    fn syndromes(&self, c: &[u8; N]) -> [u8; PARITY] {
        let mut s = [0u8; PARITY];
        for (j, sj) in s.iter_mut().enumerate() {
            let a = self.gf.alpha(j as i32 + 1);
            *sj = c.iter().fold(0, |acc, &x| self.gf.mul(acc, a) ^ x);
        }
        s
    }

    /// True if all four syndromes are zero.
    pub fn is_valid(&self, c: &[u8; N]) -> bool {
        c.iter().all(|&x| x < 128) && self.syndromes(c).iter().all(|&x| x == 0)
    }

    /// Systematic encode: `data` (11 symbols) followed by 4 parity symbols.
    pub fn encode(&self, data: &[u8; K]) -> [u8; N] {
        // generator g(x) = prod (x - alpha^j), j = 1..4 (low degree first)
        let mut g = vec![1u8];
        for j in 1..=PARITY as i32 {
            g = self.gf.poly_mul(&g, &[self.gf.alpha(j), 1]);
        }
        // remainder of data(x) * x^4 divided by g(x); work high degree first
        let mut rem = [0u8; PARITY];
        for &d in data {
            let fb = d ^ rem[0];
            rem = [rem[1], rem[2], rem[3], 0];
            for (i, r) in rem.iter_mut().enumerate() {
                *r ^= self.gf.mul(fb, g[PARITY - 1 - i]);
            }
        }
        let mut c = [0u8; N];
        c[..K].copy_from_slice(data);
        c[K..].copy_from_slice(&rem);
        c
    }

    /// Correct `c` in place. `erasures` are known-bad positions (indices into `c`). Succeeds when
    /// 2 * errors + erasures <= 4. Returns the number of symbols changed.
    pub fn decode(&self, c: &mut [u8; N], erasures: &[usize]) -> Result<usize, RsError> {
        if c.iter().any(|&x| x >= 128) || erasures.iter().any(|&e| e >= N) {
            return Err(RsError::InvalidSymbol);
        }
        let f = erasures.len();
        if f > PARITY {
            return Err(RsError::TooManyErrors);
        }
        let gf = &self.gf;
        let s = self.syndromes(c); // S(x) = S1 + S2 x + S3 x^2 + S4 x^3
        // erasure locator: prod (1 + X_e x), X_e = alpha^(14 - position)
        let mut gamma = vec![1u8];
        for &e in erasures {
            gamma = gf.poly_mul(&gamma, &[1, gf.alpha((N - 1 - e) as i32)]);
        }
        // Forney syndromes: S(x) * gamma(x) mod x^4, dropping the first f terms
        let mut t = gf.poly_mul(&s, &gamma);
        t.resize(PARITY, 0);
        let fs: Vec<u8> = t[f..PARITY].to_vec();

        // Berlekamp-Massey on the Forney syndromes -> error locator lambda (low degree first)
        let (mut lam, mut b) = (vec![1u8], vec![1u8]);
        let (mut l, mut m, mut bb) = (0usize, 1usize, 1u8);
        for n in 0..fs.len() {
            let mut d = fs[n];
            for i in 1..=l {
                if i < lam.len() {
                    d ^= gf.mul(lam[i], fs[n - i]);
                }
            }
            if d == 0 {
                m += 1;
            } else if 2 * l <= n {
                let old = lam.clone();
                let coef = gf.div(d, bb);
                let mut shifted = vec![0u8; m];
                shifted.extend(b.iter().map(|&x| gf.mul(coef, x)));
                lam.resize(lam.len().max(shifted.len()), 0);
                for (i, v) in shifted.iter().enumerate() {
                    lam[i] ^= v;
                }
                l = n + 1 - l;
                b = old;
                bb = d;
                m = 1;
            } else {
                let coef = gf.div(d, bb);
                let mut shifted = vec![0u8; m];
                shifted.extend(b.iter().map(|&x| gf.mul(coef, x)));
                lam.resize(lam.len().max(shifted.len()), 0);
                for (i, v) in shifted.iter().enumerate() {
                    lam[i] ^= v;
                }
                m += 1;
            }
        }
        if 2 * l + f > PARITY {
            return Err(RsError::TooManyErrors);
        }
        // combined locator psi = lambda * gamma; find its roots over the 15 positions
        let psi = gf.poly_mul(&lam, &gamma);
        let positions: Vec<usize> = (0..N).filter(|&i| gf.eval(&psi, gf.alpha(-((N - 1 - i) as i32))) == 0).collect();
        let degree = psi.iter().rposition(|&x| x != 0).unwrap_or(0);
        if positions.len() != degree {
            return Err(RsError::TooManyErrors);
        }
        // error evaluator omega = S * psi mod x^4; magnitude = omega(X^-1) / psi'(X^-1)
        let mut omega = gf.poly_mul(&s, &psi);
        omega.resize(PARITY, 0);
        // formal derivative (characteristic 2): only odd-degree terms survive, lowered by one
        let mut deriv = vec![0u8; psi.len().saturating_sub(1).max(1)];
        for (i, &x) in psi.iter().enumerate() {
            if i % 2 == 1 {
                deriv[i - 1] = x;
            }
        }
        let mut changed = 0;
        for &i in &positions {
            let xinv = gf.alpha(-((N - 1 - i) as i32));
            let den = gf.eval(&deriv, xinv);
            if den == 0 {
                return Err(RsError::TooManyErrors);
            }
            let mag = gf.div(gf.eval(&omega, xinv), den);
            if mag != 0 {
                c[i] ^= mag;
                changed += 1;
            }
        }
        if self.syndromes(c).iter().any(|&x| x != 0) {
            return Err(RsError::TooManyErrors);
        }
        Ok(changed)
    }
}

impl Default for Rs {
    fn default() -> Self {
        Self::new()
    }
}

/// Fields of the decoded 10 data bytes `b[0..10]` (layout from the protocol notes, which are not
/// in this repository; the tests check it against three real discs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaId {
    /// 12-bit imaging parameter (media type); 104 for the Memorex media.
    pub imaging_parameter: u16,
    pub low_speed_max_power_mw: u16,
    pub high_speed_max_power_mw: u16,
    pub low_speed_min_power_mw: u16,
    pub high_speed_min_power_mw: u16,
    pub low_velocity_mm_s: u16,
    pub high_velocity_mm_s: u16,
    pub write_read_ratio: u8,
    /// 1: label area 23.8-58.7 mm, 2: 23.8-37.5 mm.
    pub label_shape: u8,
    pub manufacturer: u8,
    pub laser_type: u8,
    pub media_class: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub enum IdError {
    Version,
    LaserType,
    MediaClass,
    PowerOrder,
    VelocityOrder,
}

impl MediaId {
    pub fn from_data_bytes(b: &[u8; 10]) -> Result<Self, IdError> {
        if b[0] & 0x0F != 0 {
            return Err(IdError::Version);
        }
        let id = Self {
            imaging_parameter: (b[0] >> 4) as u16 + b[1] as u16 * 16,
            low_speed_max_power_mw: (b[2] & 0x3F) as u16 + 10,
            high_speed_max_power_mw: (b[2] >> 6) as u16 + (b[3] & 0x0F) as u16 * 4 + 10,
            low_speed_min_power_mw: (b[3] >> 4) as u16 + (b[4] & 3) as u16 * 16 + 10,
            high_speed_min_power_mw: (b[4] >> 2) as u16 + 10,
            low_velocity_mm_s: b[5] as u16 * 10 + 10,
            high_velocity_mm_s: b[6] as u16 * 10 + 10,
            write_read_ratio: (b[7] & 0x0F) + 2,
            label_shape: b[7] >> 4,
            manufacturer: b[8],
            laser_type: b[9] & 3,
            media_class: ((b[9] >> 2) & 7) + 1,
        };
        if !(1..=2).contains(&id.laser_type) {
            return Err(IdError::LaserType);
        }
        if id.media_class != 1 {
            return Err(IdError::MediaClass);
        }
        if id.low_speed_max_power_mw >= id.high_speed_max_power_mw || id.low_speed_min_power_mw >= id.high_speed_min_power_mw {
            return Err(IdError::PowerOrder);
        }
        if id.low_velocity_mm_s >= id.high_velocity_mm_s {
            return Err(IdError::VelocityOrder);
        }
        Ok(id)
    }
}


/// Symbol (0..=127) for a raw ID byte, or `None` if the byte is not a valid code (it is then an
/// erasure for the RS decoder).
pub fn code_to_symbol(code: u8) -> Option<u8> {
    let ones = code.count_ones();
    if !(3..=5).contains(&ones) {
        return None;
    }
    let rank = (0..code).filter(|c| (3..=5).contains(&c.count_ones())).count();
    if rank < 128 { Some(rank as u8) } else { None }
}

/// Pack the 11 data symbols into the 10 data bytes (little-endian bitstream, 7 bits per symbol).
pub fn symbols_to_data_bytes(s: &[u8; K]) -> [u8; 10] {
    let mut bits: u128 = 0;
    for (i, &x) in s.iter().enumerate() {
        bits |= (x as u128) << (7 * i);
    }
    let mut b = [0u8; 10];
    for (j, byte) in b.iter_mut().enumerate() {
        *byte = (bits >> (8 * j)) as u8;
    }
    b
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// More damage than RS(15,11) can repair (2 * errors + erasures > 4).
    Uncorrectable,
    Fields(IdError),
}

/// A decoded media ID plus how much repair it needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedId {
    pub id: MediaId,
    /// Bytes that were not valid codes (treated as erasures).
    pub erasures: usize,
    /// Additional symbols the RS decoder changed.
    pub corrected: usize,
}

/// Decode the 15 raw outer-ID bytes from page 0x32.
pub fn decode_outer_id(raw: &[u8; N]) -> Result<DecodedId, DecodeError> {
    let mut sym = [0u8; N];
    let mut erasures = vec![];
    for (i, &b) in raw.iter().enumerate() {
        match code_to_symbol(b) {
            Some(x) => sym[i] = x,
            None => erasures.push(i),
        }
    }
    let rs = Rs::new();
    let changed = rs.decode(&mut sym, &erasures).map_err(|_| DecodeError::Uncorrectable)?;
    let mut data = [0u8; K];
    data.copy_from_slice(&sym[..K]);
    let id = MediaId::from_data_bytes(&symbols_to_data_bytes(&data)).map_err(DecodeError::Fields)?;
    Ok(DecodedId { id, erasures: erasures.len(), corrected: changed.saturating_sub(erasures.len()) })
}

impl MediaId {
    /// Write-power window (mW) allowed at `speed_mm_s`: the low-speed pair at the low velocity, the
    /// high-speed pair at the high velocity, linearly in between (and clamped outside).
    pub fn power_window_mw(&self, speed_mm_s: f64) -> (f64, f64) {
        let (v0, v1) = (self.low_velocity_mm_s as f64, self.high_velocity_mm_s as f64);
        let f = ((speed_mm_s - v0) / (v1 - v0)).clamp(0.0, 1.0);
        let lerp = |a: u16, b: u16| a as f64 + (b as f64 - a as f64) * f;
        (lerp(self.low_speed_min_power_mw, self.high_speed_min_power_mw), lerp(self.low_speed_max_power_mw, self.high_speed_max_power_mw))
    }
}
