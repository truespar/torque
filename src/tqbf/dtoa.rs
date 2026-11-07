//! Double -> decimal text exactly as nlohmann::json `dump()` writes it (Typesense stores and returns
//! documents through nlohmann::json). Port of `nlohmann::detail::to_chars` (Grisu2, Loitsch 2010, plus
//! nlohmann's `format_buffer`). Grisu2 is not always the shortest representation, e.g. it writes
//! `11.952819999999999` where the shortest round-trip form is `11.95282`; byte-identical output needs
//! this algorithm, not ryu.

#[derive(Clone, Copy)]
struct DiyFp {
    f: u64,
    e: i32,
}

impl DiyFp {
    const fn new(f: u64, e: i32) -> Self {
        Self { f, e }
    }

    fn sub(x: DiyFp, y: DiyFp) -> DiyFp {
        DiyFp::new(x.f - y.f, x.e)
    }

    /// 64x64 -> upper 64 bits, rounded (ties up), as nlohmann's diyfp::mul.
    fn mul(x: DiyFp, y: DiyFp) -> DiyFp {
        let u_lo = x.f & 0xFFFF_FFFF;
        let u_hi = x.f >> 32;
        let v_lo = y.f & 0xFFFF_FFFF;
        let v_hi = y.f >> 32;
        let p0 = u_lo * v_lo;
        let p1 = u_lo * v_hi;
        let p2 = u_hi * v_lo;
        let p3 = u_hi * v_hi;
        let p0_hi = p0 >> 32;
        let p1_lo = p1 & 0xFFFF_FFFF;
        let p1_hi = p1 >> 32;
        let p2_lo = p2 & 0xFFFF_FFFF;
        let p2_hi = p2 >> 32;
        let mut q = p0_hi + p1_lo + p2_lo;
        q += 1u64 << 31;
        let h = p3 + p2_hi + p1_hi + (q >> 32);
        DiyFp::new(h, x.e + y.e + 64)
    }

    fn normalize(mut x: DiyFp) -> DiyFp {
        while x.f >> 63 == 0 {
            x.f <<= 1;
            x.e -= 1;
        }
        x
    }

    fn normalize_to(x: DiyFp, target_exponent: i32) -> DiyFp {
        let delta = x.e - target_exponent;
        DiyFp::new(x.f << delta, target_exponent)
    }
}

struct Boundaries {
    w: DiyFp,
    minus: DiyFp,
    plus: DiyFp,
}

fn compute_boundaries(value: f64) -> Boundaries {
    const PRECISION: i32 = 53;
    const BIAS: i32 = 1024 - 1 + (PRECISION - 1);
    const MIN_EXP: i32 = 1 - BIAS;
    const HIDDEN_BIT: u64 = 1u64 << (PRECISION - 1);

    let bits = value.to_bits();
    let e = bits >> (PRECISION - 1);
    let f = bits & (HIDDEN_BIT - 1);
    let is_denormal = e == 0;
    let v = if is_denormal {
        DiyFp::new(f, MIN_EXP)
    } else {
        DiyFp::new(f + HIDDEN_BIT, e as i32 - BIAS)
    };
    let lower_boundary_is_closer = f == 0 && e > 1;
    let m_plus = DiyFp::new(2 * v.f + 1, v.e - 1);
    let m_minus = if lower_boundary_is_closer {
        DiyFp::new(4 * v.f - 1, v.e - 2)
    } else {
        DiyFp::new(2 * v.f - 1, v.e - 1)
    };
    let w_plus = DiyFp::normalize(m_plus);
    let w_minus = DiyFp::normalize_to(m_minus, w_plus.e);
    Boundaries { w: DiyFp::normalize(v), minus: w_minus, plus: w_plus }
}

const ALPHA: i32 = -60;

struct CachedPower {
    f: u64,
    e: i32,
    k: i32,
}

