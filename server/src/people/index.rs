//! Every usable face (and every dog, every cat) embedding in memory, as int8 vectors,
//! searched by brute force: 512 bytes a face, so 600,000 faces take about 300 MB, and
//! one search over them a few milliseconds on a NAS CPU (in parallel over the cores
//! for large spaces). Exact, deterministic and with nothing to rebuild; an approximate
//! index (HNSW) would only pay off at several million faces.

use super::DIM;
use rayon::prelude::*;
use std::collections::HashMap;

/// int8 with a per-vector scale (the largest component maps to 127). Cosine
/// similarities come out within about 0.002 of the float ones.
pub fn quantize(v: &[f32]) -> Vec<i8> {
    let m = v.iter().fold(0f32, |m, x| m.max(x.abs()));
    let s = if m > 0.0 { 127.0 / m } else { 0.0 };
    v.iter().map(|x| (x * s).round().clamp(-127.0, 127.0) as i8).collect()
}

/// Dot product of two int8 vectors of the same length: the inner loop of every search,
/// written out for AVX2 and SSE2 (the compiler's own vectorisation of the plain loop
/// widens to 32 bits first and runs several times slower).
#[inline]
pub fn dot(a: &[i8], b: &[i8]) -> i32 {
    debug_assert_eq!(a.len(), b.len());
    #[cfg(target_arch = "x86_64")]
    {
        if crate::images::simd::avx2() {
            // SAFETY: the CPU has AVX2.
            return unsafe { dot_avx2(a, b) };
        }
        // SAFETY: SSE2 is part of x86-64.
        unsafe { dot_sse2(a, b) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    dot_scalar(a, b)
}

#[allow(dead_code)]
#[inline(always)]
fn dot_scalar(a: &[i8], b: &[i8]) -> i32 {
    let mut acc = [0i32; 16];
    let n = a.len() / 16 * 16;
    for (x, y) in a[..n].chunks_exact(16).zip(b[..n].chunks_exact(16)) {
        for i in 0..16 {
            acc[i] += x[i] as i32 * y[i] as i32;
        }
    }
    acc.iter().sum::<i32>() + a[n..].iter().zip(&b[n..]).map(|(&x, &y)| x as i32 * y as i32).sum::<i32>()
}

/// 32 bytes at a time: sign-extend to 16 bits (vpmovsxbw) and multiply-add pairs into
/// 32-bit sums (vpmaddwd). Products of two int8 fit 16 bits, pairs of them 32.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dot_avx2(a: &[i8], b: &[i8]) -> i32 {
    use std::arch::x86_64::*;
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let mut acc0 = _mm256_setzero_si256();
    let mut acc1 = _mm256_setzero_si256();
    let mut i = 0;
    // SAFETY (all loads): i + 32 <= n.
    unsafe {
        while i + 32 <= n {
            let a0 = _mm256_cvtepi8_epi16(_mm_loadu_si128(pa.add(i) as *const __m128i));
            let b0 = _mm256_cvtepi8_epi16(_mm_loadu_si128(pb.add(i) as *const __m128i));
            let a1 = _mm256_cvtepi8_epi16(_mm_loadu_si128(pa.add(i + 16) as *const __m128i));
            let b1 = _mm256_cvtepi8_epi16(_mm_loadu_si128(pb.add(i + 16) as *const __m128i));
            acc0 = _mm256_add_epi32(acc0, _mm256_madd_epi16(a0, b0));
            acc1 = _mm256_add_epi32(acc1, _mm256_madd_epi16(a1, b1));
            i += 32;
        }
    }
    let acc = _mm256_add_epi32(acc0, acc1);
    let s = _mm_add_epi32(_mm256_castsi256_si128(acc), _mm256_extracti128_si256::<1>(acc));
    let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b01_00_11_10>(s));
    let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b10_11_00_01>(s));
    let mut r = _mm_cvtsi128_si32(s);
    while i < n {
        r += a[i] as i32 * b[i] as i32;
        i += 1;
    }
    r
}

