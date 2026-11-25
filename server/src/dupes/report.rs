//! The Duplicates report: groups of exact copies, near-duplicates or series, each with a
//! suggested keeper, for the library, a folder (and its sub-folders), or a folder against
//! the rest of the library.
//!
//! Photos are counted as the grid shows them: a RAW with its JPEG twin is one photo, and
//! a copy of either file makes the pair a duplicate. Photos in reject folders are left out.

use super::{bursts, mih, phash};
use crate::model::{Kind, PhotoRec};
use anyhow::Result;
use rusqlite::{Connection, ToSql, params_from_iter};
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Exact,
    Near,
    Series,
    /// Look-alikes: photos whose image embeddings (search by content) are very close:
    /// crops, edits, other renderings and mirror images that pixel fingerprints miss.
    Visual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Library,
    /// A folder and its sub-folders.
    Folder(i64),
    /// Groups with photos both in a folder (and its sub-folders) and elsewhere.
    Against(i64),
}

/// Near-duplicate levels: how many of the 64 perceptual-hash bits may differ.
/// Copies: resized, recompressed or converted.
pub const STRICT: u32 = 4;
/// Also re-exports, other renderings of one RAW and light edits.
pub const NORMAL: u32 = 7;
/// Also frames of one moment that hardly differ (and the odd look-alike).
pub const LOOSE: u32 = 10;

/// Look-alike levels: the cosine of two photos' image embeddings, at least. Crops and
/// edits of one photo stay above 0.95; frames of one moment 0.92 to 0.98; photos of
/// one scene from another angle 0.88 to 0.92.
pub const ALIKE_STRICT: f32 = 0.95;
pub const ALIKE_NORMAL: f32 = 0.92;
pub const ALIKE_LOOSE: f32 = 0.88;

/// Pairs looked at, at most: a library of near-identical frames could have many more.
const MAX_PAIRS: usize = 4_000_000;

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub mode: Mode,
    pub scope: Scope,
    /// Near-duplicates: bits apart, at most.
    pub radius: u32,
    /// Look-alikes: cosine, at least.
    pub alike: f32,
    pub series: bursts::Rules,
    /// Groups returned, at most (the totals count them all).
    pub limit: usize,
}

