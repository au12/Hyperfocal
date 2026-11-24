//! Series as stacks: the frames one camera took of one moment (see `dupes::bursts`), and
//! the frame to show for them.

use crate::dupes::bursts::{self, Looks, Rules, Shot};
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
    /// The photo's image embedding (`visual::Index::slot`).
    pub emb: Option<u32>,
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
pub fn group(frames: &[Frame], rules: Rules, looks: Option<&dyn Looks>) -> Vec<Vec<usize>> {
    let mut shots: Vec<Shot> = frames
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            Some(Shot { id: i as i64, folder: f.folder, camera: f.camera.clone(), taken: f.taken?, name: f.name.clone(), hash: f.hash, emb: f.emb })
        })
        .collect();
    bursts::series(&mut shots, rules, looks).into_iter().map(|r| shots[r].iter().map(|s| s.id as usize).collect()).collect()
}

/// How the best frame is chosen (Settings → Advanced).
#[derive(Debug, Clone, Copy)]
pub struct Choice {
    /// Open eyes count before sharpness.
    pub eyes_first: bool,
    /// Sharpness values this close tell nothing apart.
    pub tie: f64,
}

impl Choice {
    pub fn current() -> Choice {
        let t = crate::tuning::get();
        Choice { eyes_first: t.best_eyes_first, tie: t.best_tie }
    }
}

impl Default for Choice {
    fn default() -> Choice {
        Choice { eyes_first: true, tie: 0.05 }
    }
}

fn by_eyes(a: &Frame, b: &Frame) -> Ordering {
    match (a.closed, b.closed) {
        (Some(x), Some(y)) => y.cmp(&x),
        _ => Ordering::Equal,
    }
}

fn by_focus(a: &Frame, b: &Frame, tie: f64) -> Ordering {
    match (a.focus, b.focus) {
        (Some(x), Some(y)) if (x - y).abs() > tie => x.total_cmp(&y),
        _ => Ordering::Equal,
    }
}

/// Is `a` a better frame to keep than `b`? Not rejected; rated higher; picked; fewer
/// closed eyes and a sharper subject (in the order chosen); then the thumbnail's
/// sharpness estimate. Equal frames compare equal: the first of the series wins.
pub fn compare_with(a: &Frame, b: &Frame, c: Choice) -> Ordering {
    let not_rejected = |f: &Frame| (f.pick != -1) as i64;
    let measured = if c.eyes_first { by_eyes(a, b).then_with(|| by_focus(a, b, c.tie)) } else { by_focus(a, b, c.tie).then_with(|| by_eyes(a, b)) };
    not_rejected(a)
        .cmp(&not_rejected(b))
        .then(a.rating.cmp(&b.rating))
        .then(((a.pick == 1) as i64).cmp(&((b.pick == 1) as i64)))
        .then(measured)
        .then_with(|| match (a.sharp, b.sharp) {
            (Some(x), Some(y)) if (x - y).abs() > 0.1 * x.max(y) => x.total_cmp(&y),
            _ => Ordering::Equal,
        })
}

pub fn compare(a: &Frame, b: &Frame) -> Ordering {
    compare_with(a, b, Choice::current())
}

/// The best frame of a series (an index into `frames`): ties go to the earlier frame.
pub fn best_with(frames: &[Frame], members: &[usize], c: Choice) -> usize {
    let mut b = members[0];
    for &m in &members[1..] {
        if compare_with(&frames[m], &frames[b], c).is_gt() {
            b = m;
        }
    }
    b
}

pub fn best(frames: &[Frame], members: &[usize]) -> usize {
    best_with(frames, members, Choice::current())
}

/// Why the best frame was chosen over the second best, in a few words. `first`: the best
/// is the series' first frame.
pub fn why(best: &Frame, other: &Frame, first: bool) -> &'static str {
    let c = Choice::current();
    match decisive(best, other, c) {
        // Nothing tells the two apart.
        "" if best.focus.is_some() && other.focus.is_some() => "As sharp as the next best, and earlier",
        "" if first => "First frame",
        "" => "Earliest of equally good frames",
        r => r,
    }
}

fn decisive(best: &Frame, other: &Frame, c: Choice) -> &'static str {
    if (best.pick != -1) != (other.pick != -1) {
        return "Not rejected";
    }
    if best.rating != other.rating {
        return "Highest rating";
    }
    if (best.pick == 1) != (other.pick == 1) {
        return "Picked";
    }
    let eyes = by_eyes(best, other).is_ne();
    let focus = by_focus(best, other, c.tie).is_ne();
    if c.eyes_first {
        if eyes {
            return "Eyes open";
        }
        if focus {
            return "Sharpest";
        }
    } else {
        if focus {
            return "Sharpest";
        }
        if eyes {
            return "Eyes open";
        }
    }
    if let (Some(x), Some(y)) = (best.sharp, other.sharp)
        && (x - y).abs() > 0.1 * x.max(y)
    {
        return "Sharpest (estimate)";
    }
    ""
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
        let c = Choice::default();
        let g = group(&f, Rules::default(), None);
        assert_eq!(g, vec![vec![0, 1, 2], vec![3, 4]]);
        assert_eq!(best_with(&f, &g[0], c), 1);
        assert_eq!(decisive(&f[1], &f[2], c), "Eyes open");
        // Sharpness first: the blink loses to a frame 0.02 sharper only past the tie.
        assert_eq!(best_with(&f, &g[0], Choice { eyes_first: false, tie: 0.05 }), 1);
        assert_eq!(best_with(&f, &g[0], Choice { eyes_first: false, tie: 0.01 }), 2);
        // A rating beats everything measured.
        f[0].rating = 3;
        assert_eq!(best_with(&f, &g[0], c), 0);
        // Equal frames: the first.
        assert_eq!(best_with(&f, &g[1], c), 3);
        assert_eq!(decisive(&f[3], &f[4], c), "");
        f[3].focus = Some(0.8);
        f[4].focus = Some(0.82);
        assert_eq!(decisive(&f[3], &f[4], c), "");
        // A rejected frame is never the best.
        f[0].pick = -1;
        assert_eq!(best_with(&f, &g[0], c), 1);
    }

    #[test]
    fn frames_without_a_time_are_alone() {
        let mut f = vec![frame(1, 100), frame(2, 100)];
        f[1].taken = None;
        assert!(group(&f, Rules::default(), None).is_empty());
    }
}
