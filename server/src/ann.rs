//! Approximate nearest neighbours for int8 embeddings: an inverted file (IVF, Jégou,
//! Douze and Schmid 2011) over spherical k-means cells.
//!
//! The vectors are split into about √n cells around k-means centroids. A vector's close
//! neighbours (cosine 0.6 and up) almost always lie in its own cell or in one of the few
//! cells whose centroids are nearest to it, so a search looks only there: `nprobe` cells
//! of about √n vectors each instead of all n. For 300,000 vectors that is a few
//! thousand comparisons per vector instead of 300,000.
//!
//! Centroids are kept as int8 like the vectors, so every comparison uses the same AVX2
//! dot product (`people::index::dot`). Training is deterministic for a given input.

use crate::people::index::{dot, norm, quantize};
use rayon::prelude::*;

pub struct Ivf {
    dim: usize,
    /// k centroids (int8, quantized unit vectors) and their norms.
    centroids: Vec<i8>,
    cnorms: Vec<f32>,
    /// The vectors (indexes into the input) of each cell.
    pub lists: Vec<Vec<u32>>,
    /// Each vector's cell.
    pub cell: Vec<u32>,
}

/// Cells for n vectors: about √n, at least 1 and at most 4096.
pub fn cells_for(n: usize) -> usize {
    ((n as f64).sqrt().round() as usize).clamp(1, 4096)
}

fn unit(v: &[i8], n: f32) -> Vec<f32> {
    let n = if n > 0.0 { n } else { 1.0 };
    v.iter().map(|&x| x as f32 / n).collect()
}

impl Ivf {
    /// Builds the index over `n` vectors of `dim` int8 values (`norms[i]` = norm of
    /// vector i), with `k` cells (0: `cells_for(n)`).
    pub fn build(vectors: &[i8], norms: &[f32], dim: usize, k: usize) -> Ivf {
        let n = norms.len();
        debug_assert_eq!(vectors.len(), n * dim);
        let k = if k == 0 { cells_for(n) } else { k }.min(n.max(1));
        let vec_at = |i: usize| &vectors[i * dim..(i + 1) * dim];
        if n == 0 {
            return Ivf { dim, centroids: Vec::new(), cnorms: Vec::new(), lists: Vec::new(), cell: Vec::new() };
        }
        // Training sample: every s-th vector, about 40 per cell (enough for stable cells).
        let step = (n / (k * 40).max(1)).max(1);
        let sample: Vec<usize> = (0..n).step_by(step).collect();
        // Seeds spread over the sample.
        let seed_step = (sample.len() / k).max(1);
        let mut cents: Vec<Vec<f32>> = (0..k).map(|c| unit(vec_at(sample[(c * seed_step).min(sample.len() - 1)]), norms[sample[(c * seed_step).min(sample.len() - 1)]])).collect();
        let mut ivf = Ivf { dim, centroids: Vec::new(), cnorms: Vec::new(), lists: Vec::new(), cell: Vec::new() };
        for _round in 0..8 {
            ivf.set_centroids(&cents);
            let assign: Vec<u32> = sample.par_iter().map(|&i| ivf.nearest(vec_at(i), norms[i], 1)[0]).collect();
            let mut sums = vec![vec![0f32; dim]; k];
            let mut counts = vec![0usize; k];
            for (&i, &c) in sample.iter().zip(&assign) {
                let u = unit(vec_at(i), norms[i]);
                for (s, x) in sums[c as usize].iter_mut().zip(&u) {
                    *s += x;
                }
                counts[c as usize] += 1;
            }
            let mut moved = 0usize;
            for c in 0..k {
                if counts[c] == 0 {
                    // An empty cell takes a point of the largest one.
                    let big = (0..k).max_by_key(|&j| counts[j]).unwrap_or(0);
                    if let Some((&i, _)) = sample.iter().zip(&assign).filter(|(_, a)| **a as usize == big).nth(counts[big] / 2) {
                        cents[c] = unit(vec_at(i), norms[i]);
                        moved += 1;
                    }
                    continue;
                }
                let len = sums[c].iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
                cents[c] = sums[c].iter().map(|x| x / len).collect();
            }
            let _ = moved;
        }
        ivf.set_centroids(&cents);
        ivf.cell = (0..n).into_par_iter().map(|i| ivf.nearest(vec_at(i), norms[i], 1)[0]).collect();
        ivf.lists = vec![Vec::new(); k];
        for (i, &c) in ivf.cell.iter().enumerate() {
            ivf.lists[c as usize].push(i as u32);
        }
        ivf
    }

    fn set_centroids(&mut self, cents: &[Vec<f32>]) {
        self.centroids = cents.iter().flat_map(|c| quantize(c)).collect();
        self.cnorms = (0..cents.len()).map(|c| norm(&self.centroids[c * self.dim..(c + 1) * self.dim]).max(1e-6)).collect();
    }

    pub fn cells(&self) -> usize {
        self.cnorms.len()
    }

    /// The `m` cells whose centroids are closest to a vector, closest first.
    pub fn nearest(&self, v: &[i8], vnorm: f32, m: usize) -> Vec<u32> {
        let k = self.cnorms.len();
        let vn = vnorm.max(1e-6);
        let mut best: Vec<(f32, u32)> = Vec::with_capacity(m + 1);
        for c in 0..k {
            let s = dot(v, &self.centroids[c * self.dim..(c + 1) * self.dim]) as f32 / (vn * self.cnorms[c]);
            if best.len() < m || s > best[best.len() - 1].0 {
                let at = best.partition_point(|b| b.0 >= s);
                best.insert(at, (s, c as u32));
                best.truncate(m);
            }
        }
        best.into_iter().map(|b| b.1).collect()
    }

