//! SIMD kernels of the XXH3 long path: AVX2 on x86-64 (chosen at run time),
//! NEON on aarch64 (always present). Each computes exactly what
//! `accumulate_scalar` and `scramble_scalar` compute, lane for lane, and
//! keeps the accumulator in registers across a run of stripes.
//!
//! Loads are plain reads of byte arrays transmuted to vectors, not
//! `_mm256_loadu_si256` or `vld1q_u8`: those copy through
//! `copy_nonoverlapping`, whose precondition checks stay on in release builds
//! (debug assertions are enabled) and made the kernels three times slower.

use super::{Acc, PRIME32_1, SECRET_CONSUME_RATE, STRIPE_LEN, Stripe, stripe_count};
use std::mem::transmute;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[cfg(target_arch = "x86_64")]
pub(super) fn long(input: &[u8]) -> u128 {
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2, checked on the line above.
        unsafe { long_avx2(input) }
    } else {
        // shortcut: x86-64 without AVX2 runs the scalar loop; production
        // requires AVX2 (decision 0002). Add an SSE2 kernel if that changes.
        super::long_scalar(input)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn long_avx2(input: &[u8]) -> u128 {
    super::long(input, accumulate_avx2, scramble_avx2)
}

/// 32 bytes at `p`.
///
/// # Safety
/// The 32 bytes at `p` are readable.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn load32(p: *const u8) -> __m256i {
    // SAFETY: readable by the contract; `[u8; 32]` has alignment 1, and
    // every bit pattern is a valid vector of the same size.
    unsafe { transmute::<[u8; 32], __m256i>(*p.cast()) }
}

/// The accumulator as two vectors of four lanes, and back.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn acc_avx2(acc: &Acc) -> [__m256i; 2] {
    // SAFETY: same size; every bit pattern is a valid vector.
    unsafe { transmute::<Acc, [__m256i; 2]>(*acc) }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn acc_from_avx2(v: [__m256i; 2]) -> Acc {
    // SAFETY: same size; every bit pattern is a valid u64.
    unsafe { transmute::<[__m256i; 2], Acc>(v) }
}

/// Only called from `long_avx2`, into which it is inlined, so AVX2 is
/// enabled wherever it runs.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn accumulate_avx2(acc: &mut Acc, input: &[u8], secret: &[u8]) {
    let count = stripe_count(input, secret);
    let mut v = acc_avx2(acc);
    for s in 0..count {
        for (i, a) in v.iter_mut().enumerate() {
            // SAFETY: AVX2 is enabled (see above). `stripe_count` checked
            // that stripe s is the 64 bytes of `input` at 64 * s and its key
            // the 64 bytes of `secret` at 8 * s; i < 2 reads one half of each.
            unsafe {
                let data = load32(input.as_ptr().add(STRIPE_LEN * s + 32 * i));
                let key = load32(secret.as_ptr().add(SECRET_CONSUME_RATE * s + 32 * i));
                let data_key = _mm256_xor_si256(data, key);
                // low 32 bits times high 32 bits of each lane
                let product = _mm256_mul_epu32(data_key, _mm256_srli_epi64::<32>(data_key));
                // swap the 64-bit lanes of each 128-bit half: acc[lane ^ 1] += data
                // (0b0100_1110 is _MM_SHUFFLE(1, 0, 3, 2))
                let swapped = _mm256_shuffle_epi32::<0b0100_1110>(data);
                *a = _mm256_add_epi64(product, _mm256_add_epi64(*a, swapped));
            }
        }
    }
    *acc = acc_from_avx2(v);
}