/// 10^k for k = -300, -292, ..., 324: 64-bit significand rounded to nearest (generated with exact
/// arithmetic; matches nlohmann's kCachedPowers).
#[rustfmt::skip]
const CACHED_POWERS: [CachedPower; 79] = [
    CachedPower { f: 0xAB70FE17C79AC6CA, e: -1060, k: -300 },
    CachedPower { f: 0xFF77B1FCBEBCDC4F, e: -1034, k: -292 },
    CachedPower { f: 0xBE5691EF416BD60C, e: -1007, k: -284 },
    CachedPower { f: 0x8DD01FAD907FFC3C, e: -980, k: -276 },
    CachedPower { f: 0xD3515C2831559A83, e: -954, k: -268 },
    CachedPower { f: 0x9D71AC8FADA6C9B5, e: -927, k: -260 },
    CachedPower { f: 0xEA9C227723EE8BCB, e: -901, k: -252 },
    CachedPower { f: 0xAECC49914078536D, e: -874, k: -244 },
    CachedPower { f: 0x823C12795DB6CE57, e: -847, k: -236 },
    CachedPower { f: 0xC21094364DFB5637, e: -821, k: -228 },
    CachedPower { f: 0x9096EA6F3848984F, e: -794, k: -220 },
    CachedPower { f: 0xD77485CB25823AC7, e: -768, k: -212 },
    CachedPower { f: 0xA086CFCD97BF97F4, e: -741, k: -204 },
    CachedPower { f: 0xEF340A98172AACE5, e: -715, k: -196 },
    CachedPower { f: 0xB23867FB2A35B28E, e: -688, k: -188 },
    CachedPower { f: 0x84C8D4DFD2C63F3B, e: -661, k: -180 },
    CachedPower { f: 0xC5DD44271AD3CDBA, e: -635, k: -172 },
    CachedPower { f: 0x936B9FCEBB25C996, e: -608, k: -164 },
    CachedPower { f: 0xDBAC6C247D62A584, e: -582, k: -156 },
    CachedPower { f: 0xA3AB66580D5FDAF6, e: -555, k: -148 },
    CachedPower { f: 0xF3E2F893DEC3F126, e: -529, k: -140 },
    CachedPower { f: 0xB5B5ADA8AAFF80B8, e: -502, k: -132 },
    CachedPower { f: 0x87625F056C7C4A8B, e: -475, k: -124 },
    CachedPower { f: 0xC9BCFF6034C13053, e: -449, k: -116 },
    CachedPower { f: 0x964E858C91BA2655, e: -422, k: -108 },
    CachedPower { f: 0xDFF9772470297EBD, e: -396, k: -100 },
    CachedPower { f: 0xA6DFBD9FB8E5B88F, e: -369, k: -92 },
    CachedPower { f: 0xF8A95FCF88747D94, e: -343, k: -84 },
    CachedPower { f: 0xB94470938FA89BCF, e: -316, k: -76 },
    CachedPower { f: 0x8A08F0F8BF0F156B, e: -289, k: -68 },
    CachedPower { f: 0xCDB02555653131B6, e: -263, k: -60 },
    CachedPower { f: 0x993FE2C6D07B7FAC, e: -236, k: -52 },
    CachedPower { f: 0xE45C10C42A2B3B06, e: -210, k: -44 },
    CachedPower { f: 0xAA242499697392D3, e: -183, k: -36 },
    CachedPower { f: 0xFD87B5F28300CA0E, e: -157, k: -28 },
    CachedPower { f: 0xBCE5086492111AEB, e: -130, k: -20 },
    CachedPower { f: 0x8CBCCC096F5088CC, e: -103, k: -12 },
    CachedPower { f: 0xD1B71758E219652C, e: -77, k: -4 },
    CachedPower { f: 0x9C40000000000000, e: -50, k: 4 },
    CachedPower { f: 0xE8D4A51000000000, e: -24, k: 12 },
    CachedPower { f: 0xAD78EBC5AC620000, e: 3, k: 20 },
    CachedPower { f: 0x813F3978F8940984, e: 30, k: 28 },
    CachedPower { f: 0xC097CE7BC90715B3, e: 56, k: 36 },
    CachedPower { f: 0x8F7E32CE7BEA5C70, e: 83, k: 44 },
    CachedPower { f: 0xD5D238A4ABE98068, e: 109, k: 52 },
    CachedPower { f: 0x9F4F2726179A2245, e: 136, k: 60 },
    CachedPower { f: 0xED63A231D4C4FB27, e: 162, k: 68 },
    CachedPower { f: 0xB0DE65388CC8ADA8, e: 189, k: 76 },
    CachedPower { f: 0x83C7088E1AAB65DB, e: 216, k: 84 },
    CachedPower { f: 0xC45D1DF942711D9A, e: 242, k: 92 },
    CachedPower { f: 0x924D692CA61BE758, e: 269, k: 100 },
    CachedPower { f: 0xDA01EE641A708DEA, e: 295, k: 108 },
    CachedPower { f: 0xA26DA3999AEF774A, e: 322, k: 116 },
    CachedPower { f: 0xF209787BB47D6B85, e: 348, k: 124 },
    CachedPower { f: 0xB454E4A179DD1877, e: 375, k: 132 },
    CachedPower { f: 0x865B86925B9BC5C2, e: 402, k: 140 },
    CachedPower { f: 0xC83553C5C8965D3D, e: 428, k: 148 },
    CachedPower { f: 0x952AB45CFA97A0B3, e: 455, k: 156 },
    CachedPower { f: 0xDE469FBD99A05FE3, e: 481, k: 164 },
    CachedPower { f: 0xA59BC234DB398C25, e: 508, k: 172 },
    CachedPower { f: 0xF6C69A72A3989F5C, e: 534, k: 180 },
    CachedPower { f: 0xB7DCBF5354E9BECE, e: 561, k: 188 },
    CachedPower { f: 0x88FCF317F22241E2, e: 588, k: 196 },
    CachedPower { f: 0xCC20CE9BD35C78A5, e: 614, k: 204 },
    CachedPower { f: 0x98165AF37B2153DF, e: 641, k: 212 },
    CachedPower { f: 0xE2A0B5DC971F303A, e: 667, k: 220 },
    CachedPower { f: 0xA8D9D1535CE3B396, e: 694, k: 228 },
    CachedPower { f: 0xFB9B7CD9A4A7443C, e: 720, k: 236 },
    CachedPower { f: 0xBB764C4CA7A44410, e: 747, k: 244 },
    CachedPower { f: 0x8BAB8EEFB6409C1A, e: 774, k: 252 },
    CachedPower { f: 0xD01FEF10A657842C, e: 800, k: 260 },
    CachedPower { f: 0x9B10A4E5E9913129, e: 827, k: 268 },
    CachedPower { f: 0xE7109BFBA19C0C9D, e: 853, k: 276 },
    CachedPower { f: 0xAC2820D9623BF429, e: 880, k: 284 },
    CachedPower { f: 0x80444B5E7AA7CF85, e: 907, k: 292 },
    CachedPower { f: 0xBF21E44003ACDD2D, e: 933, k: 300 },
    CachedPower { f: 0x8E679C2F5E44FF8F, e: 960, k: 308 },
    CachedPower { f: 0xD433179D9C8CB841, e: 986, k: 316 },
    CachedPower { f: 0x9E19DB92B4E31BA9, e: 1013, k: 324 },
];

