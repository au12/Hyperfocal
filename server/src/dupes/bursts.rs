//! Series: the frames one camera took of one moment in one folder (bursts, brackets, a
//! run of attempts at a pose, a subject worked for a minute). Used by culling's stacks
//! (`cull::series`) and the Duplicates report's series.
//!
//! Frames are walked in shooting order. A frame joins the series before it when
//! - it followed within `burst_gap` seconds, unless it looks clearly different (a quick
//!   turn to another subject); or
//! - it followed within `max_gap` seconds and looks like the series' last frames: the
//!   longer the pause, the more alike it must look (`strictness`).
//!
//! "Alike" is the SigLIP 2 image embeddings' cosine when search by content has looked at
//! both frames (robust to a subject that moved, a step to the side, a zoom), else their
//! perceptual hashes. Without either, only the burst gap joins frames.
//!
//! Calibrated on a hand-labelled real sample (669 Canon R6 RAWs: an event, a wildlife walk,
//! pets, flowers): with the defaults 98% of the frames that belong together are joined and
//! no two scenes are (the old rule, a 2-second gap and a 22-bit hash cut, joined 67%).
//! With hashes alone, 90%.

use super::phash::{FLAT, distance};
use std::cmp::Ordering;

pub struct Shot {
    pub id: i64,
    pub folder: i64,
    /// Make and model.
    pub camera: String,
    /// Capture time, seconds (camera clock).
    pub taken: i64,
    /// File name: orders the frames of one second (camera counters).
    pub name: String,
    /// Perceptual hash, when there is one.
    pub hash: Option<u64>,
    /// The photo's image embedding in the search index (`visual::Index::slot`).
    pub emb: Option<u32>,
}

/// How series are cut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rules {
    /// Frames this close (seconds) stay together unless the scene clearly changes.
    pub burst_gap: f64,
    /// The longest pause within a series, seconds.
    pub max_gap: f64,
    /// How alike frames after a pause must look: 0 loose .. 1 strict.
    pub strictness: f32,
    /// Within a burst, frames less alike than this (0..1) are another scene.
    pub scene: f32,
    /// Fewest frames that make a series.
    pub min_len: usize,
    /// One series may hold frames of several cameras.
    pub cameras: bool,
}

impl Default for Rules {
    fn default() -> Rules {
        Rules { burst_gap: 2.0, max_gap: 60.0, strictness: 0.5, scene: 0.1, min_len: 2, cameras: false }
    }
}

/// Embedding cosines from "unrelated" (0) to "the same scene" (1).
const COS_LOW: f32 = 0.80;
const COS_HIGH: f32 = 0.97;
/// Perceptual-hash distances (bits) from "the same scene" (1) to "unrelated" (0): two
/// random photos are about 32 bits apart.
const BITS_SAME: f32 = 6.0;
const BITS_UNRELATED: f32 = 32.0;
/// A frame is compared with this many of the series' last frames (so one odd frame, a
/// passer-by or a misfire, does not end the series).
const LOOK_BACK: usize = 3;

/// The image embeddings, when search by content has made them.
pub trait Looks {
    /// Cosine of two embeddings (slots).
    fn cos(&self, a: u32, b: u32) -> f32;
}

impl Looks for crate::visual::Index {
    fn cos(&self, a: u32, b: u32) -> f32 {
        self.cos_slots(a as usize, b as usize)
    }
}

/// How alike two frames look, 0..1 (None: nothing to tell by).
pub fn alike(a: &Shot, b: &Shot, looks: Option<&dyn Looks>) -> Option<f32> {
    let mut s = None;
    if let (Some(l), Some(x), Some(y)) = (looks, a.emb, b.emb) {
        let c = l.cos(x, y);
        s = Some(((c - COS_LOW) / (COS_HIGH - COS_LOW)).clamp(0.0, 1.0));
    }
    if let (Some(x), Some(y)) = (a.hash, b.hash)
        && x != FLAT
        && y != FLAT
    {
        let h = ((BITS_UNRELATED - distance(x, y) as f32) / (BITS_UNRELATED - BITS_SAME)).clamp(0.0, 1.0);
        s = Some(s.map_or(h, |e: f32| e.max(h)));
    }
    s
}