/// Only called from `long_avx2`; see `accumulate_avx2`.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn scramble_avx2(acc: &mut Acc, secret: &Stripe) {
    let mut v = acc_avx2(acc);
    for (i, a) in v.iter_mut().enumerate() {
        // SAFETY: AVX2 is enabled (see `accumulate_avx2`). `secret` is 64
        // bytes, so 32 bytes at 32 * i with i < 2 are readable.
        unsafe {
            let key = load32(secret.as_ptr().add(32 * i));
            let data_key = _mm256_xor_si256(_mm256_xor_si256(*a, _mm256_srli_epi64::<47>(*a)), key);
            // a 64x32-bit multiply from two 32x32-bit ones
            let prime = _mm256_set1_epi32(PRIME32_1 as i32);
            let low = _mm256_mul_epu32(data_key, prime);
            let high = _mm256_mul_epu32(_mm256_srli_epi64::<32>(data_key), prime);
            *a = _mm256_add_epi64(low, _mm256_slli_epi64::<32>(high));
        }
    }
    *acc = acc_from_avx2(v);
}

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

#[cfg(target_arch = "aarch64")]
pub(super) fn long(input: &[u8]) -> u128 {
    super::long(input, accumulate_neon, scramble_neon)
}

/// 16 bytes at `p`.
///
/// # Safety
/// The 16 bytes at `p` are readable.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn load16(p: *const u8) -> uint64x2_t {
    // SAFETY: readable by the contract; `[u8; 16]` has alignment 1, and
    // every bit pattern is a valid vector of the same size.
    unsafe { transmute::<[u8; 16], uint64x2_t>(*p.cast()) }
}

/// The accumulator as four vectors of two lanes, and back.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn acc_neon(acc: &Acc) -> [uint64x2_t; 4] {
    // SAFETY: same size; every bit pattern is a valid vector.
    unsafe { transmute::<Acc, [uint64x2_t; 4]>(*acc) }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn acc_from_neon(v: [uint64x2_t; 4]) -> Acc {
    // SAFETY: same size; every bit pattern is a valid u64.
    unsafe { transmute::<[uint64x2_t; 4], Acc>(v) }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn accumulate_neon(acc: &mut Acc, input: &[u8], secret: &[u8]) {
    let count = stripe_count(input, secret);
    let mut v = acc_neon(acc);
    for s in 0..count {
        for (i, a) in v.iter_mut().enumerate() {
            // SAFETY: NEON is part of the aarch64 baseline. `stripe_count`
            // checked that stripe s is the 64 bytes of `input` at 64 * s and
            // its key the 64 bytes of `secret` at 8 * s; i < 4 reads one
            // quarter of each.
            unsafe {
                let data = load16(input.as_ptr().add(STRIPE_LEN * s + 16 * i));
                let key = load16(secret.as_ptr().add(SECRET_CONSUME_RATE * s + 16 * i));
                let data_key = veorq_u64(data, key);
                // acc[lane ^ 1] += data
                let sum = vaddq_u64(*a, vextq_u64::<1>(data, data));
                // += low 32 bits times high 32 bits of each lane
                *a = vmlal_u32(sum, vmovn_u64(data_key), vshrn_n_u64::<32>(data_key));
            }
        }
    }
    *acc = acc_from_neon(v);
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn scramble_neon(acc: &mut Acc, secret: &Stripe) {
    let mut v = acc_neon(acc);
    for (i, a) in v.iter_mut().enumerate() {
        // SAFETY: NEON is part of the aarch64 baseline. `secret` is 64
        // bytes, so 16 bytes at 16 * i with i < 4 are readable.
        unsafe {
            let key = load16(secret.as_ptr().add(16 * i));
            let data_key = veorq_u64(veorq_u64(*a, vshrq_n_u64::<47>(*a)), key);
            // a 64x32-bit multiply from two 32x32-bit ones
            let prime = vdup_n_u32(PRIME32_1 as u32);
            let high = vshlq_n_u64::<32>(vmull_u32(vshrn_n_u64::<32>(data_key), prime));
            *a = vmlal_u32(high, vmovn_u64(data_key), prime);
        }
    }
    *acc = acc_from_neon(v);
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(super) fn long(input: &[u8]) -> u128 {
    super::long_scalar(input)
}