fn cached_power_for_binary_exponent(e: i32) -> &'static CachedPower {
    const MIN_DEC_EXP: i32 = -300;
    const DEC_STEP: i32 = 8;
    let f = ALPHA - e - 1;
    let k = (f * 78913) / (1 << 18) + i32::from(f > 0);
    let index = (-MIN_DEC_EXP + k + (DEC_STEP - 1)) / DEC_STEP;
    &CACHED_POWERS[index as usize]
}

fn find_largest_pow10(n: u32) -> (i32, u32) {
    match n {
        1_000_000_000.. => (10, 1_000_000_000),
        100_000_000.. => (9, 100_000_000),
        10_000_000.. => (8, 10_000_000),
        1_000_000.. => (7, 1_000_000),
        100_000.. => (6, 100_000),
        10_000.. => (5, 10_000),
        1_000.. => (4, 1_000),
        100.. => (3, 100),
        10.. => (2, 10),
        _ => (1, 1),
    }
}

fn grisu2_round(buf: &mut [u8], len: usize, dist: u64, delta: u64, mut rest: u64, ten_k: u64) {
    while rest < dist
        && delta - rest >= ten_k
        && (rest + ten_k < dist || dist - rest > rest + ten_k - dist)
    {
        buf[len - 1] -= 1;
        rest += ten_k;
    }
}

fn grisu2_digit_gen(buf: &mut [u8], len: &mut usize, decimal_exponent: &mut i32, m_minus: DiyFp, w: DiyFp, m_plus: DiyFp) {
    let mut delta = DiyFp::sub(m_plus, m_minus).f;
    let mut dist = DiyFp::sub(m_plus, w).f;
    let one = DiyFp::new(1u64 << -m_plus.e, m_plus.e);
    let mut p1 = (m_plus.f >> -one.e) as u32;
    let mut p2 = m_plus.f & (one.f - 1);

    let (k, mut pow10) = find_largest_pow10(p1);
    let mut n = k;
    while n > 0 {
        let d = p1 / pow10;
        let r = p1 % pow10;
        buf[*len] = b'0' + d as u8;
        *len += 1;
        p1 = r;
        n -= 1;
        let rest = (u64::from(p1) << -one.e) + p2;
        if rest <= delta {
            *decimal_exponent += n;
            let ten_n = u64::from(pow10) << -one.e;
            grisu2_round(buf, *len, dist, delta, rest, ten_n);
            return;
        }
        pow10 /= 10;
    }

    let mut m = 0;
    loop {
        p2 *= 10;
        let d = p2 >> -one.e;
        let r = p2 & (one.f - 1);
        buf[*len] = b'0' + d as u8;
        *len += 1;
        p2 = r;
        m += 1;
        delta *= 10;
        dist *= 10;
        if p2 <= delta {
            break;
        }
    }
    *decimal_exponent -= m;
    grisu2_round(buf, *len, dist, delta, p2, one.f);
}

