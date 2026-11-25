//! Lock-free latency histograms: four buckets per doubling (about 19% wide), from a
//! microsecond to an hour and more. Percentiles read from them are within a bucket of the
//! truth, which is plenty for telling a 40 ms request from a 400 ms one.

use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

pub const BUCKETS: usize = 128;

/// The bucket of a duration in microseconds: 0-7 exact, then four per power of two (the
/// browser's telemetry uses the same buckets: web/src/lib/telemetry.ts).
pub fn bucket(us: u64) -> usize {
    if us < 8 {
        return us as usize;
    }
    let log = 63 - us.leading_zeros() as usize;
    let sub = ((us >> (log - 2)) & 3) as usize;
    (8 + (log - 3) * 4 + sub).min(BUCKETS - 1)
}

/// The lowest duration (µs) that falls in bucket `i`.
pub fn bucket_low(i: usize) -> u64 {
    if i < 8 {
        return i as u64;
    }
    let (log, sub) = ((i - 8) / 4 + 3, ((i - 8) % 4) as u64);
    (4 + sub) << (log - 2)
}

/// A representative duration (µs) of bucket `i`: the middle of its range.
pub fn bucket_mid(i: usize) -> u64 {
    if i < 8 {
        return i as u64;
    }
    let lo = bucket_low(i);
    let hi = if i + 1 < BUCKETS { bucket_low(i + 1) } else { lo * 5 / 4 };
    (lo + hi) / 2
}

pub struct Hist {
    n: AtomicU64,
    err: AtomicU64,
    sum: AtomicU64,
    max: AtomicU64,
    /// The largest since the sampler last looked (see [`Hist::take_window_max`]).
    wmax: AtomicU64,
    b: [AtomicU64; BUCKETS],
}

impl Default for Hist {
    fn default() -> Self {
        Hist::new()
    }
}

impl Hist {
    pub const fn new() -> Hist {
        Hist {
            n: AtomicU64::new(0),
            err: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            max: AtomicU64::new(0),
            wmax: AtomicU64::new(0),
            b: [const { AtomicU64::new(0) }; BUCKETS],
        }
    }

    pub fn add(&self, us: u64, err: bool) {
        self.n.fetch_add(1, Ordering::Relaxed);
        if err {
            self.err.fetch_add(1, Ordering::Relaxed);
        }
        self.sum.fetch_add(us, Ordering::Relaxed);
        self.max.fetch_max(us, Ordering::Relaxed);
        self.wmax.fetch_max(us, Ordering::Relaxed);
        self.b[bucket(us)].fetch_add(1, Ordering::Relaxed);
    }

    /// Adds counts read elsewhere (a browser's report): `buckets` as (index, count).
    pub fn merge(&self, n: u64, err: u64, sum: u64, max: u64, buckets: impl Iterator<Item = (usize, u64)>) {
        self.n.fetch_add(n, Ordering::Relaxed);
        self.err.fetch_add(err, Ordering::Relaxed);
        self.sum.fetch_add(sum, Ordering::Relaxed);
        self.max.fetch_max(max, Ordering::Relaxed);
        self.wmax.fetch_max(max, Ordering::Relaxed);
        for (i, c) in buckets {
            if i < BUCKETS {
                self.b[i].fetch_add(c, Ordering::Relaxed);
            }
        }
    }

    pub fn count(&self) -> u64 {
        self.n.load(Ordering::Relaxed)
    }

