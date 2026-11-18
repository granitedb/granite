//! The 128-bit checksum of the on-disk and wire formats: XXH3-128 with the
//! default secret and seed 0 (`docs/decisions/0002-checksum.md`).
//!
//! [`checksum`] is the only entry point. The value is part of the format, so
//! the scalar path and the SIMD kernels must return the same bits for every
//! input; the tests check them against vectors from the reference C
//! implementation (xxHash 0.8.3) and against each other.

#[allow(
    unsafe_code,
    reason = "SIMD kernels; every block has a SAFETY argument"
)]
mod simd;

#[cfg(test)]
#[allow(
    unsafe_code,
    reason = "AES instructions of the rejected candidate; every block has a SAFETY argument"
)]
mod aegis;
#[cfg(test)]
mod bench;

/// Returns the checksum of `bytes`.
pub fn checksum(bytes: &[u8]) -> u128 {
    if bytes.len() <= MIDSIZE_MAX {
        short(bytes)
    } else {
        simd::long(bytes)
    }
}

/// The same function without SIMD kernels: the reference the kernels are
/// tested and benchmarked against.
#[cfg(test)]
fn checksum_scalar(bytes: &[u8]) -> u128 {
    if bytes.len() <= MIDSIZE_MAX {
        short(bytes)
    } else {
        long_scalar(bytes)
    }
}

const PRIME32_1: u64 = 0x9E37_79B1;
const PRIME32_2: u64 = 0x85EB_CA77;
const PRIME32_3: u64 = 0xC2B2_AE3D;
const PRIME64_1: u64 = 0x9E37_79B1_85EB_CA87;
const PRIME64_2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const PRIME64_3: u64 = 0x1656_67B1_9E37_79F9;
const PRIME64_4: u64 = 0x85EB_CA77_C2B2_AE63;
const PRIME64_5: u64 = 0x27D4_EB2F_1656_67C5;
const PRIME_MX1: u64 = 0x1656_6791_9E37_79F9;
const PRIME_MX2: u64 = 0x9FB2_1C65_1E98_DF25;

/// Inputs up to this length take the short paths, which have no SIMD kernel.
const MIDSIZE_MAX: usize = 240;
const MIDSIZE_START_OFFSET: usize = 3;
const MIDSIZE_LAST_OFFSET: usize = 17;
const SECRET_SIZE_MIN: usize = 136;
const STRIPE_LEN: usize = 64;
const SECRET_CONSUME_RATE: usize = 8;
const STRIPES_PER_BLOCK: usize = (SECRET.len() - STRIPE_LEN) / SECRET_CONSUME_RATE;
const BLOCK_LEN: usize = STRIPE_LEN * STRIPES_PER_BLOCK;
const SECRET_LAST_ACC_START: usize = 7;
const SECRET_MERGE_ACCS_START: usize = 11;

/// Accumulator lanes of the long path.
type Acc = [u64; STRIPE_LEN / 8];
type Stripe = [u8; STRIPE_LEN];

const INIT_ACC: Acc = [
    PRIME32_3, PRIME64_1, PRIME64_2, PRIME64_3, PRIME64_4, PRIME32_2, PRIME64_5, PRIME32_1,
];

/// The default secret of XXH3.
const SECRET: [u8; 192] = [
    0xb8, 0xfe, 0x6c, 0x39, 0x23, 0xa4, 0x4b, 0xbe, 0x7c, 0x01, 0x81, 0x2c, 0xf7, 0x21, 0xad, 0x1c,
    0xde, 0xd4, 0x6d, 0xe9, 0x83, 0x90, 0x97, 0xdb, 0x72, 0x40, 0xa4, 0xa4, 0xb7, 0xb3, 0x67, 0x1f,
    0xcb, 0x79, 0xe6, 0x4e, 0xcc, 0xc0, 0xe5, 0x78, 0x82, 0x5a, 0xd0, 0x7d, 0xcc, 0xff, 0x72, 0x21,
    0xb8, 0x08, 0x46, 0x74, 0xf7, 0x43, 0x24, 0x8e, 0xe0, 0x35, 0x90, 0xe6, 0x81, 0x3a, 0x26, 0x4c,
    0x3c, 0x28, 0x52, 0xbb, 0x91, 0xc3, 0x00, 0xcb, 0x88, 0xd0, 0x65, 0x8b, 0x1b, 0x53, 0x2e, 0xa3,
    0x71, 0x64, 0x48, 0x97, 0xa2, 0x0d, 0xf9, 0x4e, 0x38, 0x19, 0xef, 0x46, 0xa9, 0xde, 0xac, 0xd8,
    0xa8, 0xfa, 0x76, 0x3f, 0xe3, 0x9c, 0x34, 0x3f, 0xf9, 0xdc, 0xbb, 0xc7, 0xc7, 0x0b, 0x4f, 0x1d,
    0x8a, 0x51, 0xe0, 0x4b, 0xcd, 0xb4, 0x59, 0x31, 0xc8, 0x9f, 0x7e, 0xc9, 0xd9, 0x78, 0x73, 0x64,
    0xea, 0xc5, 0xac, 0x83, 0x34, 0xd3, 0xeb, 0xc3, 0xc5, 0x81, 0xa0, 0xff, 0xfa, 0x13, 0x63, 0xeb,
    0x17, 0x0d, 0xdd, 0x51, 0xb7, 0xf0, 0xda, 0x49, 0xd3, 0x16, 0x55, 0x26, 0x29, 0xd4, 0x68, 0x9e,
    0x2b, 0x16, 0xbe, 0x58, 0x7d, 0x47, 0xa1, 0xfc, 0x8f, 0xf8, 0xb8, 0xd1, 0x7a, 0xd0, 0x31, 0xce,
    0x45, 0xcb, 0x3a, 0x8f, 0x95, 0x16, 0x04, 0x28, 0xaf, 0xd7, 0xfb, 0xca, 0xbb, 0x4b, 0x40, 0x7e,
];

