//! Every pair of 64-bit hashes within a Hamming distance, without comparing every pair
//! with every other: multi-index hashing (Norouzi, Punjani and Fleet, 2012).
//!
//! Each hash is cut into four 16-bit pieces, and each piece position gets a table of
//! the hashes by that piece. Two hashes at most `r` bits apart differ in at most `r / 4`
//! bits in one of the four pieces at least (pigeonhole), so only hashes in the same
//! bucket of some table, or in buckets whose pieces are that few bits apart, need their
//! full distance checked. A pair is reported by the first table that can find it, so
//! once.
//!
//! The textbook structure for this is a BK-tree, but at the distances near-duplicate
//! search needs (7 to 10 of 64 bits) a BK-tree visits most of its nodes for every
//! query; this takes a fraction of a second for 300,000 photos.

use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const PIECES: usize = 4;
const VALUES: usize = 1 << 16;

fn piece(h: u64, t: usize) -> usize {
    ((h >> (16 * t)) & 0xffff) as usize
}

/// The hashes ordered by one piece (counting sort): bucket `v` is
/// `items[start[v]..start[v + 1]]`, in increasing index order. Each item carries its
/// hash, so a bucket is checked reading memory in order.
struct Table {
    start: Vec<u32>,
    items: Vec<(u64, u32)>,
}

impl Table {
    fn new(hashes: &[u64], t: usize) -> Table {
        let mut start = vec![0u32; VALUES + 1];
        for &h in hashes {
            start[piece(h, t) + 1] += 1;
        }
        for v in 1..=VALUES {
            start[v] += start[v - 1];
        }
        let mut next = start.clone();
        let mut items = vec![(0u64, 0u32); hashes.len()];
        for (i, &h) in hashes.iter().enumerate() {
            let p = piece(h, t);
            items[next[p] as usize] = (h, i as u32);
            next[p] += 1;
        }
        Table { start, items }
    }

    fn bucket(&self, v: usize) -> &[(u64, u32)] {
        &self.items[self.start[v] as usize..self.start[v + 1] as usize]
    }
}

/// The non-zero 16-bit masks with at most `bits` bits set.
fn masks(bits: u32) -> Vec<usize> {
    (1..VALUES).filter(|m| (*m as u32).count_ones() <= bits).collect()
}

pub struct Pairs {
    /// (i, j, distance), i < j, indexes into the hashes given.
    pub pairs: Vec<(u32, u32, u8)>,
    /// More than `cap` pairs: the rest were not looked for.
    pub truncated: bool,
}

/// Every pair of `hashes` at most `radius` bits apart (at most `cap` of them).
///
/// Rather than probing the tables once for every hash, each table is joined with
/// itself bucket by bucket: a bucket against itself and against each bucket whose
/// piece is within `s` bits, every pair of hashes once, reading both buckets in order.
pub fn pairs(hashes: &[u64], radius: u32, cap: usize) -> Pairs {
    let radius = radius.min(32);
    let s = radius / PIECES as u32;
    if hashes.len() < 2 {
        return Pairs { pairs: Vec::new(), truncated: false };
    }
    let tables: Vec<Table> = (0..PIECES).into_par_iter().map(|t| Table::new(hashes, t)).collect();
    let near = masks(s);
    let found = AtomicUsize::new(0);
    let full = AtomicBool::new(false);
    // Whether a pair is reported by table `t`: the first table in which their pieces are
    // within `s` bits.
    let first = |t: usize, a: u64, b: u64| (0..t).all(|u| ((piece(a, u) ^ piece(b, u)) as u32).count_ones() > s);
    let pairs = (0..PIECES * VALUES)
        .into_par_iter()
        .with_min_len(1024)
        .fold(Vec::new, |mut out: Vec<(u32, u32, u8)>, k| {
            if full.load(Ordering::Relaxed) {
                return out;
            }
            let (t, v) = (k / VALUES, k % VALUES);
            let table = &tables[t];
            let here = table.bucket(v);
            if here.is_empty() {
                return out;
            }
            let check = |out: &mut Vec<(u32, u32, u8)>, (a, i): (u64, u32), (b, j): (u64, u32)| -> bool {
                let d = (a ^ b).count_ones();
                if d <= radius && first(t, a, b) {
                    out.push((i.min(j), i.max(j), d as u8));
                    if found.fetch_add(1, Ordering::Relaxed) + 1 >= cap {
                        full.store(true, Ordering::Relaxed);
                        return false;
                    }
                }
                true
            };
            for x in 0..here.len() {
                for y in x + 1..here.len() {
                    if !check(&mut out, here[x], here[y]) {
                        return out;
                    }
                }
            }
            for &m in &near {
                let w = v ^ m;
                // Each pair of buckets once.
                if w < v {
                    continue;
                }
                for &a in here {
                    for &b in table.bucket(w) {
                        if !check(&mut out, a, b) {
                            return out;
                        }
                    }
                }
            }
            out
        })
        .reduce(Vec::new, |mut a, mut b| {
            if a.len() < b.len() {
                std::mem::swap(&mut a, &mut b);
            }
            a.append(&mut b);
            a
        });
    Pairs { pairs, truncated: full.load(Ordering::Relaxed) }
}

