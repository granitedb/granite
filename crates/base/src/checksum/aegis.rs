//! AEGIS-128L as a MAC with a 128-bit tag and an all-zero key and nonce,
//! read as a little-endian u128: TigerBeetle's checksum, and the candidate
//! decision 0002 rejected. Test-only; kept so the benchmark can be rerun.

pub(super) trait Block: Copy {
    fn load(bytes: [u8; 16]) -> Self;
    fn bytes(self) -> [u8; 16];
    fn xor(self, other: Self) -> Self;
    /// One AES encryption round with `key` as the round key (x86 `aesenc`).
    fn round(self, key: Self) -> Self;
}

#[inline(always)]
fn mac<B: Block>(data: &[u8]) -> u128 {
    const C0: [u8; 16] = [
        0x00, 0x01, 0x01, 0x02, 0x03, 0x05, 0x08, 0x0d, 0x15, 0x22, 0x37, 0x59, 0x90, 0xe9, 0x79,
        0x62,
    ];
    const C1: [u8; 16] = [
        0xdb, 0x3d, 0x18, 0x55, 0x6d, 0xc2, 0x2f, 0xf1, 0x20, 0x11, 0x31, 0x42, 0x73, 0xb5, 0x28,
        0xdd,
    ];
    let (zero, c0, c1) = (B::load([0; 16]), B::load(C0), B::load(C1));
    // key ^ nonce, C1, C0, C1, key ^ nonce, key ^ C0, key ^ C1, key ^ C0
    let mut s = [zero, c1, c0, c1, zero, c0, c1, c0];
    let update = |s: &mut [B; 8], m0: B, m1: B| {
        let last = s[7];
        for i in (1..8).rev() {
            s[i] = s[i - 1].round(s[i]);
        }
        s[0] = last.round(s[0]).xor(m0);
        s[4] = s[4].xor(m1);
    };
    for _ in 0..10 {
        update(&mut s, zero, zero);
    }
    let mut chunks = data.chunks_exact(32);
    for c in &mut chunks {
        update(
            &mut s,
            B::load(*c.first_chunk().unwrap()),
            B::load(*c.last_chunk().unwrap()),
        );
    }
    if !chunks.remainder().is_empty() {
        let mut pad = [0u8; 32];
        pad[..chunks.remainder().len()].copy_from_slice(chunks.remainder());
        update(
            &mut s,
            B::load(*pad.first_chunk().unwrap()),
            B::load(*pad.last_chunk().unwrap()),
        );
    }
    let mut sizes = [0u8; 16];
    sizes[..8].copy_from_slice(&(data.len() as u64 * 8).to_le_bytes());
    sizes[8..].copy_from_slice(&128u64.to_le_bytes());
    let t = s[2].xor(B::load(sizes));
    for _ in 0..7 {
        update(&mut s, t, t);
    }
    u128::from_le_bytes(s[..7].iter().fold(zero, |a, b| a.xor(*b)).bytes())
}