fn r32(b: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes(b[at..at + 4].try_into().unwrap()))
}

fn r64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn hash128(high: u64, low: u64) -> u128 {
    (u128::from(high) << 64) | u128::from(low)
}

fn mul128(a: u64, b: u64) -> (u64, u64) {
    let p = u128::from(a) * u128::from(b);
    ((p >> 64) as u64, p as u64)
}

fn mul128_fold64(a: u64, b: u64) -> u64 {
    let (high, low) = mul128(a, b);
    high ^ low
}

fn xxh64_avalanche(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(PRIME64_2);
    h ^= h >> 29;
    h = h.wrapping_mul(PRIME64_3);
    h ^ (h >> 32)
}

fn avalanche(mut h: u64) -> u64 {
    h ^= h >> 37;
    h = h.wrapping_mul(PRIME_MX1);
    h ^ (h >> 32)
}

fn mix16(input: &[u8], at: usize, secret_at: usize) -> u64 {
    mul128_fold64(
        r64(input, at) ^ r64(&SECRET, secret_at),
        r64(input, at + 8) ^ r64(&SECRET, secret_at + 8),
    )
}

fn mix32(acc: (u64, u64), input: &[u8], at1: usize, at2: usize, secret_at: usize) -> (u64, u64) {
    let low = acc.0.wrapping_add(mix16(input, at1, secret_at))
        ^ r64(input, at2).wrapping_add(r64(input, at2 + 8));
    let high = acc.1.wrapping_add(mix16(input, at2, secret_at + 16))
        ^ r64(input, at1).wrapping_add(r64(input, at1 + 8));
    (low, high)
}

/// The 17 to 240 byte paths end the same way.
fn mid_final(acc: (u64, u64), len: u64) -> u128 {
    let low = acc.0.wrapping_add(acc.1);
    let high = acc
        .0
        .wrapping_mul(PRIME64_1)
        .wrapping_add(acc.1.wrapping_mul(PRIME64_4))
        .wrapping_add(len.wrapping_mul(PRIME64_2));
    hash128(avalanche(high).wrapping_neg(), avalanche(low))
}