    /// Candidates for a vector's neighbours: the vectors of its `nprobe` nearest cells.
    pub fn candidates<'a>(&'a self, v: &[i8], vnorm: f32, nprobe: usize) -> impl Iterator<Item = u32> + 'a {
        self.nearest(v, vnorm, nprobe.max(1)).into_iter().flat_map(move |c| self.lists[c as usize].iter().copied())
    }

    /// Every pair of vectors with a cosine of at least `floor` (approximately: pairs in
    /// cells too far apart are missed), as (i, j, cosine) with i < j, each once.
    pub fn pairs(&self, vectors: &[i8], norms: &[f32], floor: f32, nprobe: usize, cap: usize) -> (Vec<(u32, u32, f32)>, bool) {
        let dim = self.dim;
        let n = norms.len();
        let found = std::sync::atomic::AtomicUsize::new(0);
        let mut out: Vec<(u32, u32, f32)> = (0..n)
            .into_par_iter()
            .with_min_len(64)
            .flat_map_iter(|i| {
                let mut local = Vec::new();
                if found.load(std::sync::atomic::Ordering::Relaxed) >= cap {
                    return local;
                }
                let v = &vectors[i * dim..(i + 1) * dim];
                let vn = norms[i].max(1e-6);
                for j in self.candidates(v, vn, nprobe) {
                    let j = j as usize;
                    if j == i {
                        continue;
                    }
                    let s = dot(v, &vectors[j * dim..(j + 1) * dim]) as f32 / (vn * norms[j].max(1e-6));
                    if s >= floor {
                        local.push((i.min(j) as u32, i.max(j) as u32, s));
                    }
                }
                found.fetch_add(local.len(), std::sync::atomic::Ordering::Relaxed);
                local
            })
            .collect();
        out.sort_unstable_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        out.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        let truncated = found.load(std::sync::atomic::Ordering::Relaxed) >= cap;
        (out, truncated)
    }
}

/// Every pair with a cosine of at least `floor`, exactly (all pairs): for small sets.
pub fn pairs_exact(vectors: &[i8], norms: &[f32], dim: usize, floor: f32) -> Vec<(u32, u32, f32)> {
    let n = norms.len();
    (0..n)
        .into_par_iter()
        .with_min_len(32)
        .flat_map_iter(|i| {
            let v = &vectors[i * dim..(i + 1) * dim];
            let vn = norms[i].max(1e-6);
            let mut local = Vec::new();
            for j in i + 1..n {
                let s = dot(v, &vectors[j * dim..(j + 1) * dim]) as f32 / (vn * norms[j].max(1e-6));
                if s >= floor {
                    local.push((i as u32, j as u32, s));
                }
            }
            local
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clustered test data: `groups` random directions, each with `per` noisy copies.
    fn data(groups: usize, per: usize, dim: usize, noise: f32) -> (Vec<i8>, Vec<f32>) {
        let mut x: u64 = 0x2545F4914F6CDD1D;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % 20001) as f32 / 10000.0 - 1.0
        };
        let mut vecs = Vec::new();
        let mut norms = Vec::new();
        for _ in 0..groups {
            let c: Vec<f32> = (0..dim).map(|_| rnd()).collect();
            for _ in 0..per {
                let v: Vec<f32> = c.iter().map(|a| a + noise * rnd()).collect();
                let q = quantize(&v);
                norms.push(norm(&q));
                vecs.extend(q);
            }
        }
        (vecs, norms)
    }

    #[test]
    fn finds_the_same_close_pairs_as_brute_force() {
        let dim = 64;
        let (vecs, norms) = data(300, 4, dim, 0.3);
        let exact = pairs_exact(&vecs, &norms, dim, 0.8);
        assert!(exact.len() > 1000, "{}", exact.len());
        let ivf = Ivf::build(&vecs, &norms, dim, 0);
        assert_eq!(ivf.cells(), cells_for(1200));
        assert_eq!(ivf.lists.iter().map(Vec::len).sum::<usize>(), 1200);
        let (approx, cut) = ivf.pairs(&vecs, &norms, 0.8, 3, usize::MAX);
        assert!(!cut);
        let e: std::collections::HashSet<(u32, u32)> = exact.iter().map(|p| (p.0, p.1)).collect();
        let a: std::collections::HashSet<(u32, u32)> = approx.iter().map(|p| (p.0, p.1)).collect();
        assert!(a.is_subset(&e), "no false pairs");
        let recall = a.len() as f64 / e.len() as f64;
        assert!(recall > 0.98, "recall {recall}");
    }

    #[test]
    fn small_and_empty_inputs() {
        let ivf = Ivf::build(&[], &[], 8, 0);
        assert_eq!(ivf.cells(), 0);
        let (vecs, norms) = data(1, 3, 8, 0.05);
        let ivf = Ivf::build(&vecs, &norms, 8, 0);
        let (p, _) = ivf.pairs(&vecs, &norms, 0.9, 2, usize::MAX);
        assert_eq!(p.len(), 3);
    }
}
