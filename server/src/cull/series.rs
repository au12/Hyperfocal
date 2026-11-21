//! Series as stacks: the frames one camera took of one moment (see `dupes::bursts`), and
//! the frame to show for them.

use crate::dupes::bursts::{self, Rules, Shot};
use std::cmp::Ordering;

#[derive(Debug, Clone, Default)]
pub struct Frame {
    pub id: i64,
    pub folder: i64,
    /// Make and model.
    pub camera: String,
    pub taken: Option<i64>,
    pub name: String,
    pub hash: Option<u64>,
    pub rating: i64,
    pub pick: i64,
    /// Measured sharpness of the subject (0..1).
    pub focus: Option<f64>,
    /// Faces with closed eyes (None: not looked at).
    pub closed: Option<i64>,
    /// The thumbnail's sharpness estimate (Duplicates report).
    pub sharp: Option<f64>,
}

/// The series among `frames` (in any order): indices into `frames`, each series in
/// shooting order. Frames without a capture time belong to none.
pub fn group(frames: &[Frame], rules: Rules) -> Vec<Vec<usize>> {
    let mut shots: Vec<Shot> = frames
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            Some(Shot { id: i as i64, folder: f.folder, camera: f.camera.clone(), taken: f.taken?, name: f.name.clone(), hash: f.hash })
        })
        .collect();
    bursts::series(&mut shots, rules).into_iter().map(|r| shots[r].iter().map(|s| s.id as usize).collect()).collect()
}

/// The two sharpness values tell nothing apart when this close.
const FOCUS_SAME: f64 = 0.05;

/// Is `a` a better frame to keep than `b`? Not rejected; rated higher; picked; fewer
/// closed eyes; sharper subject (measured, then the thumbnail estimate). Equal frames
/// compare equal: the first of the series wins.
pub fn compare(a: &Frame, b: &Frame) -> Ordering {
    let not_rejected = |f: &Frame| (f.pick != -1) as i64;
    not_rejected(a)
        .cmp(&not_rejected(b))
        .then(a.rating.cmp(&b.rating))
        .then(((a.pick == 1) as i64).cmp(&((b.pick == 1) as i64)))
        .then_with(|| match (a.closed, b.closed) {
            (Some(x), Some(y)) => y.cmp(&x),
            _ => Ordering::Equal,
        })
        .then_with(|| match (a.focus, b.focus) {
            (Some(x), Some(y)) if (x - y).abs() > FOCUS_SAME => x.total_cmp(&y),
            _ => Ordering::Equal,
        })
        .then_with(|| match (a.sharp, b.sharp) {
            (Some(x), Some(y)) if (x - y).abs() > 0.1 * x.max(y) => x.total_cmp(&y),
            _ => Ordering::Equal,
        })
}

/// The best frame of a series (an index into `frames`): ties go to the earlier frame.
pub fn best(frames: &[Frame], members: &[usize]) -> usize {
    let mut b = members[0];
    for &m in &members[1..] {
        if compare(&frames[m], &frames[b]).is_gt() {
            b = m;
        }
    }
    b
}

/// Why the best frame was chosen over the second best, in a few words.
pub fn why(best: &Frame, other: &Frame) -> &'static str {
    let reason = decisive(best, other);
    // Nothing tells the two apart: as sharp as the second, and earlier.
    if reason == "First frame" && best.focus.is_some() && other.focus.is_some() {
        return "Sharpest, and the earlier of two";
    }
    reason
}

fn decisive(best: &Frame, other: &Frame) -> &'static str {
    if (best.pick != -1) != (other.pick != -1) {
        return "Not rejected";
    }
    if best.rating != other.rating {
        return "Highest rating";
    }
    if (best.pick == 1) != (other.pick == 1) {
        return "Picked";
    }
    if let (Some(x), Some(y)) = (best.closed, other.closed)
        && x != y
    {
        return "Eyes open";
    }
    if let (Some(x), Some(y)) = (best.focus, other.focus)
        && (x - y).abs() > FOCUS_SAME
    {
        return "Sharpest";
    }
    if let (Some(x), Some(y)) = (best.sharp, other.sharp)
        && (x - y).abs() > 0.1 * x.max(y)
    {
        return "Sharpest (estimate)";
    }
    "First frame"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: i64, taken: i64) -> Frame {
        Frame { id, folder: 1, camera: "Canon EOS R5".into(), taken: Some(taken), name: format!("IMG_{id:04}.CR3"), ..Default::default() }
    }

    #[test]
    fn frames_of_a_moment_group_and_the_best_is_chosen() {
        let mut f = vec![frame(1, 100), frame(2, 100), frame(3, 101), frame(4, 200), frame(5, 201), frame(6, 400)];
        f[0].focus = Some(0.5);
        f[1].focus = Some(0.9);
        f[2].focus = Some(0.92);
        // Frame 3 is sharpest, but someone blinked.
        f[2].closed = Some(1);
        f[1].closed = Some(0);
        let g = group(&f, Rules::default());
        assert_eq!(g, vec![vec![0, 1, 2], vec![3, 4]]);
        assert_eq!(best(&f, &g[0]), 1);
        assert_eq!(why(&f[1], &f[2]), "Eyes open");
        // A rating beats everything measured.
        f[0].rating = 3;
        assert_eq!(best(&f, &g[0]), 0);
        // Equal frames: the first.
        assert_eq!(best(&f, &g[1]), 3);
        assert_eq!(why(&f[3], &f[4]), "First frame");
        f[3].focus = Some(0.8);
        f[4].focus = Some(0.82);
        assert_eq!(why(&f[3], &f[4]), "Sharpest, and the earlier of two");
        // A rejected frame is never the best.
        f[0].pick = -1;
        assert_eq!(best(&f, &g[0]), 1);
    }

    #[test]
    fn frames_without_a_time_are_alone() {
        let mut f = vec![frame(1, 100), frame(2, 100)];
        f[1].taken = None;
        assert!(group(&f, Rules::default()).is_empty());
    }
}
