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

// ---------------------------------------------------------------- sign codes

/// Above this many vectors in a space, batched searches compare sign codes first
/// (`Space::search_batch`): about six times faster at 600,000 faces, and the same matches
/// but for about one neighbour in 2,000 right at the floor.
pub const CODED_FROM: usize = 50_000;

/// 64-bit words of a sign code: one bit per dimension.
pub const WORDS: usize = DIM / 64;

/// A random rotation's sign flips (a fixed seed: codes are comparable across runs).
fn flips(seed: u64) -> [f32; DIM] {
    let mut x = seed;
    std::array::from_fn(|_| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        if x & 1 == 0 { 1.0 } else { -1.0 }
    })
}

fn fwht(x: &mut [f32; DIM]) {
    // The first three rounds within each run of eight, then wider ones over contiguous
    // halves (which the compiler turns into vector instructions).
    for c in x.chunks_exact_mut(8) {
        let mut v: [f32; 8] = [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]];
        for h in [1, 2, 4] {
            for i in (0..8).step_by(h * 2) {
                for j in i..i + h {
                    let (a, b) = (v[j], v[j + h]);
                    v[j] = a + b;
                    v[j + h] = a - b;
                }
            }
        }
        c.copy_from_slice(&v);
    }
    let mut h = 8;
    while h < DIM {
        for blk in x.chunks_exact_mut(2 * h) {
            let (lo, hi) = blk.split_at_mut(h);
            for (a, b) in lo.iter_mut().zip(hi.iter_mut()) {
                let t = *a;
                *a = t + *b;
                *b = t - *b;
            }
        }
        h *= 2;
    }
}

/// A vector's sign code (SimHash, Charikar 2002): the signs of its components after a
/// random rotation (two rounds of random sign flips and a Walsh-Hadamard transform).
/// The share of bits two codes differ in estimates the angle between the vectors:
/// cos ≈ cos(π · differing / 512), within about ±0.06 at the matching floor. Comparing
/// codes takes eight XOR-and-count instructions instead of 512 multiplications, so a
/// search first keeps the vectors whose codes are near enough (with a safe margin) and
/// then checks only those exactly.
pub fn sign_code(v: &[i8]) -> [u64; WORDS] {
    use std::sync::OnceLock;
    static F: OnceLock<([f32; DIM], [f32; DIM])> = OnceLock::new();
    let (f1, f2) = F.get_or_init(|| (flips(0x9E37_79B9_7F4A_7C15), flips(0xD1B5_4A32_D192_ED03)));
    let mut x: [f32; DIM] = std::array::from_fn(|i| v[i] as f32 * f1[i]);
    fwht(&mut x);
    for (a, f) in x.iter_mut().zip(f2) {
        *a *= f;
    }
    fwht(&mut x);
    let mut out = [0u64; WORDS];
    for (i, a) in x.iter().enumerate() {
        if *a > 0.0 {
            out[i / 64] |= 1 << (i % 64);
        }
    }
    out
}

/// The most differing bits of two codes whose vectors may still have a cosine of at
/// least `floor`: the bits the angle of `floor` gives, plus 3.3 standard deviations of
/// the estimate (a true neighbour is kept with a probability of 99.95%).
pub fn code_radius(floor: f32) -> u32 {
    let bits = DIM as f64;
    let p = (floor.clamp(-1.0, 1.0) as f64).acos() / std::f64::consts::PI;
    let sd = (bits * p * (1.0 - p)).sqrt();
    (bits * p + 3.3 * sd).ceil().min(bits) as u32
}

#[cfg(test)]
pub(crate) fn code_distance(a: &[u64], b: &[u64]) -> u32 {
    a.iter().zip(b).map(|(x, y)| (x ^ y).count_ones()).sum()
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
    /// Sign codes (`WORDS` per vector), for searches over many vectors.
    codes: Vec<u64>,
    quality: Vec<f32>,
    subject: Vec<i64>,
    confirmed: Vec<bool>,
    pos: HashMap<i64, usize>,
}

impl Space {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Memory held (from capacities: what was allocated, not just what is used).
    pub fn mem_bytes(&self) -> usize {
        use std::mem::size_of;
        self.ids.capacity() * 8
            + self.photos.capacity() * 8
            + self.vecs.capacity()
            + self.norms.capacity() * 4
            + self.codes.capacity() * 8
            + self.quality.capacity() * 4
            + self.subject.capacity() * 8
            + self.confirmed.capacity()
            + self.pos.capacity() * (size_of::<(i64, usize)>() + 1)
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
        self.insert_coded(id, photo, v, sign_code(v), quality, subject, confirmed);
    }