/// The four tables, for looking up hashes that are not in the set (a photo's turned and
/// mirrored versions against everyone's upright hash).
pub struct Index {
    tables: Vec<Table>,
}

impl Index {
    pub fn new(hashes: &[u64]) -> Index {
        Index { tables: (0..PIECES).into_par_iter().map(|t| Table::new(hashes, t)).collect() }
    }

    /// The hashes of the set at most `radius` bits from `h`: (index, distance), each once.
    pub fn query(&self, h: u64, radius: u32, masks: &[usize], out: &mut Vec<(u32, u8)>) {
        let radius = radius.min(32);
        let s = radius / PIECES as u32;
        let start = out.len();
        for (t, table) in self.tables.iter().enumerate() {
            let v = piece(h, t);
            for m in std::iter::once(0usize).chain(masks.iter().copied()) {
                if (m as u32).count_ones() > s {
                    continue;
                }
                for &(b, j) in table.bucket(v ^ m) {
                    let d = (h ^ b).count_ones();
                    if d <= radius {
                        out.push((j, d as u8));
                    }
                }
            }
        }
        // A hash found through several tables is listed once.
        let tail = &mut out[start..];
        tail.sort_unstable();
        let mut w = start;
        for r in start..out.len() {
            if w == start || out[w - 1].0 != out[r].0 {
                out[w] = out[r];
                w += 1;
            }
        }
        out.truncate(w);
    }

    /// The 16-bit masks a query at `radius` needs.
    pub fn masks(radius: u32) -> Vec<usize> {
        masks(radius.min(32) / PIECES as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute(hashes: &[u64], r: u32) -> Vec<(u32, u32, u8)> {
        let mut v = Vec::new();
        for i in 0..hashes.len() {
            for j in i + 1..hashes.len() {
                let d = (hashes[i] ^ hashes[j]).count_ones();
                if d <= r {
                    v.push((i as u32, j as u32, d as u8));
                }
            }
        }
        v
    }

    /// Clusters of nearby hashes among random ones, like a library: copies, edits and
    /// bursts around a few originals.
    fn library(n: usize, seed: u64) -> Vec<u64> {
        let mut x = seed | 1;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut v = Vec::with_capacity(n);
        while v.len() < n {
            let base = rnd();
            v.push(base);
            for _ in 0..(rnd() % 4) {
                let mut h = base;
                for _ in 0..(rnd() % 12) {
                    h ^= 1 << (rnd() % 64);
                }
                v.push(h);
            }
        }
        v.truncate(n);
        v
    }

    #[test]
    fn finds_exactly_the_pairs_brute_force_finds() {
        for (n, seed) in [(2, 1), (50, 2), (3000, 3), (6000, 4)] {
            let hashes = library(n, seed);
            for r in [0, 3, 4, 7, 10, 12] {
                let mut got = pairs(&hashes, r, usize::MAX).pairs;
                got.sort_unstable();
                assert_eq!(got, brute(&hashes, r), "n {n} r {r}");
            }
        }
    }

    #[test]
    fn identical_hashes_and_the_cap() {
        let hashes = vec![7u64; 40];
        let p = pairs(&hashes, 4, usize::MAX);
        assert_eq!(p.pairs.len(), 40 * 39 / 2);
        assert!(p.pairs.iter().all(|&(i, j, d)| i < j && d == 0));
        let p = pairs(&hashes, 4, 100);
        assert!(p.truncated);
        assert!(p.pairs.len() >= 100 && p.pairs.len() < 100 + 64, "{}", p.pairs.len());
        assert!(pairs(&[], 7, 10).pairs.is_empty());
        assert!(pairs(&[1], 7, 10).pairs.is_empty());
    }
}