/// Inputs of at most `MIDSIZE_MAX` bytes.
fn short(input: &[u8]) -> u128 {
    let n = input.len();
    let len = n as u64;
    assert!(n <= MIDSIZE_MAX);
    match n {
        0 => hash128(
            xxh64_avalanche(r64(&SECRET, 80) ^ r64(&SECRET, 88)),
            xxh64_avalanche(r64(&SECRET, 64) ^ r64(&SECRET, 72)),
        ),
        1..=3 => {
            let combined_low = (u32::from(input[0]) << 16)
                | (u32::from(input[n >> 1]) << 24)
                | u32::from(input[n - 1])
                | ((n as u32) << 8);
            let combined_high = combined_low.swap_bytes().rotate_left(13);
            let low = u64::from(combined_low) ^ (r32(&SECRET, 0) ^ r32(&SECRET, 4));
            let high = u64::from(combined_high) ^ (r32(&SECRET, 8) ^ r32(&SECRET, 12));
            hash128(xxh64_avalanche(high), xxh64_avalanche(low))
        }
        4..=8 => {
            let input64 = r32(input, 0) | (r32(input, n - 4) << 32);
            let keyed = input64 ^ (r64(&SECRET, 16) ^ r64(&SECRET, 24));
            let (mut high, mut low) = mul128(keyed, PRIME64_1.wrapping_add(len << 2));
            high = high.wrapping_add(low << 1);
            low ^= high >> 3;
            low ^= low >> 35;
            low = low.wrapping_mul(PRIME_MX2);
            low ^= low >> 28;
            hash128(avalanche(high), low)
        }
        9..=16 => {
            let flip_low = r64(&SECRET, 32) ^ r64(&SECRET, 40);
            let flip_high = r64(&SECRET, 48) ^ r64(&SECRET, 56);
            let input_high = r64(input, n - 8);
            let (mut high, mut low) = mul128(r64(input, 0) ^ input_high ^ flip_low, PRIME64_1);
            low = low.wrapping_add((len - 1) << 54);
            let input_high = input_high ^ flip_high;
            high = high
                .wrapping_add(input_high)
                .wrapping_add(u64::from(input_high as u32).wrapping_mul(PRIME32_2 - 1));
            low ^= high.swap_bytes();
            let (h_high, h_low) = mul128(low, PRIME64_2);
            let h_high = h_high.wrapping_add(high.wrapping_mul(PRIME64_2));
            hash128(avalanche(h_high), avalanche(h_low))
        }
        17..=128 => {
            let mut acc = (len.wrapping_mul(PRIME64_1), 0);
            for i in (0..=(n - 1) / 32).rev() {
                acc = mix32(acc, input, 16 * i, n - 16 * (i + 1), 32 * i);
            }
            mid_final(acc, len)
        }
        _ => {
            let mut acc = (len.wrapping_mul(PRIME64_1), 0);
            for i in (32..160).step_by(32) {
                acc = mix32(acc, input, i - 32, i - 16, i - 32);
            }
            acc = (avalanche(acc.0), avalanche(acc.1));
            for i in (160..=n).step_by(32) {
                acc = mix32(acc, input, i - 32, i - 16, MIDSIZE_START_OFFSET + i - 160);
            }
            let secret_at = SECRET_SIZE_MIN - MIDSIZE_LAST_OFFSET - 16;
            acc = mix32(acc, input, n - 16, n - 32, secret_at);
            mid_final(acc, len)
        }
    }
}

/// The long path with the scalar kernels, for x86-64 without AVX2 and
/// for the tests.
#[cfg_attr(
    target_arch = "aarch64",
    allow(dead_code, reason = "aarch64 always has NEON; the tests use it")
)]
fn long_scalar(input: &[u8]) -> u128 {
    long(input, accumulate_scalar, scramble_scalar)
}

/// The number of stripes a kernel call processes, after checking the
/// precondition every kernel relies on: `input` is whole stripes, and
/// `secret` holds the 64-byte key of the last one, which starts at
/// `SECRET_CONSUME_RATE` bytes per stripe.
fn stripe_count(input: &[u8], secret: &[u8]) -> usize {
    let count = input.len() / STRIPE_LEN;
    assert!(input.len().is_multiple_of(STRIPE_LEN));
    assert!(count == 0 || secret.len() >= (count - 1) * SECRET_CONSUME_RATE + STRIPE_LEN);
    count
}

/// Accumulates every stripe of `input`; stripe `s` is keyed by `secret`
/// from byte `s * SECRET_CONSUME_RATE`.
fn accumulate_scalar(acc: &mut Acc, input: &[u8], secret: &[u8]) {
    for s in 0..stripe_count(input, secret) {
        for lane in 0..acc.len() {
            let data = r64(input, s * STRIPE_LEN + 8 * lane);
            let key = data ^ r64(secret, s * SECRET_CONSUME_RATE + 8 * lane);
            acc[lane ^ 1] = acc[lane ^ 1].wrapping_add(data);
            acc[lane] = acc[lane].wrapping_add((key & 0xFFFF_FFFF) * (key >> 32));
        }
    }
}

fn scramble_scalar(acc: &mut Acc, secret: &Stripe) {
    for (lane, a) in acc.iter_mut().enumerate() {
        *a = (*a ^ (*a >> 47) ^ r64(secret, 8 * lane)).wrapping_mul(PRIME32_1);
    }
}

fn merge(acc: &Acc, secret_at: usize, start: u64) -> u64 {
    let mut result = start;
    for i in 0..acc.len() / 2 {
        let at = secret_at + 16 * i;
        result = result.wrapping_add(mul128_fold64(
            acc[2 * i] ^ r64(&SECRET, at),
            acc[2 * i + 1] ^ r64(&SECRET, at + 8),
        ));
    }
    avalanche(result)
}

