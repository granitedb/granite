//! Rejected: a fused multiply-add rounds once, so its result differs from
//! `a * b + c` and from machines that evaluate it in software.

pub fn fma(a: f64, b: f64, c: f64) -> f64 {
    a.mul_add(b, c)
}

pub fn fma32(a: f32, b: f32, c: f32) -> f32 {
    a.mul_add(b, c)
}
