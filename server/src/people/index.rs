//! Every usable face (and every dog, every cat) embedding in memory, as int8 vectors,
//! searched by brute force: 512 bytes a face, so 600,000 faces take about 300 MB, and
//! one search over them a few milliseconds on a NAS CPU (in parallel over the cores
//! for large spaces). Exact, deterministic and with nothing to rebuild; an approximate
//! index (HNSW) would only pay off at several million faces.

use super::DIM;
use crate::images::simd::dispatch;
use rayon::prelude::*;
use std::collections::HashMap;

/// int8 with a per-vector scale (the largest component maps to 127). Cosine
/// similarities come out within about 0.002 of the float ones.
pub fn quantize(v: &[f32]) -> Vec<i8> {
    let m = v.iter().fold(0f32, |m, x| m.max(x.abs()));
    let s = if m > 0.0 { 127.0 / m } else { 0.0 };
    v.iter().map(|x| (x * s).round().clamp(-127.0, 127.0) as i8).collect()
}

#[inline(always)]
fn dot_imp(a: &[i8], b: &[i8]) -> i32 {
    let mut acc = [0i32; 16];
    for (x, y) in a.chunks_exact(16).zip(b.chunks_exact(16)) {
        for i in 0..16 {
            acc[i] += x[i] as i32 * y[i] as i32;
        }
    }
    acc.iter().sum()
}

dispatch!(pub fn dot = dot_imp(a: &[i8], b: &[i8]) -> i32);

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
