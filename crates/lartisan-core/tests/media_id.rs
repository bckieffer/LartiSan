//! Reed-Solomon RS(15,11) over GF(128) and the media ID field layout.

use lartisan_core::media_id::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn sym(&mut self) -> u8 {
        (self.next() % 128) as u8
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn random_codeword(rs: &Rs, rng: &mut Rng) -> [u8; N] {
    let mut d = [0u8; K];
    d.iter_mut().for_each(|x| *x = rng.sym());
    rs.encode(&d)
}

#[test]
fn field_arithmetic() {
    let gf = Gf128::new();
    // alpha has order 127 and 1 / alpha^k * alpha^k = 1
    assert_eq!(gf.alpha(0), 1);
    assert_eq!(gf.alpha(127), 1);
    assert_eq!(gf.alpha(7), 0x09, "alpha^7 = x^3 + 1 for polynomial 0x89");
    for a in 1..128u8 {
        assert_eq!(gf.mul(a, gf.inv(a)), 1);
        assert_eq!(gf.mul(a, 1), a);
    }
    for a in 0..128u8 {
        for b in 0..128u8 {
            assert_eq!(gf.mul(a, b), gf.mul(b, a));
            assert!(gf.mul(a, b) < 128);
        }
    }
}

#[test]
fn encoded_words_have_zero_syndromes() {
    let rs = Rs::new();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for _ in 0..200 {
        assert!(rs.is_valid(&random_codeword(&rs, &mut rng)));
    }
    // a single changed symbol is detected
    let mut c = random_codeword(&rs, &mut rng);
    c[3] ^= 0x15;
    assert!(!rs.is_valid(&c));
}

#[test]
fn corrects_errors_and_erasures_within_capacity() {
    let rs = Rs::new();
    let mut rng = Rng(0x0dd_c0ffee_u64);
    let mut cases = 0;
    for _ in 0..400 {
        let good = random_codeword(&rs, &mut rng);
        for (errors, erasures) in [(0usize, 0usize), (1, 0), (2, 0), (0, 1), (0, 2), (0, 3), (0, 4), (1, 1), (1, 2), (2, 0)] {
            assert!(2 * errors + erasures <= 4);
            let mut pos: Vec<usize> = (0..N).collect();
            for i in (1..N).rev() {
                pos.swap(i, rng.below(i + 1));
            }
            let (err_pos, rest) = pos.split_at(errors);
            let era_pos = &rest[..erasures];
            let mut bad = good;
            for &p in err_pos {
                bad[p] ^= 1 + (rng.next() % 127) as u8; // guaranteed different
            }
            for &p in era_pos {
                bad[p] = rng.sym(); // arbitrary, possibly equal to the original
            }
            let mut fixed = bad;
            rs.decode(&mut fixed, era_pos).unwrap_or_else(|e| panic!("{e:?} for errors {err_pos:?} erasures {era_pos:?}"));
            assert_eq!(fixed, good, "errors {err_pos:?} erasures {era_pos:?}");
            cases += 1;
        }
    }
    assert!(cases > 3000);
}

#[test]
fn five_erasures_are_refused() {
    let rs = Rs::new();
    let mut c = random_codeword(&rs, &mut Rng(77));
    assert_eq!(rs.decode(&mut c, &[0, 1, 2, 3, 4]), Err(RsError::TooManyErrors));
}

#[test]
fn field_layout_matches_the_probed_discs() {
    // data bytes rebuilt from the values measured on every probed disc (imaging parameter 104,
    // 29/43/23/32 mW, 250/400 mm/s, ratio 3, shape 1, manufacturer 2, laser type 1, class 1)
    let b = [0x80u8, 6, 83, 0xD8, 88, 24, 39, 0x11, 2, 1];
    let id = MediaId::from_data_bytes(&b).unwrap();
    assert_eq!(id.imaging_parameter, 104);
    assert_eq!((id.low_speed_max_power_mw, id.high_speed_max_power_mw), (29, 43));
    assert_eq!((id.low_speed_min_power_mw, id.high_speed_min_power_mw), (23, 32));
    assert_eq!((id.low_velocity_mm_s, id.high_velocity_mm_s), (250, 400));
    assert_eq!((id.write_read_ratio, id.label_shape, id.manufacturer, id.laser_type, id.media_class), (3, 1, 2, 1, 1));
}

#[test]
fn field_validation() {
    let mut b = [0x80u8, 6, 83, 0xD8, 88, 24, 39, 0x11, 2, 1];
    b[0] |= 1;
    assert_eq!(MediaId::from_data_bytes(&b), Err(IdError::Version));
    let mut b = [0x80u8, 6, 83, 0xD8, 88, 24, 39, 0x11, 2, 0];
    assert_eq!(MediaId::from_data_bytes(&b), Err(IdError::LaserType));
    b[9] = 1 | (1 << 2);
    assert_eq!(MediaId::from_data_bytes(&b), Err(IdError::MediaClass));
    let b = [0x80u8, 6, 83, 0xD8, 88, 40, 39, 0x11, 2, 1];
    assert_eq!(MediaId::from_data_bytes(&b), Err(IdError::VelocityOrder));
}


const KNOWN: [u8; 15] = [0x07, 0x1d, 0x6b, 0x61, 0x1d, 0x1b, 0x65, 0x27, 0x25, 0x0f, 0x0f, 0x66, 0x2a, 0x07, 0x27];

fn known_ok() -> DecodedId {
    decode_outer_id(&KNOWN).unwrap()
}

#[test]
fn symbol_code_rule() {
    assert_eq!(code_to_symbol(0x07), Some(0));
    assert_eq!(code_to_symbol(0x0b), Some(1));
    assert_eq!(code_to_symbol(0x0d), Some(2));
    assert_eq!(code_to_symbol(0xac), Some(127));
    for bad in [0x00u8, 0x01, 0x03, 0x3f, 0x7f, 0xad, 0xff] {
        assert_eq!(code_to_symbol(bad), None, "{bad:#x}");
    }
    // exactly 128 valid codes, symbols 0..=127 each used once, in increasing code order
    let valid: Vec<(u8, u8)> = (0..=255u8).filter_map(|c| code_to_symbol(c).map(|s| (c, s))).collect();
    assert_eq!(valid.len(), 128);
    assert!(valid.iter().enumerate().all(|(i, &(_, s))| s as usize == i));
}

#[test]
fn known_disc_id_decodes_cleanly() {
    let syms: Vec<u8> = KNOWN.iter().map(|&b| code_to_symbol(b).unwrap()).collect();
    assert_eq!(syms, [0, 13, 76, 66, 13, 11, 70, 19, 17, 4, 4, 71, 21, 0, 19]);
    let mut arr = [0u8; 15];
    arr.copy_from_slice(&syms);
    assert!(Rs::new().is_valid(&arr), "all four syndromes are zero on the real ID");
    let d = known_ok();
    assert_eq!((d.erasures, d.corrected), (0, 0));
    assert_eq!(d.id.imaging_parameter, 104);
    assert_eq!((d.id.low_speed_max_power_mw, d.id.high_speed_max_power_mw, d.id.low_speed_min_power_mw, d.id.high_speed_min_power_mw), (29, 43, 23, 32));
    assert_eq!((d.id.low_velocity_mm_s, d.id.high_velocity_mm_s), (250, 400));
    assert_eq!((d.id.write_read_ratio, d.id.label_shape, d.id.manufacturer, d.id.laser_type, d.id.media_class), (3, 1, 2, 1, 1));
    // the power window the CLI used to hard-code: 23-29 mW at 250 mm/s
    assert_eq!(d.id.power_window_mw(250.0), (23.0, 29.0));
}

#[test]
fn repairs_a_damaged_known_id() {
    // one byte replaced by another valid code (a plain error)
    let mut raw = KNOWN;
    raw[3] = 0x0b;
    let d = decode_outer_id(&raw).unwrap();
    assert_eq!((d.erasures, d.corrected), (0, 1));
    assert_eq!(d.id.imaging_parameter, 104);
    // two bytes unreadable (invalid codes -> erasures)
    let mut raw = KNOWN;
    raw[0] = 0x00;
    raw[9] = 0xff;
    let d = decode_outer_id(&raw).unwrap();
    assert_eq!((d.erasures, d.corrected), (2, 0));
    assert_eq!(d.id, known_ok().id);
    // one error and two erasures still fit (2*1 + 2 = 4)
    let mut raw = KNOWN;
    raw[1] = 0x0b;
    raw[5] = 0x00;
    raw[12] = 0x00;
    assert_eq!(decode_outer_id(&raw).unwrap().id, known_ok().id);
    // five unreadable bytes do not
    let mut raw = KNOWN;
    raw[..5].fill(0);
    assert_eq!(decode_outer_id(&raw), Err(DecodeError::Uncorrectable));
}

#[test]
fn control_from_the_notes() {
    // corrupting symbol 5 of the real ID by xor 17 gave syndromes (64, 11, 94, 12) on the bench
    let mut s: Vec<u8> = KNOWN.iter().map(|&b| code_to_symbol(b).unwrap()).collect();
    s[5] ^= 17;
    let mut arr = [0u8; 15];
    arr.copy_from_slice(&s);
    assert!(!Rs::new().is_valid(&arr));
}