/// Digits and decimal exponent of a finite positive double: value = digits * 10^exponent.
fn grisu2(buf: &mut [u8], value: f64) -> (usize, i32) {
    let w = compute_boundaries(value);
    let cached = cached_power_for_binary_exponent(w.plus.e);
    let c_minus_k = DiyFp::new(cached.f, cached.e);
    let w_scaled = DiyFp::mul(w.w, c_minus_k);
    let w_minus = DiyFp::mul(w.minus, c_minus_k);
    let w_plus = DiyFp::mul(w.plus, c_minus_k);
    let m_minus = DiyFp::new(w_minus.f + 1, w_minus.e);
    let m_plus = DiyFp::new(w_plus.f - 1, w_plus.e);
    let mut len = 0;
    let mut decimal_exponent = -cached.k;
    grisu2_digit_gen(buf, &mut len, &mut decimal_exponent, m_minus, w_scaled, m_plus);
    (len, decimal_exponent)
}

fn push_exponent(out: &mut Vec<u8>, mut e: i32) {
    if e < 0 {
        e = -e;
        out.push(b'-');
    } else {
        out.push(b'+');
    }
    let k = e as u32;
    if k < 10 {
        out.push(b'0');
        out.push(b'0' + k as u8);
    } else if k < 100 {
        out.push(b'0' + (k / 10) as u8);
        out.push(b'0' + (k % 10) as u8);
    } else {
        out.push(b'0' + (k / 100) as u8);
        out.push(b'0' + (k / 10 % 10) as u8);
        out.push(b'0' + (k % 10) as u8);
    }
}

/// Append `value` as nlohmann::json writes it. Non-finite values are written as `null` (nlohmann's
/// serializer does the same; JSON has no NaN or infinity).
pub fn push_f64(out: &mut Vec<u8>, mut value: f64) {
    if !value.is_finite() {
        out.extend_from_slice(b"null");
        return;
    }
    if value.is_sign_negative() {
        value = -value;
        out.push(b'-');
    }
    if value == 0.0 {
        out.extend_from_slice(b"0.0");
        return;
    }
    const MIN_EXP: i32 = -4;
    const MAX_EXP: i32 = 15;
    let mut digits = [0u8; 20];
    let (k, decimal_exponent) = grisu2(&mut digits, value);
    let digits = &digits[..k];
    let k = k as i32;
    let n = k + decimal_exponent;
    if k <= n && n <= MAX_EXP {
        out.extend_from_slice(digits);
        out.extend(std::iter::repeat_n(b'0', (n - k) as usize));
        out.extend_from_slice(b".0");
    } else if 0 < n && n <= MAX_EXP {
        out.extend_from_slice(&digits[..n as usize]);
        out.push(b'.');
        out.extend_from_slice(&digits[n as usize..]);
    } else if MIN_EXP < n && n <= 0 {
        out.extend_from_slice(b"0.");
        out.extend(std::iter::repeat_n(b'0', (-n) as usize));
        out.extend_from_slice(digits);
    } else {
        out.push(digits[0]);
        if k > 1 {
            out.push(b'.');
            out.extend_from_slice(&digits[1..]);
        }
        out.push(b'e');
        push_exponent(out, n - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::push_f64;

    fn fmt(v: f64) -> String {
        let mut out = Vec::new();
        push_f64(&mut out, v);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn matches_nlohmann() {
        // Values observed in Typesense-stored documents plus nlohmann format edge cases.
        let cases = [
            (11.95282, "11.952819999999999"),
            (59.43384, "59.433839999999996"),
            (59.31372, "59.313720000000004"),
            (21.31189, "21.311889999999998"),
            (56.05908, "56.05908"),
            (40.0, "40.0"),
            (250000.0, "250000.0"),
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (0.5, "0.5"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e14, "100000000000000.0"),
            (1e15, "1e+15"),
            (1e16, "1e+16"),
            (123456789012345680.0, "1.2345678901234568e+17"),
            (-2.5e-300, "-2.5e-300"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
        ];
        for (v, want) in cases {
            assert_eq!(fmt(v), want, "value {v:?}");
        }
    }
}