impl Default for Params {
    fn default() -> Params {
        Params { mode: Mode::Exact, scope: Scope::Library, radius: NORMAL, alike: ALIKE_NORMAL, series: crate::tuning::get().series_rules(), limit: 2000 }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub id: i64,
    pub name: String,
    pub folder: String,
    pub folder_id: i64,
    /// Thumbnail version, as photo lists give it.
    pub v: String,
    /// File-type badge: `cr3`, `cr3+jpg`.
    pub label: String,
    pub kind: i32,
    pub w: Option<i64>,
    pub h: Option<i64>,
    pub orientation: i32,
    /// Bytes, its JPEG/HEIF twin included.
    pub bytes: i64,
    pub taken: Option<i64>,
    pub rating: i32,
    pub lcolor: Option<String>,
    pub pick: i32,
    /// How much metadata it carries: rating, label, title, caption, keywords, place, creator.
    pub meta: u8,
    /// Perceptual-hash bits that differ from the keeper's (near-duplicates and series).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub d: Option<u8>,
    /// How alike it looks to the keeper, in percent (look-alikes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alike: Option<u8>,
    /// A turned or mirrored copy of the keeper: how ("Turned 90°", "Mirrored").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turned: Option<&'static str>,
    /// In the folder (folder against the library).
    pub inside: bool,
    #[serde(skip)]
    pub mtime: i64,
    #[serde(skip)]
    pub hash: Option<u64>,
    /// Its turned and mirrored hashes.
    #[serde(skip)]
    pub dih: Option<[u64; 7]>,
    #[serde(skip)]
    pub sharp: Option<f64>,
    #[serde(skip)]
    pub companion: bool,
    /// In a backup or export folder, or named as a copy.
    #[serde(skip)]
    pub copyish: bool,
}

#[derive(Debug, Serialize)]
pub struct Group {
    pub key: String,
    /// The suggested keeper first.
    pub members: Vec<Member>,
    pub keeper: i64,
    /// Why that one.
    pub reason: String,
    /// What the others take up.
    pub extra_bytes: i64,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    /// The newest groups, up to the limit asked for.
    pub groups: Vec<Group>,
    /// Groups found, and the photos in them, and how many of those are not keepers.
    pub total_groups: usize,
    pub photos: usize,
    pub extra: usize,
    /// What the non-keepers of the groups returned take up.
    pub extra_bytes: i64,
    /// The search stopped at its limit of pairs: there may be more groups.
    pub truncated: bool,
    /// Files (exact copies) or photos (the others) the search covered, of how many, and
    /// how many could not be read.
    pub covered: i64,
    pub eligible: i64,
    pub unreadable: i64,
    /// Pairs dismissed as "not duplicates".
    pub dismissed: i64,
}

// ------------------------------------------------------------------ scope

/// SQL for "this folder and everything below it" on `f.path` (paths below `a` sort
/// between `a/` and `a0`, since '0' follows '/'); nothing for the library root.
pub(crate) fn subtree(path: &str, args: &mut Vec<Box<dyn ToSql>>) -> String {
    if path.is_empty() {
        return "1".into();
    }
    args.push(Box::new(path.to_string()));
    args.push(Box::new(format!("{path}/")));
    args.push(Box::new(format!("{path}0")));
    "(f.path = ? OR (f.path >= ? AND f.path < ?))".into()
}

pub(crate) fn folder_path(c: &Connection, id: i64) -> Result<String> {
    c.query_row("SELECT path FROM folders WHERE id = ?1", [id], |r| r.get(0)).map_err(|_| crate::api::missing("folder"))
}

/// The photos (as shown) in a folder and below it.
fn photos_below(c: &Connection, path: &str) -> Result<HashSet<i64>> {
    let mut args = Vec::new();
    let sql = format!("SELECT p.id FROM photos p JOIN folders f ON f.id = p.folder_id WHERE p.pair_of IS NULL AND {}", subtree(path, &mut args));
    let mut st = c.prepare(&sql)?;
    let ids = st.query_map(params_from_iter(args.iter().map(|b| b.as_ref())), |r| r.get(0))?.collect::<rusqlite::Result<HashSet<i64>>>()?;
    Ok(ids)
}

fn ignored(c: &Connection) -> Result<HashSet<(i64, i64)>> {
    let mut st = c.prepare("SELECT a, b FROM dupes_ignored")?;
    let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<HashSet<(i64, i64)>>>()?;
    Ok(v)
}

fn pair(a: i64, b: i64) -> (i64, i64) {
    if a < b { (a, b) } else { (b, a) }
}

// ------------------------------------------------------------------ grouping

struct UnionFind(Vec<usize>);

impl UnionFind {
    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

/// Exact copies: photos with an original of the same content hash (either file of a
/// RAW+JPEG pair counts for the pair), joined into groups.
pub fn exact_groups(rows: &[(String, i64)], allow: &dyn Fn(i64) -> bool, ignored: &HashSet<(i64, i64)>) -> Vec<Vec<i64>> {
    let mut rows: Vec<&(String, i64)> = rows.iter().filter(|r| allow(r.1)).collect();
    rows.sort_unstable();
    let ignored_ids: HashSet<i64> = ignored.iter().flat_map(|&(a, b)| [a, b]).collect();
    let mut index: HashMap<i64, usize> = HashMap::new();
    let mut ids: Vec<i64> = Vec::new();
    let mut uf = UnionFind(Vec::new());
    let mut i = 0;
    while i < rows.len() {
        let mut j = i + 1;
        while j < rows.len() && rows[j].0 == rows[i].0 {
            j += 1;
        }
        let mut same: Vec<i64> = rows[i..j].iter().map(|r| r.1).collect();
        same.dedup();
        if same.len() > 1 {
            let at: Vec<usize> = same
                .iter()
                .map(|&id| {
                    *index.entry(id).or_insert_with(|| {
                        ids.push(id);
                        uf.0.push(uf.0.len());
                        ids.len() - 1
                    })
                })
                .collect();
            if same.iter().any(|id| ignored_ids.contains(id)) {
                for a in 0..same.len() {
                    for b in a + 1..same.len() {
                        if !ignored.contains(&pair(same[a], same[b])) {
                            uf.union(at[a], at[b]);
                        }
                    }
                }
            } else {
                for &k in &at[1..] {
                    uf.union(at[0], k);
                }
            }
        }
        i = j;
    }
    let mut groups: HashMap<usize, Vec<i64>> = HashMap::new();
    for k in 0..ids.len() {
        let root = uf.find(k);
        groups.entry(root).or_default().push(ids[k]);
    }
    let mut out: Vec<Vec<i64>> = groups.into_values().filter(|g| g.len() > 1).collect();
    for g in &mut out {
        g.sort_unstable();
    }
    out
}

/// Near-duplicates: photos whose perceptual hashes are at most `radius` bits apart.
/// Groups form around the photo with the most look-alikes (each member is within
/// `radius` of it, so a slow drift through a timelapse does not chain into one group),
/// then the next among those left. Returns the groups and whether the pair limit cut
/// the search short.
pub fn near_groups(rows: &[(i64, u64)], allow: &dyn Fn(i64) -> bool, ignored: &HashSet<(i64, i64)>, radius: u32) -> (Vec<Vec<i64>>, bool) {
    near_groups_turned(rows, &HashMap::new(), allow, ignored, radius)
}

/// `near_groups`, where a photo also matches another when one of its turned or mirrored
/// versions (`turned`: by photo) is near the other's upright hash.
pub fn near_groups_turned(rows: &[(i64, u64)], turned: &HashMap<i64, [u64; 7]>, allow: &dyn Fn(i64) -> bool, ignored: &HashSet<(i64, i64)>, radius: u32) -> (Vec<Vec<i64>>, bool) {
    let mut rows: Vec<(u64, i64)> = rows.iter().filter(|r| r.1 != phash::FLAT && allow(r.0)).map(|&(id, h)| (h, id)).collect();
    rows.sort_unstable();
    // Identical hashes once: a thousand frames that hash alike are one entry, not a
    // million pairs.
    let mut uniq: Vec<u64> = Vec::new();
    let mut members: Vec<std::ops::Range<usize>> = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let mut j = i + 1;
        while j < rows.len() && rows[j].0 == rows[i].0 {
            j += 1;
        }
        uniq.push(rows[i].0);
        members.push(i..j);
        i = j;
    }
    let found = mih::pairs(&uniq, radius, MAX_PAIRS);
    let mut adj: Vec<Vec<(u32, u8)>> = vec![Vec::new(); uniq.len()];
    for &(a, b, d) in &found.pairs {
        adj[a as usize].push((b, d));
        adj[b as usize].push((a, d));
    }
    // Turned and mirrored copies: each distinct hash's variants against every upright one.
    if !turned.is_empty() {
        use rayon::prelude::*;
        let index = mih::Index::new(&uniq);
        let masks = mih::Index::masks(radius);
        let extra: Vec<(u32, u32, u8)> = (0..uniq.len())
            .into_par_iter()
            .with_min_len(256)
            .flat_map_iter(|u| {
                let mut out = Vec::new();
                let Some(vs) = members[u].clone().find_map(|r| turned.get(&rows[r].1)) else { return out };
                let mut hits = Vec::new();
                for &v in vs.iter().filter(|&&v| v != phash::FLAT) {
                    hits.clear();
                    index.query(v, radius, &masks, &mut hits);
                    out.extend(hits.iter().filter(|&&(w, _)| w as usize != u).map(|&(w, d)| (u as u32, w, d)));
                }
                out
            })
            .collect();
        for (a, b, d) in extra {
            adj[a as usize].push((b, d));
            adj[b as usize].push((a, d));
        }
        for list in &mut adj {
            list.sort_unstable_by_key(|&(v, d)| (v, d));
            list.dedup_by_key(|x| x.0);
        }
    }
    let size = |u: usize| members[u].len();
    let weight: Vec<usize> = (0..uniq.len()).map(|u| size(u) - 1 + adj[u].iter().map(|&(v, _)| size(v as usize)).sum::<usize>()).collect();
    let mut order: Vec<usize> = (0..uniq.len()).filter(|&u| weight[u] > 0).collect();
    order.sort_unstable_by(|&a, &b| weight[b].cmp(&weight[a]).then(uniq[a].cmp(&uniq[b])));
    let mut taken = vec![false; rows.len()];
    let mut out = Vec::new();
    for u in order {
        let Some(lead) = members[u].clone().find(|&r| !taken[r]) else { continue };
        let leader = rows[lead].1;
        let mut near: Vec<(u8, usize)> = members[u].clone().filter(|&r| r != lead).map(|r| (0, r)).collect();
        let mut nb = adj[u].clone();
        nb.sort_unstable_by_key(|&(v, d)| (d, v));
        for (v, d) in nb {
            near.extend(members[v as usize].clone().map(|r| (d, r)));
        }
        let mut group = vec![lead];
        group.extend(near.into_iter().filter(|&(_, r)| !taken[r] && !ignored.contains(&pair(leader, rows[r].1))).map(|(_, r)| r));
        if group.len() > 1 {
            for &r in &group {
                taken[r] = true;
            }
            out.push(group.into_iter().map(|r| rows[r].1).collect());
        }
    }
    (out, found.truncated)
}

/// Look-alikes: photos whose embeddings have a cosine of at least `floor`, grouped like
/// near-duplicates (around the photo with the most look-alikes; each member is that
/// close to it). `ids[i]` owns `vectors[i * dim..]`.
pub fn visual_groups(ids: &[i64], vectors: &[i8], norms: &[f32], dim: usize, allow: &dyn Fn(i64) -> bool, ignored: &HashSet<(i64, i64)>, floor: f32) -> (Vec<Vec<i64>>, bool) {
    // Only the photos in scope (a copy only when the scope is narrower than the library).
    let keep: Vec<usize> = (0..ids.len()).filter(|&i| allow(ids[i])).collect();
    let (ids, vectors, norms): (Vec<i64>, std::borrow::Cow<[i8]>, std::borrow::Cow<[f32]>) = if keep.len() == ids.len() {
        (ids.to_vec(), std::borrow::Cow::Borrowed(vectors), std::borrow::Cow::Borrowed(norms))
    } else {
        let mut v = Vec::with_capacity(keep.len() * dim);
        for &i in &keep {
            v.extend_from_slice(&vectors[i * dim..(i + 1) * dim]);
        }
        (keep.iter().map(|&i| ids[i]).collect(), std::borrow::Cow::Owned(v), std::borrow::Cow::Owned(keep.iter().map(|&i| norms[i]).collect()))
    };
    let n = ids.len();
    if n < 2 {
        return (Vec::new(), false);
    }
    // All pairs for a folder; an inverted file (approximate, a few seconds for 300,000
    // photos) for the library.
    let (pairs, truncated) = if n <= 12_000 {
        (crate::ann::pairs_exact(&vectors, &norms, dim, floor), false)
    } else {
        let ivf = crate::ann::Ivf::build(&vectors, &norms, dim, 0);
        ivf.pairs(&vectors, &norms, floor, 4, MAX_PAIRS)
    };
    let mut adj: Vec<Vec<(u32, f32)>> = vec![Vec::new(); n];
    for &(a, b, s) in &pairs {
        if ignored.contains(&pair(ids[a as usize], ids[b as usize])) {
            continue;
        }
        adj[a as usize].push((b, s));
        adj[b as usize].push((a, s));
    }
    let mut order: Vec<usize> = (0..n).filter(|&u| !adj[u].is_empty()).collect();
    order.sort_unstable_by(|&a, &b| adj[b].len().cmp(&adj[a].len()).then(ids[a].cmp(&ids[b])));
    let mut taken = vec![false; n];
    let mut out = Vec::new();
    for u in order {
        if taken[u] {
            continue;
        }
        let mut nb = adj[u].clone();
        nb.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut group = vec![u];
        group.extend(nb.into_iter().map(|(v, _)| v as usize).filter(|&v| !taken[v]));
        if group.len() > 1 {
            for &r in &group {
                taken[r] = true;
            }
            out.push(group.into_iter().map(|r| ids[r]).collect());
        }
    }
    (out, truncated)
}

/// Series, without the ones dismissed (frames dismissed with the first are left out).
pub fn series_groups(shots: &mut [bursts::Shot], rules: bursts::Rules, ignored: &HashSet<(i64, i64)>, looks: Option<&dyn bursts::Looks>) -> Vec<Vec<i64>> {
    let runs = bursts::series(shots, rules, looks);
    let mut out = Vec::new();
    for run in runs {
        let s = &shots[run];
        let first = s[0].id;
        let g: Vec<i64> = s.iter().map(|x| x.id).filter(|&id| id == first || !ignored.contains(&pair(first, id))).collect();
        if g.len() >= rules.min_len.max(2) {
            out.push(g);
        }
    }
    out
}

// ------------------------------------------------------------------ the keeper

fn kind_rank(kind: i32) -> i64 {
    match Kind::from_i32(kind) {
        Kind::Raw => 3,
        Kind::Tiff | Kind::Png => 2,
        Kind::Heic | Kind::Jpeg => 1,
    }
}

fn kind_name(kind: i32) -> &'static str {
    match Kind::from_i32(kind) {
        Kind::Raw => "RAW",
        Kind::Tiff => "TIFF",
        Kind::Png => "PNG",
        Kind::Heic => "HEIC",
        Kind::Jpeg => "JPEG",
    }
}