/// 16 bytes at a time: sign-extend by unpacking with the sign masks, then pmaddwd.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn dot_sse2(a: &[i8], b: &[i8]) -> i32 {
    use std::arch::x86_64::*;
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let zero = _mm_setzero_si128();
    let mut acc0 = _mm_setzero_si128();
    let mut acc1 = _mm_setzero_si128();
    let mut i = 0;
    // SAFETY (all loads): i + 16 <= n.
    unsafe {
        while i + 16 <= n {
            let va = _mm_loadu_si128(pa.add(i) as *const __m128i);
            let vb = _mm_loadu_si128(pb.add(i) as *const __m128i);
            let sa = _mm_cmpgt_epi8(zero, va);
            let sb = _mm_cmpgt_epi8(zero, vb);
            let a0 = _mm_unpacklo_epi8(va, sa);
            let a1 = _mm_unpackhi_epi8(va, sa);
            let b0 = _mm_unpacklo_epi8(vb, sb);
            let b1 = _mm_unpackhi_epi8(vb, sb);
            acc0 = _mm_add_epi32(acc0, _mm_madd_epi16(a0, b0));
            acc1 = _mm_add_epi32(acc1, _mm_madd_epi16(a1, b1));
            i += 16;
        }
    }
    let s = _mm_add_epi32(acc0, acc1);
    let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b01_00_11_10>(s));
    let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b10_11_00_01>(s));
    let mut r = _mm_cvtsi128_si32(s);
    while i < n {
        r += a[i] as i32 * b[i] as i32;
        i += 1;
    }
    r
}

pub fn norm(v: &[i8]) -> f32 {
    (dot(v, v) as f32).sqrt()
}

/// Cosine similarity of two int8 vectors.
pub fn cosine(a: &[i8], b: &[i8]) -> f32 {
    let d = norm(a) * norm(b);
    if d > 0.0 { dot(a, b) as f32 / d } else { 0.0 }
}

pub fn from_blob(b: &[u8]) -> Option<Vec<i8>> {
    (b.len() == DIM).then(|| b.iter().map(|&x| x as i8).collect())
}

pub fn to_blob(v: &[i8]) -> Vec<u8> {
    v.iter().map(|&x| x as u8).collect()
}

#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub slot: usize,
    pub sim: f32,
}

/// One embedding space (faces, or animals).
#[derive(Default)]
pub struct Space {
    ids: Vec<i64>,
    photos: Vec<i64>,
    vecs: Vec<i8>,
    norms: Vec<f32>,
    quality: Vec<f32>,
    subject: Vec<i64>,
    confirmed: Vec<bool>,
    pos: HashMap<i64, usize>,
}

impl Space {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn contains(&self, id: i64) -> bool {
        self.pos.contains_key(&id)
    }

    pub fn slot(&self, id: i64) -> Option<usize> {
        self.pos.get(&id).copied()
    }

    pub fn id(&self, slot: usize) -> i64 {
        self.ids[slot]
    }

    pub fn photo(&self, slot: usize) -> i64 {
        self.photos[slot]
    }

    pub fn quality(&self, slot: usize) -> f32 {
        self.quality[slot]
    }

    /// 0: none.
    pub fn subject(&self, slot: usize) -> i64 {
        self.subject[slot]
    }

    pub fn confirmed(&self, slot: usize) -> bool {
        self.confirmed[slot]
    }

    pub fn vector(&self, slot: usize) -> &[i8] {
        &self.vecs[slot * DIM..(slot + 1) * DIM]
    }

    pub fn insert(&mut self, id: i64, photo: i64, v: &[i8], quality: f32, subject: i64, confirmed: bool) {
        debug_assert_eq!(v.len(), DIM);
        if let Some(s) = self.slot(id) {
            self.vecs[s * DIM..(s + 1) * DIM].copy_from_slice(v);
            self.norms[s] = norm(v);
            self.quality[s] = quality;
            self.photos[s] = photo;
            self.subject[s] = subject;
            self.confirmed[s] = confirmed;
            return;
        }
        self.pos.insert(id, self.ids.len());
        self.ids.push(id);
        self.photos.push(photo);
        self.vecs.extend_from_slice(v);
        self.norms.push(norm(v));
        self.quality.push(quality);
        self.subject.push(subject);
        self.confirmed.push(confirmed);
    }

    pub fn remove(&mut self, id: i64) {
        let Some(s) = self.pos.remove(&id) else { return };
        let last = self.ids.len() - 1;
        if s != last {
            let moved = self.ids[last];
            self.ids.swap(s, last);
            self.photos.swap(s, last);
            self.norms.swap(s, last);
            self.quality.swap(s, last);
            self.subject.swap(s, last);
            self.confirmed.swap(s, last);
            let (a, b) = self.vecs.split_at_mut(last * DIM);
            a[s * DIM..(s + 1) * DIM].copy_from_slice(&b[..DIM]);
            self.pos.insert(moved, s);
        }
        self.ids.pop();
        self.photos.pop();
        self.norms.pop();
        self.quality.pop();
        self.subject.pop();
        self.confirmed.pop();
        self.vecs.truncate(last * DIM);
    }