impl Rules {
    /// How alike a frame must look to join after a pause of `gap` seconds (None: never).
    fn needed(&self, gap: f64) -> Option<f32> {
        if gap <= self.burst_gap {
            return Some(self.scene);
        }
        if gap > self.max_gap {
            return None;
        }
        let span = self.max_gap - self.burst_gap;
        let x = if span > 0.0 { ((gap - self.burst_gap) / span).sqrt() as f32 } else { 1.0 };
        let s = self.strictness.clamp(0.0, 1.0);
        let (lo, hi) = (0.3 + 0.6 * s, (0.6 + 0.6 * s).min(1.0));
        Some(lo + (hi - lo) * x)
    }
}

fn order(cameras: bool) -> impl Fn(&Shot, &Shot) -> Ordering {
    move |a, b| {
        let o = a.folder.cmp(&b.folder);
        let o = if cameras { o } else { o.then_with(|| a.camera.cmp(&b.camera)) };
        o.then(a.taken.cmp(&b.taken)).then_with(|| a.name.cmp(&b.name)).then(a.id.cmp(&b.id))
    }
}

/// Sorts `shots` into shooting order and returns the series, as ranges of `shots`.
pub fn series(shots: &mut [Shot], rules: Rules, looks: Option<&dyn Looks>) -> Vec<std::ops::Range<usize>> {
    shots.sort_unstable_by(order(rules.cameras));
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=shots.len() {
        let cut = i == shots.len() || {
            let (a, b) = (&shots[i - 1], &shots[i]);
            if a.folder != b.folder || (!rules.cameras && a.camera != b.camera) {
                true
            } else {
                let gap = (b.taken - a.taken) as f64;
                match rules.needed(gap) {
                    None => true,
                    Some(need) => {
                        let best = shots[start.max(i.saturating_sub(LOOK_BACK))..i].iter().filter_map(|p| alike(p, b, looks)).fold(None, |m: Option<f32>, s| Some(m.map_or(s, |m| m.max(s))));
                        match best {
                            Some(s) => s < need,
                            // Nothing to compare: only the burst gap holds frames together.
                            None => gap > rules.burst_gap,
                        }
                    }
                }
            }
        };
        if cut {
            if i - start >= rules.min_len.max(2) {
                out.push(start..i);
            }
            start = i;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(id: i64, folder: i64, camera: &str, taken: i64, hash: Option<u64>) -> Shot {
        Shot { id, folder, camera: camera.into(), taken, name: format!("IMG_{id:04}.CR3"), hash, emb: None }
    }

    fn ids(shots: &[Shot], runs: &[std::ops::Range<usize>]) -> Vec<Vec<i64>> {
        runs.iter().map(|r| shots[r.clone()].iter().map(|s| s.id).collect()).collect()
    }

    /// Unit vectors in 3 dimensions, as "embeddings" (cosines by hand).
    struct Vecs(Vec<[f32; 3]>);
    impl Looks for Vecs {
        fn cos(&self, a: u32, b: u32) -> f32 {
            let (x, y) = (self.0[a as usize], self.0[b as usize]);
            x[0] * y[0] + x[1] * y[1] + x[2] * y[2]
        }
    }

    #[test]
    fn cuts_at_pauses_cameras_folders_and_scene_changes() {
        let scene_a = 0x0f0f_0f0f_0f0f_0f0fu64;
        let scene_b = !scene_a;
        let mut shots = vec![
            // A burst of four at 10 fps (one-second timestamps), given out of order.
            shot(3, 1, "Canon R5", 100, Some(scene_a ^ 0b11)),
            shot(1, 1, "Canon R5", 100, Some(scene_a)),
            shot(4, 1, "Canon R5", 101, Some(scene_a ^ 0b111)),
            shot(2, 1, "Canon R5", 100, Some(scene_a ^ 0b1)),
            // Ten seconds later, the same scene again: still the same series.
            shot(5, 1, "Canon R5", 111, Some(scene_a)),
            // A second body at the same moment is its own series.
            shot(6, 1, "Sony A7", 100, None),
            shot(7, 1, "Sony A7", 101, None),
            // Without hashes, a pause ends a series.
            shot(13, 1, "Sony A7", 110, None),
            // Quick, but a different scene: two series of two.
            shot(8, 1, "Canon R5", 300, Some(scene_a)),
            shot(9, 1, "Canon R5", 300, Some(scene_a)),
            shot(10, 1, "Canon R5", 301, Some(scene_b)),
            shot(11, 1, "Canon R5", 301, Some(scene_b)),
            // The same moment in another folder (a copy) is not part of the first.
            shot(12, 2, "Canon R5", 100, Some(scene_a)),
        ];
        let runs = series(&mut shots, Rules::default(), None);
        assert_eq!(ids(&shots, &runs), vec![vec![1, 2, 3, 4, 5], vec![8, 9], vec![10, 11], vec![6, 7]]);
        // At most a second apart, three frames or more, no pauses.
        let runs = series(&mut shots, Rules { burst_gap: 0.0, max_gap: 0.0, min_len: 3, ..Rules::default() }, None);
        assert_eq!(ids(&shots, &runs), vec![vec![1, 2, 3]]);
        // Cameras mixed: the two bodies' frames of one moment together.
        let runs = series(&mut shots, Rules { cameras: true, ..Rules::default() }, None);
        assert_eq!(ids(&shots, &runs)[0], vec![1, 2, 3, 6, 4, 7]);
        assert!(series(&mut [], Rules::default(), None).is_empty());
    }

    #[test]
    fn pauses_need_more_alike_frames_the_longer_they_are() {
        let r = Rules::default();
        assert_eq!(r.needed(1.0), Some(r.scene));
        let (a, b, c) = (r.needed(3.0).unwrap(), r.needed(30.0).unwrap(), r.needed(60.0).unwrap());
        assert!(a < b && b < c && c <= 1.0, "{a} {b} {c}");
        assert_eq!(r.needed(61.0), None);
        let strict = Rules { strictness: 1.0, ..r };
        let loose = Rules { strictness: 0.0, ..r };
        assert!(strict.needed(30.0).unwrap() > b && loose.needed(30.0).unwrap() < b);
    }

    #[test]
    fn embeddings_join_a_subject_that_moved_and_part_other_scenes() {
        // Frame 0..3: one subject, the hashes far apart (it moved); 4: another scene 5 s
        // later; 5: the first subject again a minute and a half later (past the longest
        // pause).
        let v = Vecs(vec![[1.0, 0.0, 0.0], [0.99, 0.141, 0.0], [0.985, 0.0, 0.172], [0.98, 0.199, 0.0], [0.6, 0.8, 0.0], [1.0, 0.0, 0.0]]);
        let far = |k: u64| Some(0x5555_5555_5555_5555u64.rotate_left(k as u32 * 7) ^ (k * 0x1234_5678));
        let mut shots: Vec<Shot> = [(0, 100), (1, 104), (2, 110), (3, 125), (4, 130), (5, 220)]
            .iter()
            .map(|&(k, t)| Shot { emb: Some(k as u32), ..shot(k as i64 + 1, 1, "R6", t, far(k)) })
            .collect();
        let runs = series(&mut shots, Rules::default(), Some(&v));
        assert_eq!(ids(&shots, &runs), vec![vec![1, 2, 3, 4]]);
        // Without the embeddings the hashes tell nothing alike: no series past the bursts.
        assert!(series(&mut shots, Rules::default(), None).is_empty());
        // An odd frame (someone walked through) does not end a series: the next frame
        // is compared with the last few, not only the odd one.
        let v = Vecs(vec![[1.0, 0.0, 0.0], [0.99, 0.141, 0.0], [0.82, 0.572, 0.0], [0.99, -0.141, 0.0]]);
        let mut shots: Vec<Shot> = [(0, 100), (1, 101), (2, 102), (3, 111)].iter().map(|&(k, t)| Shot { emb: Some(k as u32), ..shot(k as i64 + 1, 1, "R6", t, None) }).collect();
        let runs = series(&mut shots, Rules::default(), Some(&v));
        assert_eq!(ids(&shots, &runs), vec![vec![1, 2, 3, 4]]);
        // A stricter scene cut parts the odd frame, and the frame after it starts anew.
        let runs = series(&mut shots, Rules { scene: 0.6, ..Rules::default() }, Some(&v));
        assert_eq!(ids(&shots, &runs), vec![vec![1, 2]]);
    }
}