fn pixels(m: &Member) -> i64 {
    m.w.unwrap_or(0) * m.h.unwrap_or(0)
}

fn mp(px: i64) -> String {
    let v = px as f64 / 1e6;
    if v >= 10.0 { format!("{v:.0} MP") } else { format!("{v:.1} MP") }
}

/// A way one photo beats another (greater is better) and how to say so.
struct Crit {
    cmp: fn(&Member, &Member) -> Ordering,
    why: fn(&Member, &Member) -> String,
}

/// Folder or file names of backups, exports and copies (whole words).
const COPY_WORDS: [&str; 16] = [
    "backup", "backups", "copy", "copies", "export", "exports", "exported", "duplicate", "duplicates", "dupes", "old", "temp", "tmp", "trash", "recovered", "recycle",
];

/// Whether a photo sits in a backup, export or temporary folder, or is named as a copy
/// (`IMG_1 copy.jpg`, `IMG_1 (2).jpg`).
fn looks_like_copy(m: &Member) -> bool {
    let words = |s: &str| s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect::<Vec<_>>();
    let stem = m.name.rsplit_once('.').map_or(m.name.as_str(), |(s, _)| s);
    words(&m.folder).iter().chain(words(stem).iter()).any(|w| COPY_WORDS.contains(&w.as_str()))
        || stem.trim_end().strip_suffix(')').and_then(|s| s.rsplit_once('(')).is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

const fn by(f: fn(&Member) -> i64) -> fn(&Member) -> i64 {
    f
}

macro_rules! crit {
    ($value:expr, $why:expr) => {
        Crit { cmp: |a, b| by($value)(a).cmp(&by($value)(b)), why: $why }
    };
}

const NOT_REJECTED: Crit = crit!(|m| (m.pick != -1) as i64, |_, _| "Not flagged as rejected".into());
const FORMAT: Crit = crit!(|m| kind_rank(m.kind), |k, o| format!("{} over {}", kind_name(k.kind), kind_name(o.kind)));
const PIXELS: Crit = crit!(pixels, |k, o| format!("Most pixels ({} vs {})", mp(pixels(k)), mp(pixels(o))));
const METADATA: Crit = crit!(|m| m.meta as i64, |_, _| "Most metadata".into());
/// Folder against the library: the copy elsewhere stays, the folder's goes.
const OUTSIDE: Crit = crit!(|m| !m.inside as i64, |_, _| "Already elsewhere in the library".into());
const PAIR: Crit = crit!(|m| m.companion as i64, |_, _| "RAW with its JPEG".into());
const RATING: Crit = crit!(|m| m.rating as i64, |k, _| format!("Highest rating ({}★)", k.rating));
const PICKED: Crit = crit!(|m| (m.pick == 1) as i64, |_, _| "Picked".into());
const NOT_A_COPY: Crit = crit!(|m| !m.copyish as i64, |_, _| "Not in a backup or export folder".into());
const OLDEST: Crit = crit!(|m| -m.mtime, |_, _| "Oldest file".into());
/// Within 10% counts as alike: the estimate is not finer than that.
const SHARP: Crit = Crit {
    cmp: |a, b| {
        let (x, y) = (a.sharp.unwrap_or(0.0), b.sharp.unwrap_or(0.0));
        if (x - y).abs() <= 0.1 * x.max(y) { Ordering::Equal } else { x.total_cmp(&y) }
    },
    why: |_, _| "Sharpest (estimate)".into(),
};

/// RAW over JPEG, most pixels, most metadata.
const COPIES: [Crit; 9] = [NOT_REJECTED, FORMAT, PIXELS, METADATA, OUTSIDE, PAIR, RATING, NOT_A_COPY, OLDEST];
/// The frame you rated or picked, else the sharpest.
const FRAMES: [Crit; 8] = [NOT_REJECTED, RATING, PICKED, SHARP, FORMAT, PIXELS, METADATA, PAIR];

fn better(a: &Member, b: &Member, crits: &[Crit]) -> Ordering {
    for c in crits {
        let o = (c.cmp)(a, b);
        if o.is_ne() {
            return o;
        }
    }
    Ordering::Equal
}

/// Puts the suggested keeper first and says why. Ties go to the first member (the
/// lowest id for copies, the first frame for a series).
pub fn choose_keeper(members: &mut Vec<Member>, series: bool) -> String {
    let crits: &[Crit] = if series { &FRAMES } else { &COPIES };
    let best_of = |skip: Option<usize>| -> Option<usize> {
        let mut best: Option<usize> = None;
        for i in 0..members.len() {
            if Some(i) == skip {
                continue;
            }
            if best.is_none_or(|b| better(&members[i], &members[b], crits).is_gt()) {
                best = Some(i);
            }
        }
        best
    };
    let Some(k) = best_of(None) else { return String::new() };
    let reason = match best_of(Some(k)) {
        Some(r) => crits
            .iter()
            .find(|c| (c.cmp)(&members[k], &members[r]).is_gt())
            .map(|c| (c.why)(&members[k], &members[r]))
            .unwrap_or_else(|| (if series { "First frame" } else { "Found first" }).into()),
        None => String::new(),
    };
    let keeper = members.remove(k);
    members.insert(0, keeper);
    reason
}

// ------------------------------------------------------------------ the report

const MEMBER_COLS: &str = "p.id, p.name, f.path, p.folder_id, p.kind, p.ext, p.companion, p.width, p.height, p.orientation, p.size,
    CASE WHEN p.companion IS NULL THEN p.size ELSE p.size + COALESCE((SELECT sum(c.size) FROM photos c WHERE c.pair_of = p.id), 0) END,
    p.mtime, p.taken, p.rating, p.lcolor, p.pick,
    (p.rating > 0) + (p.label IS NOT NULL) + (p.title IS NOT NULL) + (p.description IS NOT NULL) + (p.keywords IS NOT NULL)
      + (p.lat IS NOT NULL) + (p.creator IS NOT NULL),
    h.hash, h.sharp, h.dih";

/// Everything the report shows and weighs about these photos.
fn load_members(c: &Connection, ids: &[i64]) -> Result<HashMap<i64, Member>> {
    let mut out = HashMap::with_capacity(ids.len());
    let mut label = String::new();
    for chunk in ids.chunks(500) {
        let sql = format!(
            "SELECT {MEMBER_COLS} FROM photos p JOIN folders f ON f.id = p.folder_id
             LEFT JOIN phashes h ON h.photo_id = p.id AND h.size = p.size AND h.mtime = p.mtime AND h.orientation = p.orientation
             WHERE p.id IN ({})",
            vec!["?"; chunk.len()].join(",")
        );
        let mut st = c.prepare_cached(&sql)?;
        let mut rows = st.query(params_from_iter(chunk.iter()))?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let companion: Option<String> = r.get(6)?;
            let (size, mtime, orientation): (i64, i64, i32) = (r.get(10)?, r.get(12)?, r.get(9)?);
            label.clear();
            crate::api::file_label_into(&mut label, r.get_ref(5)?.as_str()?, companion.as_deref());
            let rec = PhotoRec { id, size, mtime, orientation, ..Default::default() };
            let mut m = Member {
                id,
                name: r.get(1)?,
                folder: r.get(2)?,
                folder_id: r.get(3)?,
                v: crate::images::sig_str(&rec),
                label: label.clone(),
                kind: r.get(4)?,
                w: r.get(7)?,
                h: r.get(8)?,
                orientation,
                bytes: r.get(11)?,
                taken: r.get(13)?,
                rating: r.get(14)?,
                lcolor: r.get(15)?,
                pick: r.get(16)?,
                meta: r.get::<_, i64>(17)?.clamp(0, 255) as u8,
                d: None,
                alike: None,
                turned: None,
                inside: false,
                mtime,
                hash: r.get::<_, Option<i64>>(18)?.map(|h| h as u64),
                dih: r.get_ref(20)?.as_blob_or_null()?.and_then(super::dih_from_blob),
                sharp: r.get(19)?,
                companion: companion.is_some(),
                copyish: false,
            };
            m.copyish = looks_like_copy(&m);
            out.insert(id, m);
        }
    }
    Ok(out)
}

