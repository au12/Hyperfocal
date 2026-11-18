//! Portable SIMD for the hot loops: they are written as plain fixed-width loops that the
//! compiler vectorises for the baseline instruction set (SSE2 on x86-64, NEON on
//! aarch64), and on x86-64 a second copy is compiled for AVX2 and picked at run time.
//! No floating-point contraction or reassociation is allowed, so both copies give
//! bit-identical results.

/// `dispatch!(pub fn name = imp(a: A, b: B) -> R);` declares `name`, which runs `imp` (an
/// `#[inline(always)]` function with the same signature) compiled for AVX2 when the
/// CPU has it.
macro_rules! dispatch {
    ($vis:vis fn $name:ident = $imp:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?) => {
        $vis fn $name($($arg: $ty),*) $(-> $ret)? {
            #[cfg(target_arch = "x86_64")]
            {
                #[target_feature(enable = "avx2")]
                fn avx2($($arg: $ty),*) $(-> $ret)? {
                    $imp($($arg),*)
                }
                if $crate::images::simd::avx2() {
                    // SAFETY: the CPU supports AVX2.
                    return unsafe { avx2($($arg),*) };
                }
            }
            $imp($($arg),*)
        }
    };
}
pub(crate) use dispatch;

/// Whether the AVX2 copies run (false when HYPERFOCAL_NO_AVX2 is set, for tests and
/// benchmarks of the baseline copy).
#[cfg(target_arch = "x86_64")]
pub fn avx2() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static S: AtomicU8 = AtomicU8::new(0);
    match S.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::arch::is_x86_feature_detected!("avx2") && std::env::var_os("HYPERFOCAL_NO_AVX2").is_none();
            S.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// The instruction set the hot loops run with (for the status page and logs).
pub fn level() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        if avx2() { "avx2" } else { "sse2" }
    }
    #[cfg(target_arch = "aarch64")]
    {
        "neon"
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        "scalar"
    }
}

/// `super::tone::f16_bits` without branches, so loops calling it vectorise; the same
/// results for every input (round half up, subnormals truncated, NaN and negatives to
/// 0, the top to the largest finite half).
#[inline(always)]
#[allow(clippy::manual_clamp)] // max() maps NaN to 0, which clamp() would keep
pub fn f16_bits(x: f32) -> u16 {
    let x = x.max(0.0).min(65504.0);
    let b = x.to_bits();
    let e = ((b >> 23) as i32 - 112).max(0) as u32;
    let normal = ((e << 10) | ((b >> 13) & 0x3ff)) + ((b >> 12) & 1);
    // SAFETY: x is in 0..=65504 and not NaN here, so the product is in 0..=1041 (an
    // unchecked conversion vectorises; a saturating one does not).
    let sub = unsafe { (x.min(6.2e-5) * 16_777_216.0).to_int_unchecked::<i32>() } as u32;
    (if x < 6.103_515_6e-5 { sub } else { normal }) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_free_f16_matches() {
        let check = |x: f32| assert_eq!(f16_bits(x), crate::images::tone::f16_bits(x), "{x:e} ({:#x})", x.to_bits());
        for x in [0.0f32, -0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e-30, 65504.0, 65519.0, 65520.0, 1e9] {
            check(x);
        }
        // Every half, its neighbours as f32, and the exact midpoints (ties).
        for h in 0..0x7c00u16 {
            let x = half::f16::from_bits(h).to_f32();
            let next = half::f16::from_bits(h + 1).to_f32();
            check(x);
            check(f32::from_bits(x.to_bits() + 1));
            if x > 0.0 {
                check(f32::from_bits(x.to_bits() - 1));
            }
            let mid = x + (next - x) / 2.0;
            check(mid);
            check(f32::from_bits(mid.to_bits() + 1));
            check(f32::from_bits(mid.to_bits() - 1));
        }
    }
}