    /// `insert`, given the vector's sign code (worked out ahead, in parallel).
    #[allow(clippy::too_many_arguments)]
    pub fn insert_coded(&mut self, id: i64, photo: i64, v: &[i8], code: [u64; WORDS], quality: f32, subject: i64, confirmed: bool) {
        debug_assert_eq!(v.len(), DIM);
        if let Some(s) = self.slot(id) {
            self.vecs[s * DIM..(s + 1) * DIM].copy_from_slice(v);
            self.codes[s * WORDS..(s + 1) * WORDS].copy_from_slice(&code);
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
        self.codes.extend_from_slice(&code);
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
            let (a, b) = self.codes.split_at_mut(last * WORDS);
            a[s * WORDS..(s + 1) * WORDS].copy_from_slice(&b[..WORDS]);
            self.pos.insert(moved, s);
        }
        self.ids.pop();
        self.photos.pop();
        self.norms.pop();
        self.quality.pop();
        self.subject.pop();
        self.confirmed.pop();
        self.vecs.truncate(last * DIM);
        self.codes.truncate(last * WORDS);
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

    /// `search_many`, through the sign codes in a large space (`search_many_coded`).
    pub fn search_batch(&self, qs: &[&[i8]], floor: f32, k: usize) -> Vec<Vec<Hit>> {
        if self.len() > CODED_FROM { self.search_many_coded(qs, floor, k) } else { self.search_many(qs, floor, k) }
    }

    /// `search_many` through the sign codes: each stored vector's code is compared with
    /// the queries' first, and only vectors whose codes are near enough are compared
    /// exactly (`code_radius`). The same hits but for about one true neighbour in 2,000
    /// at the floor; several times faster than `search_many` on large spaces.
    pub fn search_many_coded(&self, qs: &[&[i8]], floor: f32, k: usize) -> Vec<Vec<Hit>> {
        let nq = qs.len();
        if nq == 0 || self.is_empty() {
            return vec![Vec::new(); nq];
        }
        let qn: Vec<f32> = qs.iter().map(|q| norm(q)).collect();
        let qc: Vec<[u64; WORDS]> = qs.iter().map(|q| sign_code(q)).collect();
        let radius = code_radius(floor);
        let n = self.len();
        const CHUNK: usize = 8192;
        let parts: Vec<Vec<Vec<Hit>>> = (0..n.div_ceil(CHUNK))
            .into_par_iter()
            .map(|c| {
                let range = c * CHUNK..((c + 1) * CHUNK).min(n);
                #[cfg(target_arch = "x86_64")]
                if std::arch::is_x86_feature_detected!("popcnt") {
                    // SAFETY: the CPU has the instruction (checked above).
                    return unsafe { self.coded_chunk_popcnt(range, qs, &qn, &qc, radius, floor) };
                }
                self.coded_chunk(range, qs, &qn, &qc, radius, floor)
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

    /// One chunk of `search_many_coded`: the stored vectors in `range` against every
    /// query, codes first.
    #[inline(always)]
    fn coded_chunk(
        &self,
        range: std::ops::Range<usize>,
        qs: &[&[i8]],
        qn: &[f32],
        qc: &[[u64; WORDS]],
        radius: u32,
        floor: f32,
    ) -> Vec<Vec<Hit>> {
        let mut out: Vec<Vec<Hit>> = vec![Vec::new(); qs.len()];
        for i in range {
            let code: &[u64; WORDS] = self.codes[i * WORDS..(i + 1) * WORDS].try_into().unwrap();
            let vn = self.norms[i];
            for (j, c) in qc.iter().enumerate() {
                let mut d = 0;
                for w in 0..WORDS {
                    d += (code[w] ^ c[w]).count_ones();
                }
                if d > radius {
                    continue;
                }
                let den = qn[j] * vn;
                if den <= 0.0 {
                    continue;
                }
                let dp = dot(qs[j], &self.vecs[i * DIM..(i + 1) * DIM]) as f32;
                if dp >= floor * den {
                    out[j].push(Hit { slot: i, sim: dp / den });
                }
            }
        }
        out
    }

    /// `coded_chunk` with the CPU's population-count instruction (one instruction per
    /// word instead of a dozen).
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "popcnt")]
    unsafe fn coded_chunk_popcnt(
        &self,
        range: std::ops::Range<usize>,
        qs: &[&[i8]],
        qn: &[f32],
        qc: &[[u64; WORDS]],
        radius: u32,
        floor: f32,
    ) -> Vec<Vec<Hit>> {
        self.coded_chunk(range, qs, qn, qc, radius, floor)
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