/// Originals that may share their content hash with another: (hash, photo as shown,
/// capture time). For a folder (`path`), only the hashes of the originals in it are
/// looked up, wherever else they are.
fn exact_rows(c: &Connection, path: &str) -> Result<Vec<(String, i64, i64)>> {
    let mut args: Vec<Box<dyn ToSql>> = Vec::new();
    let hashes = if path.is_empty() {
        "SELECT hash FROM originals WHERE status = 0 AND hash != '' GROUP BY hash HAVING count(*) > 1".to_string()
    } else {
        args.push(Box::new(path.to_string()));
        args.push(Box::new(format!("{path}/")));
        args.push(Box::new(format!("{path}0")));
        "SELECT hash FROM originals WHERE status = 0 AND hash != '' AND (folder = ? OR (folder >= ? AND folder < ?))".to_string()
    };
    let mut st = c.prepare(&format!(
        "SELECT o.hash, COALESCE(p.pair_of, p.id), COALESCE(p.taken, p.mtime / 1000) FROM originals o
         JOIN folders f ON f.path = o.folder
         JOIN photos p ON p.folder_id = f.id AND p.name = o.name
         WHERE o.status = 0 AND o.hash != '' AND f.is_reject = 0 AND p.size = o.size AND p.mtime = o.mtime
           AND o.hash IN ({hashes})"
    ))?;
    let v = st.query_map(params_from_iter(args.iter().map(|b| b.as_ref())), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

/// A photo the report can see (shown, outside the reject folders).
struct Seen {
    id: i64,
    /// Capture time (else file date), seconds.
    time: i64,
    /// Its fingerprint, made from the file as it is now.
    hash: Option<u64>,
    /// It was tried, and could not be read.
    unreadable: bool,
}

const SEEN_FROM: &str = "FROM photos p JOIN folders f ON f.id = p.folder_id
     LEFT JOIN phashes h ON h.photo_id = p.id AND h.size = p.size AND h.mtime = p.mtime AND h.orientation = p.orientation
     WHERE p.pair_of IS NULL AND f.is_reject = 0";

/// The turned and mirrored hashes of the photos shown (outside the reject folders).
fn turned_hashes(c: &Connection) -> Result<HashMap<i64, [u64; 7]>> {
    let mut st = c.prepare(&format!("SELECT p.id, h.dih {SEEN_FROM} AND h.dih IS NOT NULL"))?;
    let mut out = HashMap::new();
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        if let Some(d) = r.get_ref(1)?.as_blob_or_null()?.and_then(super::dih_from_blob) {
            out.insert(r.get::<_, i64>(0)?, d);
        }
    }
    Ok(out)
}

fn seen(c: &Connection) -> Result<Vec<Seen>> {
    let mut st = c.prepare(&format!("SELECT p.id, COALESCE(p.taken, p.mtime / 1000), h.hash, h.photo_id IS NOT NULL AND h.hash IS NULL {SEEN_FROM}"))?;
    let v = st
        .query_map([], |r| Ok(Seen { id: r.get(0)?, time: r.get(1)?, hash: r.get::<_, Option<i64>>(2)?.map(|h| h as u64), unreadable: r.get(3)? }))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

/// The same with what series need (photos without a capture time have no series).
fn shots(c: &Connection) -> Result<(Vec<Seen>, Vec<bursts::Shot>)> {
    let mut st = c.prepare(&format!(
        "SELECT p.id, COALESCE(p.taken, p.mtime / 1000), h.hash, h.photo_id IS NOT NULL AND h.hash IS NULL, p.taken, p.folder_id,
                COALESCE(p.make, '') || ' ' || COALESCE(p.model, ''), p.name {SEEN_FROM}"
    ))?;
    let (mut seen, mut shots) = (Vec::new(), Vec::new());
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let s = Seen { id: r.get(0)?, time: r.get(1)?, hash: r.get::<_, Option<i64>>(2)?.map(|h| h as u64), unreadable: r.get(3)? };
        if let Some(taken) = r.get::<_, Option<i64>>(4)? {
            shots.push(bursts::Shot { id: s.id, folder: r.get(5)?, camera: r.get(6)?, taken, name: r.get(7)?, hash: s.hash, emb: None });
        }
        seen.push(s);
    }
    Ok((seen, shots))
}

/// Files with a current integrity hash, of how many (exact copies).
fn hashed_files(c: &Connection, path: &str) -> Result<(i64, i64)> {
    if path.is_empty() {
        // The whole library: counted from the tables (a join over every file would take
        // longer than the search); reject folders and files changed since count too.
        let covered: i64 = c.query_row("SELECT count(*) FROM originals WHERE status = 0", [], |r| r.get(0))?;
        let files: i64 = c.query_row("SELECT count(*) FROM photos", [], |r| r.get(0))?;
        return Ok((covered.min(files), files));
    }
    let mut args = Vec::new();
    let below = subtree(path, &mut args);
    let sql = format!(
        "SELECT count(o.id), count(*) FROM photos p JOIN folders f ON f.id = p.folder_id
         LEFT JOIN originals o ON o.folder = f.path AND o.name = p.name AND o.status = 0 AND o.size = p.size AND o.mtime = p.mtime
         WHERE f.is_reject = 0 AND {below}"
    );
    Ok(c.query_row(&sql, params_from_iter(args.iter().map(|b| b.as_ref())), |r| Ok((r.get(0)?, r.get(1)?)))?)
}

pub fn build(c: &Connection, p: &Params) -> Result<Report> {
    build_with(c, p, None)
}

/// The report, with the image embeddings look-alikes need.
pub fn build_with(c: &Connection, p: &Params, visual: Option<&crate::visual::Index>) -> Result<Report> {
    let t = std::time::Instant::now();
    // A series stays in its folder: "against the library" finds none.
    let scope = match (p.mode, p.scope) {
        (Mode::Series, Scope::Against(f)) => Scope::Folder(f),
        (_, s) => s,
    };
    let path = match scope {
        Scope::Library => String::new(),
        Scope::Folder(f) | Scope::Against(f) => folder_path(c, f)?,
    };
    let inside: Option<HashSet<i64>> = match scope {
        Scope::Library => None,
        _ if path.is_empty() => None,
        _ => Some(photos_below(c, &path)?),
    };
    let in_scope = |id: i64| inside.as_ref().is_none_or(|s| s.contains(&id));
    let restrict = matches!(scope, Scope::Folder(_));
    let allow = |id: i64| !restrict || in_scope(id);
    let ignored = ignored(c)?;
    let mut when: HashMap<i64, i64> = HashMap::new();
    let seen_count = |seen: &[Seen]| {
        let (mut covered, mut eligible, mut unreadable) = (0, 0, 0);
        for s in seen.iter().filter(|s| in_scope(s.id)) {
            eligible += 1;
            covered += s.hash.is_some() as i64;
            unreadable += s.unreadable as i64;
        }
        (covered, eligible, unreadable)
    };
    let (mut groups, truncated, (covered, eligible, unreadable)) = match p.mode {
        Mode::Exact => {
            let rows = exact_rows(c, &path)?;
            for &(_, id, time) in &rows {
                let e = when.entry(id).or_insert(time);
                *e = (*e).max(time);
            }
            let pairs: Vec<(String, i64)> = rows.into_iter().map(|(h, id, _)| (h, id)).collect();
            let (covered, files) = hashed_files(c, &path)?;
            (exact_groups(&pairs, &allow, &ignored), false, (covered, files, 0))
        }
        Mode::Near => {
            let seen = seen(c)?;
            let counts = seen_count(&seen);
            let rows: Vec<(i64, u64)> = seen.iter().filter_map(|s| s.hash.map(|h| (s.id, h))).collect();
            let turned = turned_hashes(c)?;
            let (g, cut) = near_groups_turned(&rows, &turned, &allow, &ignored, p.radius);
            when = seen.iter().map(|s| (s.id, s.time)).collect();
            (g, cut, counts)
        }
        Mode::Series => {
            let (seen, mut shots) = shots(c)?;
            let counts = seen_count(&seen);
            shots.retain(|s| allow(s.id));
            when = seen.iter().map(|s| (s.id, s.time)).collect();
            let looks: Option<&dyn bursts::Looks> = match visual {
                Some(ix) if !ix.is_empty() => {
                    for s in shots.iter_mut() {
                        s.emb = ix.slot(s.id);
                    }
                    Some(ix)
                }
                _ => None,
            };
            (series_groups(&mut shots, p.series, &ignored, looks), false, counts)
        }
        Mode::Visual => {
            let seen = seen(c)?;
            when = seen.iter().map(|s| (s.id, s.time)).collect();
            match visual {
                Some(ix) if !ix.is_empty() => {
                    let (ids, vectors, norms) = ix.parts();
                    // Photos shown, outside the reject folders, in scope.
                    let shown: HashSet<i64> = seen.iter().map(|s| s.id).collect();
                    let ok = |id: i64| shown.contains(&id) && allow(id);
                    let covered = seen.iter().filter(|s| in_scope(s.id) && ix.contains(s.id)).count() as i64;
                    let eligible = seen.iter().filter(|s| in_scope(s.id)).count() as i64;
                    let (g, cut) = visual_groups(ids, vectors, norms, crate::visual::DIM, &ok, &ignored, p.alike);
                    (g, cut, (covered, eligible, 0))
                }
                _ => (Vec::new(), false, (0, seen.iter().filter(|s| in_scope(s.id)).count() as i64, 0)),
            }
        }
    };
    if let Scope::Against(_) = scope {
        // Groups with a photo in the folder and one outside it (none for the library root).
        let ins = |id: &i64| inside.as_ref().is_some_and(|s| s.contains(id));
        groups.retain(|g| g.iter().any(ins) && !g.iter().all(ins));
    }
    let grouped = t.elapsed();
    // Newest first; details only for the groups returned.
    let newest = |g: &Vec<i64>| g.iter().map(|id| when.get(id).copied().unwrap_or(0)).max().unwrap_or(0);
    let mut keyed: Vec<(i64, i64, Vec<i64>)> = groups.into_iter().map(|g| (newest(&g), g.iter().copied().min().unwrap_or(0), g)).collect();
    keyed.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let total_groups = keyed.len();
    let photos: usize = keyed.iter().map(|g| g.2.len()).sum();
    keyed.truncate(p.limit.max(1));

    let ids: Vec<i64> = keyed.iter().flat_map(|g| g.2.iter().copied()).collect();
    let mut by_id = load_members(c, &ids)?;
    let series = p.mode == Mode::Series;
    let tag = match p.mode {
        Mode::Exact => "e",
        Mode::Near => "n",
        Mode::Series => "s",
        Mode::Visual => "v",
    };
    let mut out: Vec<Group> = Vec::with_capacity(keyed.len());
    for (_, first, g) in keyed {
        let mut members: Vec<Member> = g.iter().filter_map(|id| by_id.remove(id)).collect();
        if members.len() < 2 {
            continue;
        }
        if !series {
            members.sort_by_key(|m| m.id);
        }
        if let Scope::Against(_) = scope {
            for m in &mut members {
                m.inside = inside.as_ref().is_some_and(|s| s.contains(&m.id));
            }
        }
        let reason = choose_keeper(&mut members, series);
        if p.mode == Mode::Visual {
            if let Some(ix) = visual {
                let k = ix.vector(members[0].id).map(<[i8]>::to_vec);
                for m in &mut members {
                    m.alike = k.as_deref().zip(ix.vector(m.id)).map(|(a, b)| (crate::people::index::cosine(a, b).clamp(0.0, 1.0) * 100.0).round() as u8);
                }
            }
        } else if p.mode != Mode::Exact {
            let kh = members[0].hash.filter(|&h| h != phash::FLAT);
            let kd = members[0].dih;
            for m in &mut members {
                m.d = kh.zip(m.hash.filter(|&h| h != phash::FLAT)).map(|(a, b)| phash::distance(a, b) as u8);
                // Not near upright: a turned or mirrored copy (either way round).
                if p.mode == Mode::Near && m.d.is_none_or(|d| d as u32 > p.radius) {
                    let best = (0..7u8)
                        .filter_map(|k| {
                            let a = kd.zip(m.hash).map(|(v, h)| phash::distance(v[k as usize], h));
                            let b = m.dih.zip(kh).map(|(v, h)| phash::distance(v[k as usize], h));
                            a.into_iter().chain(b).min().map(|d| (d, k + 1))
                        })
                        .min();
                    if let Some((d, k)) = best.filter(|b| b.0 <= p.radius) {
                        m.d = Some(d as u8);
                        m.turned = Some(phash::variant_name(k));
                    }
                }
            }
        }
        let extra_bytes = members[1..].iter().map(|m| m.bytes).sum();
        out.push(Group { key: format!("{first}{tag}"), keeper: members[0].id, reason, extra_bytes, members });
    }
    let dismissed: i64 = c.query_row("SELECT count(*) FROM dupes_ignored", [], |r| r.get(0))?;
    tracing::debug!("duplicates: {total_groups} groups ({:?}) in {:?}, details {:?}", p.mode, grouped, t.elapsed() - grouped);
    Ok(Report {
        total_groups,
        photos,
        extra: photos - total_groups.min(photos),
        extra_bytes: out.iter().map(|g| g.extra_bytes).sum(),
        truncated,
        covered,
        eligible,
        unreadable,
        dismissed,
        groups: out,
    })
}

/// Photos in a folder (and below it) that still need a perceptual hash, to do first.
pub fn unhashed_below(c: &Connection, folder: i64) -> Result<Vec<i64>> {
    let path = folder_path(c, folder)?;
    let mut args = Vec::new();
    let below = subtree(&path, &mut args);
    let sql = format!(
        "SELECT p.id FROM photos p JOIN folders f ON f.id = p.folder_id LEFT JOIN phashes h ON h.photo_id = p.id
         WHERE p.pair_of IS NULL AND f.is_reject = 0 AND {below}
           AND (h.photo_id IS NULL OR h.size != p.size OR h.mtime != p.mtime OR h.orientation != p.orientation)
         ORDER BY p.id DESC LIMIT 20000"
    );
    let mut st = c.prepare(&sql)?;
    let ids = st.query_map(params_from_iter(args.iter().map(|b| b.as_ref())), |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_turned_copy_joins_its_original() {
        let a: u64 = 0x0123_4567_89ab_cdef;
        let x: u64 = 0xfedc_ba98_7654_3210 ^ 0x00ff;
        let rows = vec![(1, a), (2, x ^ 0b101), (3, 0x5555_aaaa_5555_aaaa)];
        // Photo 1 turned 90 degrees hashes like photo 2 (two bits off).
        let turned: HashMap<i64, [u64; 7]> = [(1, [x, 1, 2, 3, 4, 5, 6])].into_iter().collect();
        let none = HashSet::new();
        let (g, _) = near_groups(&rows, &|_| true, &none, 7);
        assert!(g.is_empty(), "upright, nothing is near");
        let (g, _) = near_groups_turned(&rows, &turned, &|_| true, &none, 7);
        assert_eq!(g.len(), 1);
        let mut g0 = g[0].clone();
        g0.sort();
        assert_eq!(g0, vec![1, 2]);
    }

    #[test]
    fn look_alikes_group_around_the_most_alike_photo() {
        use crate::people::index::{norm, quantize};
        let dim = 32;
        let base = |seed: u32| -> Vec<f32> { (0..dim).map(|k| (((k as u32).wrapping_mul(2654435761u32) ^ seed.wrapping_mul(40503)) % 1000) as f32 / 500.0 - 1.0).collect() };
        let a = base(1);
        let b = base(2);
        let near = |v: &[f32], k: usize| -> Vec<f32> { v.iter().enumerate().map(|(i, x)| if i == k { x + 0.15 } else { *x }).collect() };
        let raw: Vec<(i64, Vec<f32>)> = vec![(1, a.clone()), (2, near(&a, 3)), (3, near(&a, 7)), (4, b.clone()), (5, near(&b, 1)), (6, base(3))];
        let ids: Vec<i64> = raw.iter().map(|r| r.0).collect();
        let mut vectors = Vec::new();
        let mut norms = Vec::new();
        for (_, v) in &raw {
            let q = quantize(v);
            norms.push(norm(&q));
            vectors.extend(q);
        }
        let none = HashSet::new();
        let (mut g, cut) = visual_groups(&ids, &vectors, &norms, dim, &|_| true, &none, 0.95);
        assert!(!cut);
        g.iter_mut().for_each(|x| x.sort_unstable());
        g.sort();
        assert_eq!(g, vec![vec![1, 2, 3], vec![4, 5]]);
        // A pair dismissed as "not duplicates" stays apart; the scope narrows the search.
        let ign: HashSet<(i64, i64)> = [(4, 5)].into_iter().collect();
        let (g, _) = visual_groups(&ids, &vectors, &norms, dim, &|_| true, &ign, 0.95);
        assert_eq!(g.len(), 1);
        let (g, _) = visual_groups(&ids, &vectors, &norms, dim, &|id| id != 1 && id != 2, &none, 0.95);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].len(), 2);
    }

    use super::*;

    fn member(id: i64, kind: Kind, w: i64, h: i64) -> Member {
        Member {
            id,
            name: format!("IMG_{id}"),
            folder: "a".into(),
            folder_id: 1,
            alike: None,
            turned: None,
            dih: None,
            v: String::new(),
            label: String::new(),
            kind: kind as i32,
            w: Some(w),
            h: Some(h),
            orientation: 1,
            bytes: 1000,
            taken: Some(0),
            rating: 0,
            lcolor: None,
            pick: 0,
            meta: 0,
            d: None,
            inside: false,
            mtime: 0,
            hash: None,
            sharp: None,
            companion: false,
            copyish: false,
        }
    }

    #[test]
    fn keeper_raw_then_pixels_then_metadata() {
        let mut g = vec![member(1, Kind::Jpeg, 6000, 4000), member(2, Kind::Raw, 6000, 4000), member(3, Kind::Jpeg, 8000, 6000)];
        assert_eq!(choose_keeper(&mut g, false), "RAW over JPEG");
        assert_eq!(g[0].id, 2);
        let mut g = vec![member(1, Kind::Jpeg, 2048, 1365), member(2, Kind::Jpeg, 6000, 4000)];
        assert_eq!(choose_keeper(&mut g, false), "Most pixels (24 MP vs 2.8 MP)");
        assert_eq!(g[0].id, 2);
        let mut g = vec![member(1, Kind::Jpeg, 6000, 4000), member(2, Kind::Jpeg, 6000, 4000)];
        g[1].meta = 3;
        assert_eq!(choose_keeper(&mut g, false), "Most metadata");
        assert_eq!(g[0].id, 2);
        // A copy flagged as rejected is never the keeper, whatever it is.
        let mut g = vec![member(1, Kind::Raw, 6000, 4000), member(2, Kind::Jpeg, 100, 100)];
        g[0].pick = -1;
        assert_eq!(choose_keeper(&mut g, false), "Not flagged as rejected");
        assert_eq!(g[0].id, 2);
        // All alike: the oldest file, then the first found.
        let mut g = vec![member(1, Kind::Jpeg, 10, 10), member(2, Kind::Jpeg, 10, 10)];
        g[1].mtime = -5;
        assert_eq!(choose_keeper(&mut g, false), "Oldest file");
        assert_eq!(g[0].id, 2);
        let mut g = vec![member(1, Kind::Jpeg, 10, 10), member(2, Kind::Jpeg, 10, 10)];
        assert_eq!(choose_keeper(&mut g, false), "Found first");
        assert_eq!(g[0].id, 1);
        // A series: rated, then picked, then the sharpest frame; else the first.
        let mut g = vec![member(1, Kind::Raw, 10, 10), member(2, Kind::Raw, 10, 10), member(3, Kind::Raw, 10, 10)];
        g[1].sharp = Some(2.0);
        g[2].sharp = Some(1.0);
        assert_eq!(choose_keeper(&mut g, true), "Sharpest (estimate)");
        assert_eq!(g[0].id, 2);
        g[2].rating = 3;
        assert_eq!(choose_keeper(&mut g, true), "Highest rating (3★)");
        assert_eq!(g[0].id, 3);
        let mut g = vec![member(1, Kind::Raw, 10, 10), member(2, Kind::Raw, 10, 10)];
        assert_eq!(choose_keeper(&mut g, true), "First frame");
        assert_eq!(g[0].id, 1);
        // Sharpness within 10% is a tie: the first frame.
        let mut g = vec![member(1, Kind::Raw, 10, 10), member(2, Kind::Raw, 10, 10), member(3, Kind::Raw, 10, 10)];
        (g[0].sharp, g[1].sharp, g[2].sharp) = (Some(0.376), Some(0.392), Some(0.129));
        assert_eq!(choose_keeper(&mut g, true), "First frame");
        assert_eq!(g[0].id, 1);
    }

    #[test]
    fn keeper_prefers_the_library_copy() {
        // Folder against the library: the copy elsewhere stays.
        let mut g = vec![member(1, Kind::Jpeg, 10, 10), member(2, Kind::Jpeg, 10, 10)];
        g[0].inside = true;
        assert_eq!(choose_keeper(&mut g, false), "Already elsewhere in the library");
        assert_eq!(g[0].id, 2);
        // A backup or an export loses a tie to the copy in the library proper.
        let mut g = vec![member(1, Kind::Jpeg, 10, 10), member(2, Kind::Jpeg, 10, 10)];
        g[0].folder = "Backup/Camera 2024".into();
        g[1].folder = "2024/2024-06-02 Launch".into();
        (g[0].copyish, g[1].copyish) = (looks_like_copy(&g[0]), looks_like_copy(&g[1]));
        g[1].mtime = 99;
        assert_eq!(choose_keeper(&mut g, false), "Not in a backup or export folder");
        assert_eq!(g[0].id, 2);
        let copy = |folder: &str, name: &str| looks_like_copy(&Member { folder: folder.into(), name: name.into(), ..member(1, Kind::Jpeg, 1, 1) });
        assert!(copy("Exports/Web", "a.jpg"));
        assert!(copy("2024/Trip", "IMG_1 copy.jpg"));
        assert!(copy("2024/Trip", "IMG_1 (2).jpg"));
        assert!(copy("Old photos", "a.jpg"));
        assert!(!copy("2024/Golden Gate", "IMG_1.jpg"));
        assert!(!copy("2024/Trip (Lisbon)", "IMG_1.jpg"));
        assert!(!copy("2024/Exporting light", "IMG_(a).jpg"));
    }

    #[test]
    fn exact_groups_join_pairs_and_respect_dismissals() {
        let none = HashSet::new();
        let all = |_: i64| true;
        // Photo 1 is a RAW+JPEG pair; 2 is a copy of the pair, 3 of its JPEG only;
        // 4 and 5 are copies of each other; 6 is alone; 7's JPEG twin matches itself.
        let rows: Vec<(String, i64)> = vec![
            ("raw1".into(), 1),
            ("jpg1".into(), 1),
            ("raw1".into(), 2),
            ("jpg1".into(), 2),
            ("jpg1".into(), 3),
            ("x".into(), 4),
            ("x".into(), 5),
            ("y".into(), 6),
            ("z".into(), 7),
            ("z".into(), 7),
        ];
        let mut g = exact_groups(&rows, &all, &none);
        g.sort();
        assert_eq!(g, vec![vec![1, 2, 3], vec![4, 5]]);
        // Within a folder holding only 1 and 3.
        let g = exact_groups(&rows, &|id| id == 1 || id == 3, &none);
        assert_eq!(g, vec![vec![1, 3]]);
        // "Not duplicates" for 4 and 5; a third copy of them brings them back.
        let dismissed: HashSet<(i64, i64)> = [(4, 5)].into();
        let mut g = exact_groups(&rows, &all, &dismissed);
        g.sort();
        assert_eq!(g, vec![vec![1, 2, 3]]);
        let mut more = rows.clone();
        more.push(("x".into(), 8));
        let mut g = exact_groups(&more, &all, &dismissed);
        g.sort();
        assert_eq!(g, vec![vec![1, 2, 3], vec![4, 5, 8]]);
    }

    #[test]
    fn near_groups_form_around_the_most_connected_photo() {
        let none = HashSet::new();
        let all = |_: i64| true;
        let base = 0x5555_aaaa_3333_ccccu64;
        let flip = |h: u64, bits: &[u32]| bits.iter().fold(h, |h, b| h ^ (1 << b));
        let rows = vec![
            (1, base),
            (2, flip(base, &[1, 2])),
            (3, flip(base, &[5])),
            (4, base),
            // Drifting away: 5 is near 3's neighbourhood but 9 bits from the base.
            (5, flip(base, &[5, 10, 11, 12, 13, 14, 15, 16, 17])),
            // Unrelated, and a flat image (never compared).
            (6, !base),
            (7, 0),
            (8, 0),
        ];
        let (g, cut) = near_groups(&rows, &all, &none, NORMAL);
        assert!(!cut);
        assert_eq!(g.len(), 1);
        let mut first = g[0].clone();
        first.sort();
        assert_eq!(first, vec![1, 2, 3, 4]);
        // A looser level reaches 5 too.
        let (g, _) = near_groups(&rows, &all, &none, LOOSE);
        let mut first = g[0].clone();
        first.sort();
        assert_eq!(first, vec![1, 2, 3, 4, 5]);
        // Dismissed with the leader: left out.
        let dismissed: HashSet<(i64, i64)> = [(1, 2), (1, 3), (1, 4), (2, 4), (3, 4), (2, 3)].into();
        let (g, _) = near_groups(&rows, &all, &dismissed, NORMAL);
        assert!(g.is_empty(), "{g:?}");
    }
}