/// With the CPU's AES instructions. Panics on a CPU without them.
pub(super) fn hardware(data: &[u8]) -> u128 {
    #[cfg(target_arch = "x86_64")]
    {
        assert!(std::arch::is_x86_feature_detected!("aes"), "no AES-NI");
        // SAFETY: the CPU supports AES-NI, checked on the line above.
        unsafe { x86::mac(data) }
    }
    #[cfg(target_arch = "aarch64")]
    {
        assert!(
            std::arch::is_aarch64_feature_detected!("aes"),
            "no AES instructions"
        );
        // SAFETY: the CPU supports the AES instructions, checked above.
        unsafe { arm::mac(data) }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    panic!("no AES instructions on this architecture: {}", data.len())
}

/// With AES in software, as on a CPU without AES instructions.
pub(super) fn software(data: &[u8]) -> u128 {
    mac::<[u8; 16]>(data)
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    #[target_feature(enable = "aes")]
    pub(super) fn mac(data: &[u8]) -> u128 {
        super::mac::<__m128i>(data)
    }

    /// Only used inside `mac`, which enables AES-NI; SSE2 is baseline.
    impl super::Block for __m128i {
        #[inline(always)]
        fn load(bytes: [u8; 16]) -> Self {
            // SAFETY: same size; every bit pattern is a valid vector. Not
            // `_mm_loadu_si128`, whose checked copy is slow with debug
            // assertions on (see `simd.rs`).
            unsafe { std::mem::transmute(bytes) }
        }
        #[inline(always)]
        fn bytes(self) -> [u8; 16] {
            // SAFETY: same size; every bit pattern is a valid byte array.
            unsafe { std::mem::transmute(self) }
        }
        #[inline(always)]
        fn xor(self, other: Self) -> Self {
            // SAFETY: SSE2 is part of the x86-64 baseline.
            unsafe { _mm_xor_si128(self, other) }
        }
        #[inline(always)]
        fn round(self, key: Self) -> Self {
            // SAFETY: only reached from `mac`, which enables AES-NI.
            unsafe { _mm_aesenc_si128(self, key) }
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use std::arch::aarch64::*;

    #[target_feature(enable = "aes")]
    pub(super) fn mac(data: &[u8]) -> u128 {
        super::mac::<uint8x16_t>(data)
    }

    /// Only used inside `mac`, which enables the AES instructions; NEON is
    /// baseline.
    impl super::Block for uint8x16_t {
        #[inline(always)]
        fn load(bytes: [u8; 16]) -> Self {
            // SAFETY: same size; every bit pattern is a valid vector. Not
            // `vld1q_u8`, whose checked copy is slow with debug
            // assertions on (see `simd.rs`).
            unsafe { std::mem::transmute(bytes) }
        }
        #[inline(always)]
        fn bytes(self) -> [u8; 16] {
            // SAFETY: same size; every bit pattern is a valid byte array.
            unsafe { std::mem::transmute(self) }
        }
        #[inline(always)]
        fn xor(self, other: Self) -> Self {
            // SAFETY: NEON is part of the aarch64 baseline.
            unsafe { veorq_u8(self, other) }
        }
        #[inline(always)]
        fn round(self, key: Self) -> Self {
            // SAFETY: only reached from `mac`, which enables the AES instructions.
            unsafe { aes_round(self, key) }
        }
    }

    /// The body of `round`, in a function that enables the AES instructions:
    /// their intrinsics are only inlined into one, and a safe trait method
    /// cannot enable target features. Linux does not enable them by default.
    #[target_feature(enable = "aes")]
    fn aes_round(state: uint8x16_t, key: uint8x16_t) -> uint8x16_t {
        // AESE with a zero key is SubBytes and ShiftRows; AESMC is MixColumns.
        veorq_u8(vaesmcq_u8(vaeseq_u8(state, vdupq_n_u8(0))), key)
    }
}

const fn xtime(a: u8) -> u8 {
    (a << 1) ^ if a & 0x80 != 0 { 0x1b } else { 0 }
}

const fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    p
}

/// The AES S-box: the inverse in GF(2^8) (x^254), then the affine map.
const SBOX: [u8; u8::MAX as usize + 1] = {
    let mut t = [0u8; u8::MAX as usize + 1];
    let mut x = 0;
    while x < t.len() {
        let (mut inv, mut base, mut e) = (1u8, x as u8, 254u8);
        while e > 0 {
            if e & 1 != 0 {
                inv = gmul(inv, base);
            }
            base = gmul(base, base);
            e >>= 1;
        }
        t[x] = inv
            ^ inv.rotate_left(1)
            ^ inv.rotate_left(2)
            ^ inv.rotate_left(3)
            ^ inv.rotate_left(4)
            ^ 0x63;
        x += 1;
    }
    t
};

/// Table-free software AES round on a column-major state.
impl Block for [u8; 16] {
    fn load(bytes: [u8; 16]) -> Self {
        bytes
    }
    fn bytes(self) -> [u8; 16] {
        self
    }
    fn xor(mut self, other: Self) -> Self {
        for (a, b) in self.iter_mut().zip(other) {
            *a ^= b;
        }
        self
    }
    fn round(self, key: Self) -> Self {
        let mut out = [0u8; 16];
        for c in 0..4 {
            // SubBytes and ShiftRows: row r moves left by r columns.
            let a: [u8; 4] = std::array::from_fn(|r| SBOX[self[4 * ((c + r) % 4) + r] as usize]);
            // MixColumns
            for r in 0..4 {
                out[4 * c + r] = xtime(a[r])
                    ^ xtime(a[(r + 1) % 4])
                    ^ a[(r + 1) % 4]
                    ^ a[(r + 2) % 4]
                    ^ a[(r + 3) % 4];
            }
        }
        out.xor(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Aegis128LMac_128` of Zig 0.17's standard library, all-zero key, over
    /// the same input pattern as the XXH3 vectors.
    const VECTORS: [(usize, u128); 14] = [
        (0, 0x635037dbb2b81e9bf114d2092a5aa1ad),
        (1, 0x1a5272dea30a1649d992fe95185bd5e3),
        (15, 0x60cf2085ba1720ffb9cb88b7183ca50c),
        (16, 0xcb9da39d86b2d926f6e305e02313a6d0),
        (17, 0x85a3dc570fc36abeec26075f1fc5a6e0),
        (31, 0x8b2aaa4059db9ddaeed052d0aa117ad9),
        (32, 0xd91f6a27ca4f2ca7a20bf723e6d01bfb),
        (33, 0xf9efecc2bb719468e92ffd20556cbc63),
        (63, 0x648e3898816c250ff1e541727afed849),
        (64, 0xb868bcbb6ec05c56d0c03876572b2781),
        (65, 0xb9e532cf244786a05eaa82ccdf635e07),
        (1000, 0x72115993c32060ab9fbb590c3e0ce24d),
        (4097, 0x4020a7452ca1bd62f264459cee70ed8a),
        (131073, 0x8fe5217a1397825846596c6be78d2813),
    ];

    #[test]
    fn vectors() {
        let input: Vec<u8> = (0..131073u64)
            .map(|i| i.wrapping_mul(31).wrapping_add(7) as u8)
            .collect();
        for (len, want) in VECTORS {
            assert_eq!(software(&input[..len]), want, "length {len}, software");
            assert_eq!(hardware(&input[..len]), want, "length {len}, hardware");
        }
    }
}