    pub fn set_label(&mut self, id: i64, subject: i64, confirmed: bool) {
        if let Some(s) = self.slot(id) {
            self.subject[s] = subject;
            self.confirmed[s] = confirmed;
        }
    }

    /// Relabel every member of a subject (a merge, or a subject removed: `to` 0).
    pub fn relabel_subject(&mut self, from: i64, to: i64) {
        for s in self.subject.iter_mut() {
            if *s == from {
                *s = to;
            }
        }
        if to == 0 {
            for (i, s) in self.subject.iter().enumerate() {
                if *s == 0 {
                    self.confirmed[i] = false;
                }
            }
        }
    }

    pub fn members(&self, subject: i64) -> impl Iterator<Item = usize> + '_ {
        self.subject.iter().enumerate().filter(move |(_, s)| **s == subject).map(|(i, _)| i)
    }

    /// The `k` most similar vectors with a cosine similarity of at least `floor`, best
    /// first.
    pub fn search(&self, q: &[i8], floor: f32, k: usize) -> Vec<Hit> {
        let qn = norm(q);
        if qn == 0.0 || self.is_empty() {
            return Vec::new();
        }
        let scan = |range: std::ops::Range<usize>| -> Vec<Hit> {
            let mut out = Vec::new();
            for i in range {
                let d = dot(q, &self.vecs[i * DIM..(i + 1) * DIM]) as f32;
                let den = qn * self.norms[i];
                if den > 0.0 && d >= floor * den {
                    out.push(Hit { slot: i, sim: d / den });
                }
            }
            out
        };
        let n = self.len();
        let mut hits = if n > 32_768 {
            const CHUNK: usize = 8192;
            (0..n.div_ceil(CHUNK)).into_par_iter().flat_map_iter(|c| scan(c * CHUNK..((c + 1) * CHUNK).min(n))).collect()
        } else {
            scan(0..n)
        };
        hits.sort_by(|a, b| b.sim.total_cmp(&a.sim).then(a.slot.cmp(&b.slot)));
        hits.truncate(k);
        hits
    }

    /// `search` for many queries at once (the same results): each stored vector is read
    /// once for all of them, so a batch costs about one pass over memory instead of
    /// one per query. In parallel over the stored vectors.
    pub fn search_many(&self, qs: &[&[i8]], floor: f32, k: usize) -> Vec<Vec<Hit>> {
        let nq = qs.len();
        if nq == 0 || self.is_empty() {
            return vec![Vec::new(); nq];
        }
        let qn: Vec<f32> = qs.iter().map(|q| norm(q)).collect();
        let n = self.len();
        const CHUNK: usize = 4096;
        let parts: Vec<Vec<Vec<Hit>>> = (0..n.div_ceil(CHUNK))
            .into_par_iter()
            .map(|c| {
                let mut out: Vec<Vec<Hit>> = vec![Vec::new(); nq];
                for i in c * CHUNK..((c + 1) * CHUNK).min(n) {
                    let v = &self.vecs[i * DIM..(i + 1) * DIM];
                    let vn = self.norms[i];
                    for (j, q) in qs.iter().enumerate() {
                        let den = qn[j] * vn;
                        if den <= 0.0 {
                            continue;
                        }
                        let d = dot(q, v) as f32;
                        if d >= floor * den {
                            out[j].push(Hit { slot: i, sim: d / den });
                        }
                    }
                }
                out
            })
            .collect();
        let mut res: Vec<Vec<Hit>> = vec![Vec::new(); nq];
        for part in parts {
            for (j, h) in part.into_iter().enumerate() {
                res[j].extend(h);
            }
        }
        for h in res.iter_mut() {
            h.sort_by(|a, b| b.sim.total_cmp(&a.sim).then(a.slot.cmp(&b.slot)));
            h.truncate(k);
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(seed: u64) -> Vec<f32> {
        let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
        let mut v: Vec<f32> = (0..DIM)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x % 2000) as f32 / 1000.0 - 1.0
            })
            .collect();
        let n = v.iter().map(|a| a * a).sum::<f32>().sqrt();
        v.iter_mut().for_each(|a| *a /= n);
        v
    }

    #[test]
    fn quantized_cosine_is_close() {
        let (a, b) = (unit(1), unit(2));
        let mix: Vec<f32> = a.iter().zip(&b).map(|(x, y)| 0.8 * x + 0.2 * y).collect();
        let exact = |u: &[f32], v: &[f32]| {
            let d: f32 = u.iter().zip(v).map(|(x, y)| x * y).sum();
            d / (u.iter().map(|x| x * x).sum::<f32>().sqrt() * v.iter().map(|x| x * x).sum::<f32>().sqrt())
        };
        for (u, v) in [(&a, &b), (&a, &mix), (&mix, &b)] {
            let e = exact(u, v);
            let q = cosine(&quantize(u), &quantize(v));
            assert!((e - q).abs() < 0.01, "{e} vs {q}");
        }
        assert!((cosine(&quantize(&a), &quantize(&a)) - 1.0).abs() < 1e-5);
        assert_eq!(from_blob(&to_blob(&quantize(&a))).unwrap(), quantize(&a));
    }

    #[test]
    fn kernels_agree() {
        let mut x: u64 = 7;
        for len in [0usize, 1, 15, 16, 31, 32, 33, 512, 515] {
            let mut gen_v = || -> Vec<i8> {
                (0..len)
                    .map(|_| {
                        x ^= x << 13;
                        x ^= x >> 7;
                        x ^= x << 17;
                        (x % 256) as u8 as i8
                    })
                    .collect()
            };
            let (a, b) = (gen_v(), gen_v());
            let want = dot_scalar(&a, &b);
            assert_eq!(dot(&a, &b), want, "len {len}");
            #[cfg(target_arch = "x86_64")]
            unsafe {
                assert_eq!(dot_sse2(&a, &b), want, "sse2 len {len}");
                if std::arch::is_x86_feature_detected!("avx2") {
                    assert_eq!(dot_avx2(&a, &b), want, "avx2 len {len}");
                }
            }
        }
        // The extremes: -128 * -128 everywhere.
        let a = vec![-128i8; 512];
        assert_eq!(dot(&a, &a), 512 * 16384);
    }

    #[test]
    fn search_insert_remove() {
        let mut s = Space::default();
        for i in 0..100 {
            s.insert(i, i / 2, &quantize(&unit(i as u64 + 10)), 0.5, 0, false);
        }
        let q = quantize(&unit(42 + 10));
        let hits = s.search(&q, 0.5, 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(s.id(hits[0].slot), 42);
        assert!(hits[0].sim > 0.99);
        s.remove(42);
        assert!(!s.contains(42));
        assert!(s.search(&q, 0.5, 5).is_empty());
        // The vector moved into the freed slot is still found under its id.
        let q99 = quantize(&unit(99 + 10));
        let h = s.search(&q99, 0.5, 1);
        assert_eq!(s.id(h[0].slot), 99);
        assert_eq!(s.photo(h[0].slot), 49);
        s.set_label(99, 7, true);
        assert_eq!(s.members(7).count(), 1);
        s.relabel_subject(7, 0);
        assert_eq!(s.members(7).count(), 0);
        assert_eq!(s.len(), 99);
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// `cargo test --release --lib people::index::bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn search_600k() {
        let n = 600_000;
        let mut s = Space::default();
        let mut x: u64 = 0x9E3779B97F4A7C15;
        let mut v = vec![0i8; DIM];
        for i in 0..n {
            for e in v.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *e = (x % 255) as i8;
            }
            s.insert(i as i64, i as i64 / 2, &v, 0.8, 0, false);
        }
        let q = v.clone();
        let t = std::time::Instant::now();
        let runs = 50;
        let mut found = 0;
        for _ in 0..runs {
            found += s.search(&q, 0.37, 64).len();
        }
        let per = t.elapsed().as_secs_f64() / runs as f64;
        println!("{n} vectors: {:.2} ms a search ({} level), {found} hits", per * 1000.0, crate::images::simd::level());
        // Batched: 64 queries a pass.
        let qs: Vec<Vec<i8>> = (0..64).map(|i| s.vector(i * 1000).to_vec()).collect();
        let refs: Vec<&[i8]> = qs.iter().map(|q| q.as_slice()).collect();
        let t = std::time::Instant::now();
        let r = s.search_many(&refs, 0.37, 64);
        let per = t.elapsed().as_secs_f64() / refs.len() as f64;
        println!("batched: {:.2} ms a query", per * 1000.0);
        for (j, q) in refs.iter().enumerate().take(4) {
            let one = s.search(q, 0.37, 64);
            assert_eq!(one.iter().map(|h| h.slot).collect::<Vec<_>>(), r[j].iter().map(|h| h.slot).collect::<Vec<_>>());
        }
    }
}