    /// The largest duration since the last call (the sampler's window).
    pub fn take_window_max(&self) -> u64 {
        self.wmax.swap(0, Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> Snap {
        let mut b = [0u64; BUCKETS];
        for (d, s) in b.iter_mut().zip(&self.b) {
            *d = s.load(Ordering::Relaxed);
        }
        Snap {
            n: self.n.load(Ordering::Relaxed),
            err: self.err.load(Ordering::Relaxed),
            sum: self.sum.load(Ordering::Relaxed),
            max: self.max.load(Ordering::Relaxed),
            b,
        }
    }
}

/// A histogram's counts at one moment (or the difference between two moments).
#[derive(Clone)]
pub struct Snap {
    pub n: u64,
    pub err: u64,
    /// Total µs.
    pub sum: u64,
    /// Largest µs (since the start; for a difference, set by the caller).
    pub max: u64,
    pub b: [u64; BUCKETS],
}

impl Default for Snap {
    fn default() -> Self {
        Snap { n: 0, err: 0, sum: 0, max: 0, b: [0; BUCKETS] }
    }
}

impl Snap {
    /// What happened between `prev` and this snapshot.
    pub fn since(&self, prev: &Snap, window_max: u64) -> Snap {
        let mut b = [0u64; BUCKETS];
        for (i, d) in b.iter_mut().enumerate() {
            *d = self.b[i].saturating_sub(prev.b[i]);
        }
        Snap { n: self.n.saturating_sub(prev.n), err: self.err.saturating_sub(prev.err), sum: self.sum.saturating_sub(prev.sum), max: window_max, b }
    }

    /// The `q` quantile (0..1) in µs.
    pub fn pct(&self, q: f64) -> u64 {
        let total: u64 = self.b.iter().sum();
        if total == 0 {
            return 0;
        }
        let want = ((total as f64) * q).ceil().max(1.0) as u64;
        let mut acc = 0;
        for (i, c) in self.b.iter().enumerate() {
            acc += c;
            if acc >= want {
                // In the top bucket the largest value seen is the better answer; elsewhere
                // the bucket's middle, never above that largest value.
                if acc == total && self.max > 0 {
                    return self.max;
                }
                let v = bucket_mid(i);
                return if self.max > 0 { v.min(self.max) } else { v };
            }
        }
        self.max
    }

    /// Compact JSON in milliseconds: count, errors, total, mean and percentiles.
    pub fn json(&self) -> Value {
        let ms = |us: u64| (us as f64 / 100.0).round() / 10.0;
        let mut v = json!({
            "n": self.n,
            "ms": ms(self.sum),
            "avg": if self.n > 0 { ms(self.sum / self.n) } else { 0.0 },
            "p50": ms(self.pct(0.5)),
            "p95": ms(self.pct(0.95)),
            "p99": ms(self.pct(0.99)),
            "max": ms(self.max),
        });
        if self.err > 0 {
            v["err"] = self.err.into();
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_ordered_and_cover_their_values() {
        let mut last = 0;
        for us in [0u64, 1, 7, 8, 9, 15, 16, 100, 999, 1000, 65_535, 1_000_000, 3_600_000_000] {
            let b = bucket(us);
            assert!(b >= last, "{us}");
            last = b;
            assert!(bucket_low(b) <= us, "{us}: low {}", bucket_low(b));
            if b + 1 < BUCKETS && us < 1 << 31 {
                assert!(bucket_low(b + 1) > us, "{us}");
            }
        }
        // About 19% wide.
        let b = bucket(10_000);
        let w = bucket_low(b + 1) - bucket_low(b);
        assert!(w as f64 / bucket_low(b) as f64 <= 0.26);
    }

    #[test]
    fn percentiles_are_close() {
        let h = Hist::new();
        for i in 1..=1000u64 {
            h.add(i * 1000, i % 100 == 0);
        }
        let s = h.snapshot();
        assert_eq!(s.n, 1000);
        assert_eq!(s.err, 10);
        let p50 = s.pct(0.5) as f64;
        let p95 = s.pct(0.95) as f64;
        assert!((p50 - 500_000.0).abs() / 500_000.0 < 0.15, "{p50}");
        assert!((p95 - 950_000.0).abs() / 950_000.0 < 0.15, "{p95}");
        assert_eq!(s.pct(1.0), 1_000_000);
        let j = s.json();
        assert_eq!(j["n"], 1000);
        assert_eq!(j["max"], 1000.0);
        // A window.
        let before = h.snapshot();
        h.take_window_max();
        h.add(5000, false);
        let w = h.snapshot().since(&before, h.take_window_max());
        assert_eq!(w.n, 1);
        assert_eq!(w.max, 5000);
        assert_eq!(w.pct(0.5), 5000);
    }
}