/// Inputs longer than `MIDSIZE_MAX`. Generic over the kernels so that each
/// SIMD module compiles one copy of the whole loop with its target features.
#[inline(always)]
fn long(
    input: &[u8],
    accumulate: impl Fn(&mut Acc, &[u8], &[u8]),
    scramble: impl Fn(&mut Acc, &Stripe),
) -> u128 {
    let n = input.len();
    assert!(n > MIDSIZE_MAX);
    let mut acc = INIT_ACC;
    let blocks = (n - 1) / BLOCK_LEN;
    for block in input[..blocks * BLOCK_LEN].chunks_exact(BLOCK_LEN) {
        accumulate(&mut acc, block, &SECRET);
        scramble(&mut acc, SECRET.last_chunk().unwrap());
    }
    // The stripes after the last whole block, but not the last byte...
    let tail = &input[blocks * BLOCK_LEN..n - 1];
    accumulate(
        &mut acc,
        &tail[..tail.len() / STRIPE_LEN * STRIPE_LEN],
        &SECRET,
    );
    // ...which the last stripe, ending at the input's end, always covers.
    let last_secret = SECRET.len() - STRIPE_LEN - SECRET_LAST_ACC_START;
    accumulate(&mut acc, &input[n - STRIPE_LEN..], &SECRET[last_secret..]);
    let len = n as u64;
    let low = merge(&acc, SECRET_MERGE_ACCS_START, len.wrapping_mul(PRIME64_1));
    let high_at = SECRET.len() - size_of::<Acc>() - SECRET_MERGE_ACCS_START;
    let high = merge(&acc, high_at, !len.wrapping_mul(PRIME64_2));
    hash128(high, low)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::FORMAT;

    /// The input of the vectors: byte `i` is `i * 31 + 7`, truncated.
    fn pattern(len: usize) -> Vec<u8> {
        (0..len as u64)
            .map(|i| i.wrapping_mul(31).wrapping_add(7) as u8)
            .collect()
    }

    /// XXH3_128bits of `pattern(len)`, from xxHash 0.8.3 (`xxhash.h`), the
    /// same with its scalar, NEON, SSE2 and AVX2 code paths. Covers every
    /// length class: 0, 1-3, 4-8, 9-16, 17-128, 129-240, and long inputs
    /// ending inside a stripe, on a stripe or block boundary, and after
    /// several blocks.
    const VECTORS: [(usize, u128); 42] = [
        (0, 0x99aa06d3014798d8_6001c324468d497f),
        (1, 0x495b62073ef70ca4_4c5cca45d0f4811f),
        (2, 0x12b2847aa0de5aaa_a7e250c97710ff27),
        (3, 0x46f66cb935381565_15f7093b173d005c),
        (4, 0x7fefeeffb4d0eab3_b987ca5d9241572a),
        (5, 0x2fbb16712b4bf1d5_752a86982353f4f3),
        (8, 0x803c675a846cc6c2_56bb836ceb6d4baa),
        (9, 0xd46556872d230f22_4376673580310154),
        (15, 0x2c612e00ed4f13d9_571df173bded2e23),
        (16, 0x650fe308c566747d_f853dd94614dfa07),
        (17, 0x18217300b5132d5a_78c349fe81b2f26c),
        (31, 0xa7591e70669b73f8_45e862e1ac921624),
        (32, 0x3220ff5fe507b3c0_5726e079716c6a62),
        (33, 0x91a4c56ad1b91d88_3b25275300c8b44e),
        (64, 0xf9bfa77da0891a96_36c5f7e547426bc4),
        (65, 0x5642c5d38e6e787d_d0d1d7884590a330),
        (96, 0x59861d1adb3e51a2_63451be079edd707),
        (97, 0x0912f66857975b13_fa4138b7dc44e45b),
        (127, 0x3117b681087b4ef4_8a02b1f75c556ac3),
        (128, 0xb4f87b99d2db8a51_1e04fad9f0cacb4d),
        (129, 0x6881633650cd8924_c51bc887976aef63),
        (159, 0x23d73d2cfa83c4b0_4710980f58432f1d),
        (160, 0xc000b788df6dbbc4_f661814e66697391),
        (161, 0x9ab20efd5075007b_3514442284da8d47),
        (191, 0xa738c169d152e2bc_07e4807e2b806ac0),
        (192, 0xd4479caa4bea0dce_d8d6fbd475016236),
        (200, 0x8d8629a1aef9ef90_60ea018811f9a437),
        (239, 0x75acb2ecd970b399_6df5c761356a5057),
        (240, 0xde57aab31e77a2ff_93e173833f75ab66),
        (241, 0x92b991a7192f3f08_0b3b630948ce4a00),
        (255, 0x3e68b7e415ce7e5c_89932170686cdd9a),
        (257, 0x0f849a4f3e33b6c2_12ef0ff633841459),
        (1000, 0xf534f51e82a81d29_989765d0ea7a5ecd),
        (1023, 0x8e8ed756aa1f01fa_f0d330ce2b3300fb),
        (1025, 0x70a4eb1b9691d77f_c09fdfbc398c7d82),
        (1088, 0xfcf9dfe2b0ba271b_a16bad67843c5a45),
        (2000, 0x6e49d9b3dde8ee92_19eb4cda7a0f1a0d),
        (4095, 0x939530bba8aafcc3_931f2730a6e6e594),
        (4097, 0xb074f6deb50fbb5e_b319759b4671c221),
        (10000, 0xd53e809be21e616d_441f01d9711bebed),
        (100000, 0x8ce7a24d31cd94b1_ccf90df7e7e37036),
        (131073, 0x32be7bc8b7759e55_b23d37ad8ddf88b5),
    ];

    #[test]
    fn vectors() {
        let input = pattern(VECTORS[VECTORS.len() - 1].0);
        for (len, want) in VECTORS {
            assert_eq!(checksum(&input[..len]), want, "length {len}");
            assert_eq!(checksum_scalar(&input[..len]), want, "length {len}, scalar");
        }
    }

    /// Every length from 0 to 5000, folded as `acc = acc * 3 ^ checksum`;
    /// the expected value is from the same reference.
    #[test]
    fn every_length_to_5000() {
        let input = pattern(5000);
        for f in [checksum, checksum_scalar] {
            let fold = (0..=input.len()).fold(0u128, |acc, n| acc.wrapping_mul(3) ^ f(&input[..n]));
            assert_eq!(fold, 0x48972a2beac8180e_b2c128dcb27c25cd);
        }
    }

    /// The faults of document 01 section 1 that a checksum must catch by
    /// itself: every single-bit flip of a header and of a sector, and a block
    /// torn at every sector boundary (new sectors, then old ones). The third,
    /// misdirection, is caught because the address is among the checksummed
    /// bytes, which is the format's job, not the function's.
    #[test]
    fn detects_bit_flips_and_torn_writes() {
        for size in [FORMAT.header_size, FORMAT.sector_size] {
            let mut data = pattern(size as usize);
            let mut seen = std::collections::BTreeSet::from([checksum(&data)]);
            for bit in 0..data.len() * 8 {
                data[bit / 8] ^= 1 << (bit % 8);
                assert!(seen.insert(checksum(&data)), "size {size}, bit {bit}");
                data[bit / 8] ^= 1 << (bit % 8);
            }
        }
        let (block, sector) = (FORMAT.grid_block_size as usize, FORMAT.sector_size as usize);
        let new = pattern(block);
        let old: Vec<u8> = new.iter().map(|b| !b).collect();
        for tear in (sector..block).step_by(sector) {
            let torn = [&new[..tear], &old[tear..]].concat();
            assert_ne!(checksum(&torn), checksum(&new), "torn at {tear}");
            assert_ne!(checksum(&torn), checksum(&old), "torn at {tear}");
        }
    }

    /// splitmix64: the test needs a fixed, seeded stream, not quality.
    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// The SIMD kernel and the scalar path agree on 1,000,000 random inputs of
    /// random length (log-uniform up to 4 blocks, so most are long inputs).
    #[test]
    fn simd_matches_scalar() {
        #[cfg(target_arch = "x86_64")]
        assert!(
            std::arch::is_x86_feature_detected!("avx2"),
            "this machine has no AVX2, so the AVX2 kernel would not be tested"
        );
        let seed = 0x0067_7261_6e69_7465; // "granite"
        let mut state = seed;
        let max = 4 * BLOCK_LEN;
        let mut input = vec![0u8; max];
        for case in 0..1_000_000 {
            let bits = next(&mut state) % u64::from(max.ilog2() + 1);
            let len = (next(&mut state) % (1 << bits)) as usize;
            for chunk in input[..len].chunks_mut(8) {
                chunk.copy_from_slice(&next(&mut state).to_le_bytes()[..chunk.len()]);
            }
            let bytes = &input[..len];
            assert_eq!(
                checksum(bytes),
                checksum_scalar(bytes),
                "seed {seed:#x}, case {case}, length {len}"
            );
        }
    }
}
