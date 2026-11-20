//! Series: photos one camera took in quick succession in one folder (bursts, brackets,
//! a run of attempts at one moment). The culling view (see ROADMAP.md) is meant to group
//! a folder the same way, so this knows nothing about the rest of the duplicates report.

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
}

/// How series are cut.
#[derive(Clone, Copy, Debug)]
pub struct Rules {
    /// Longest pause within a series, seconds.
    pub max_gap: i64,
    /// Consecutive frames further apart than this (perceptual hash bits) are different
    /// scenes, however quickly they followed each other.
    pub scene: u32,
    /// Fewest frames that make a series.
    pub min_len: usize,
}

impl Default for Rules {
    fn default() -> Rules {
        Rules { max_gap: 2, scene: 22, min_len: 2 }
    }
}

fn order(a: &Shot, b: &Shot) -> Ordering {
    a.folder.cmp(&b.folder).then_with(|| a.camera.cmp(&b.camera)).then(a.taken.cmp(&b.taken)).then_with(|| a.name.cmp(&b.name)).then(a.id.cmp(&b.id))
}

/// Sorts `shots` into shooting order and returns the series, as ranges of `shots`.
pub fn series(shots: &mut [Shot], rules: Rules) -> Vec<std::ops::Range<usize>> {
    shots.sort_unstable_by(order);
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=shots.len() {
        let cut = i == shots.len() || {
            let (a, b) = (&shots[i - 1], &shots[i]);
            a.folder != b.folder
                || a.camera != b.camera
                || b.taken - a.taken > rules.max_gap
                || matches!((a.hash, b.hash), (Some(x), Some(y)) if x != FLAT && y != FLAT && distance(x, y) > rules.scene)
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
        Shot { id, folder, camera: camera.into(), taken, name: format!("IMG_{id:04}.CR3"), hash }
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
            // Ten seconds later: alone.
            shot(5, 1, "Canon R5", 111, Some(scene_a)),
            // A second body at the same moment is its own series.
            shot(6, 1, "Sony A7", 100, None),
            shot(7, 1, "Sony A7", 101, None),
            // Quick, but a different scene: two series of two.
            shot(8, 1, "Canon R5", 200, Some(scene_a)),
            shot(9, 1, "Canon R5", 200, Some(scene_a)),
            shot(10, 1, "Canon R5", 201, Some(scene_b)),
            shot(11, 1, "Canon R5", 201, Some(scene_b)),
            // The same moment in another folder (a copy) is not part of the first.
            shot(12, 2, "Canon R5", 100, Some(scene_a)),
        ];
        let runs = series(&mut shots, Rules::default());
        let ids: Vec<Vec<i64>> = runs.iter().map(|r| shots[r.clone()].iter().map(|s| s.id).collect()).collect();
        assert_eq!(ids, vec![vec![1, 2, 3, 4], vec![8, 9], vec![10, 11], vec![6, 7]]);
        // At most one second apart, at least three frames.
        let runs = series(&mut shots, Rules { max_gap: 0, scene: 22, min_len: 3 });
        let ids: Vec<Vec<i64>> = runs.iter().map(|r| shots[r.clone()].iter().map(|s| s.id).collect()).collect();
        assert_eq!(ids, vec![vec![1, 2, 3]]);
        assert!(series(&mut [], Rules::default()).is_empty());
    }
}
