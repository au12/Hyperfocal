//! Library-wide keyword operations: the keyword tree, and moving a keyword (rename,
//! merge, re-parent or remove) on one photo's keyword lists.
//!
//! A photo has two keyword lists in its XMP: `dc:subject`, plain keywords that every
//! app reads, and `lr:hierarchicalSubject`, paths such as `Places|Portugal|Lisbon`
//! (Lightroom, Excire, Hyperfocal). A path's leaf is in the plain list too, as
//! Lightroom writes it; some apps add the path's ancestors as well.
//!
//! The *nodes* of a photo are its paths, plus each plain keyword that is not a segment
//! of one of those paths (a top-level node). Moving node `from` to `to` rewrites every
//! path through `from`, and renames (or drops) the plain keywords that stood for it,
//! leaving any keyword that another remaining path still uses.

use std::collections::{HashMap, HashSet};

/// The segments of a path (`Places|Portugal|Lisbon`), trimmed, without empty ones.
pub fn split(path: &str) -> Vec<String> {
    path.split('|').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

pub fn join(segs: &[String]) -> String {
    segs.join("|")
}

fn lower(s: &str) -> String {
    s.trim().to_lowercase()
}

fn eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim()) || lower(a) == lower(b)
}

fn starts_with(path: &[String], prefix: &[String]) -> bool {
    path.len() >= prefix.len() && path.iter().zip(prefix).all(|(a, b)| eq(a, b))
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.iter().any(|x| eq(x, &s)) {
        v.push(s);
    }
}

/// The nodes of one photo: its paths, then the plain keywords no path covers.
pub fn nodes(subj: &[String], hier: &[String]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    for h in hier {
        let segs = split(h);
        if !segs.is_empty() && !out.iter().any(|n| n.len() == segs.len() && starts_with(n, &segs)) {
            out.push(segs);
        }
    }
    let covered: HashSet<String> = out.iter().flatten().map(|s| lower(s)).collect();
    for s in subj {
        let t = s.trim();
        if !t.is_empty() && !covered.contains(&lower(t)) && !out.iter().any(|n| n.len() == 1 && eq(&n[0], t)) {
            out.push(vec![t.to_string()]);
        }
    }
    out
}

/// Whether one of the photo's nodes is `node` or lies below it.
pub fn has_node(subj: &[String], hier: &[String], node: &[String]) -> bool {
    !node.is_empty() && nodes(subj, hier).iter().any(|n| starts_with(n, node))
}

/// Move node `from` to `to` on one photo's lists (`None` removes it and everything
/// below it). Returns the new (plain, hierarchical) lists; unchanged when the photo
/// has no such node.
pub fn apply_move(subj: &[String], hier: &[String], from: &[String], to: Option<&[String]>) -> (Vec<String>, Vec<String>) {
    if !has_node(subj, hier, from) {
        return (subj.to_vec(), hier.to_vec());
    }
    let n = from.len();
    let mut new_hier: Vec<String> = Vec::new();
    // Keywords that a moved path turned into plain keywords (a path moved to the top level).
    let mut flat: Vec<String> = Vec::new();
    // Segments of removed paths: plain keywords that stood for them go too.
    let mut dropped: Vec<String> = Vec::new();
    let mut through = false;
    for h in hier {
        let segs = split(h);
        if segs.is_empty() {
            continue;
        }
        if !starts_with(&segs, from) {
            push_unique(&mut new_hier, h.trim().to_string());
            continue;
        }
        through = true;
        match to {
            Some(t) => {
                let moved: Vec<String> = t.iter().cloned().chain(segs[n..].iter().cloned()).collect();
                if moved.len() >= 2 {
                    push_unique(&mut new_hier, join(&moved));
                } else if let Some(m) = moved.into_iter().next() {
                    flat.push(m);
                }
            }
            None => dropped.extend(segs[n - 1..].iter().cloned()),
        }
    }
    // A plain top-level keyword moved under a parent gains a path.
    if n == 1 && !through {
        match to {
            Some(t) if t.len() >= 2 => push_unique(&mut new_hier, join(t)),
            Some(t) => flat.extend(t.iter().cloned()),
            None => dropped.push(from[0].clone()),
        }
    }
    let used: HashSet<String> = new_hier.iter().flat_map(|h| split(h)).map(|s| lower(&s)).collect();
    let from_leaf = &from[n - 1];
    let mut had_leaf = false;
    let mut new_subj: Vec<String> = Vec::new();
    for s in subj {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        let is_leaf = eq(t, from_leaf);
        had_leaf |= is_leaf;
        let goes = is_leaf || dropped.iter().any(|d| eq(d, t));
        if goes && !used.contains(&lower(t)) {
            continue;
        }
        push_unique(&mut new_subj, t.to_string());
    }
    if let Some(t) = to
        && had_leaf
        && let Some(leaf) = t.last()
    {
        push_unique(&mut new_subj, leaf.clone());
    }
    for f in flat {
        push_unique(&mut new_subj, f);
    }
    (new_subj, new_hier)
}

/// Why a move cannot be made.
pub fn check_move(from: &[String], to: Option<&[String]>) -> Result<(), String> {
    if from.is_empty() {
        return Err("no keyword given".into());
    }
    if let Some(t) = to {
        if t.is_empty() {
            return Err("the new keyword is empty".into());
        }
        if t.iter().any(|s| s.contains(['\n', '\r'])) {
            return Err("keywords cannot contain line breaks".into());
        }
        if t.len() == from.len() && starts_with(t, from) && t.iter().zip(from).all(|(a, b)| a == b) {
            return Err("nothing to change".into());
        }
        if t.len() > from.len() && starts_with(t, from) {
            return Err("a keyword cannot be moved into itself".into());
        }
    }
    Ok(())
}

/// The library's keyword tree: every node of every photo with the number of photos
/// that have it or a node below it, as (path, count). Paths that differ only in case
/// are one node, shown as first seen.
pub fn tree<'a>(photos: impl Iterator<Item = (&'a [String], &'a [String])>) -> Vec<(String, u64)> {
    let mut counts: HashMap<String, (String, u64)> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (subj, hier) in photos {
        seen.clear();
        for node in nodes(subj, hier) {
            for i in 1..=node.len() {
                let path = join(&node[..i]);
                let key = lower(&path);
                if seen.insert(key.clone()) {
                    counts.entry(key).or_insert_with(|| (path, 0)).1 += 1;
                }
            }
        }
    }
    let mut out: Vec<(String, u64)> = counts.into_values().collect();
    out.sort_by(|a, b| lower(&a.0).cmp(&lower(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn mv(subj: &[&str], hier: &[&str], from: &str, to: Option<&str>) -> (Vec<String>, Vec<String>) {
        let t = to.map(split);
        apply_move(&v(subj), &v(hier), &split(from), t.as_deref())
    }

    #[test]
    fn photo_nodes() {
        let n = nodes(&v(&["Lisbon", "Portugal", "dog"]), &v(&["Places|Portugal|Lisbon", "People|Ann"]));
        assert_eq!(n, vec![split("Places|Portugal|Lisbon"), split("People|Ann"), vec!["dog".to_string()]]);
        // Case-insensitive, duplicates folded.
        let n = nodes(&v(&["DOG", "dog"]), &v(&["animals|Dog", "Animals|dog"]));
        assert_eq!(n, vec![split("animals|Dog")]);
    }

    #[test]
    fn rename_a_leaf() {
        let (s, h) = mv(&["Lisbon", "Ann"], &["Places|Portugal|Lisbon", "People|Ann"], "Places|Portugal|Lisbon", Some("Places|Portugal|Lisboa"));
        assert_eq!(h, v(&["Places|Portugal|Lisboa", "People|Ann"]));
        assert_eq!(s, v(&["Ann", "Lisboa"]));
    }

    #[test]
    fn rename_a_plain_keyword() {
        let (s, h) = mv(&["Lisbon", "dog"], &[], "Lisbon", Some("Lisboa"));
        assert_eq!((s, h), (v(&["dog", "Lisboa"]), v(&[])));
        // Case-only rename.
        let (s, _) = mv(&["lisbon"], &[], "Lisbon", Some("LISBON"));
        assert_eq!(s, v(&["LISBON"]));
    }

    #[test]
    fn rename_an_intermediate_node() {
        // Lightroom-style plain list with the ancestors in it.
        let (s, h) = mv(&["Lisbon", "Portugal", "Places"], &["Places|Portugal|Lisbon", "Places|Portugal|Porto"], "Places|Portugal", Some("Places|PT"));
        assert_eq!(h, v(&["Places|PT|Lisbon", "Places|PT|Porto"]));
        assert_eq!(s, v(&["Lisbon", "Places", "PT"]));
    }

    #[test]
    fn a_keyword_another_path_still_uses_stays() {
        let (s, h) = mv(&["Lisbon"], &["Places|Portugal|Lisbon", "Streets|Lisbon"], "Places|Portugal|Lisbon", Some("Places|Portugal|Lisboa"));
        assert_eq!(h, v(&["Places|Portugal|Lisboa", "Streets|Lisbon"]));
        assert_eq!(s, v(&["Lisbon", "Lisboa"]));
    }

    #[test]
    fn merge_into_an_existing_keyword() {
        let (s, h) = mv(&["Lisboa", "Lisbon"], &["Places|Portugal|Lisboa", "Places|Portugal|Lisbon"], "Places|Portugal|Lisboa", Some("Places|Portugal|Lisbon"));
        assert_eq!(h, v(&["Places|Portugal|Lisbon"]));
        assert_eq!(s, v(&["Lisbon"]));
    }

    #[test]
    fn reparent_a_plain_keyword() {
        let (s, h) = mv(&["Lisbon", "dog"], &[], "Lisbon", Some("Places|Portugal|Lisbon"));
        assert_eq!(h, v(&["Places|Portugal|Lisbon"]));
        assert_eq!(s, v(&["Lisbon", "dog"]));
    }

    #[test]
    fn reparent_a_subtree() {
        let (s, h) = mv(&["Alfama", "Lisbon"], &["Lisbon|Alfama"], "Lisbon", Some("Places|Portugal|Lisbon"));
        assert_eq!(h, v(&["Places|Portugal|Lisbon|Alfama"]));
        assert_eq!(s, v(&["Alfama", "Lisbon"]));
    }

    #[test]
    fn move_to_the_top_level() {
        let (s, h) = mv(&["Lisbon"], &["Places|Portugal|Lisbon"], "Places|Portugal|Lisbon", Some("Lisbon"));
        assert_eq!((s, h), (v(&["Lisbon"]), v(&[])));
    }

    #[test]
    fn remove_a_subtree() {
        let (s, h) = mv(&["Lisbon", "Alfama", "Ann", "Portugal"], &["Places|Portugal|Lisbon|Alfama", "People|Ann"], "Places|Portugal", None);
        assert_eq!(h, v(&["People|Ann"]));
        assert_eq!(s, v(&["Ann"]));
        let (s, h) = mv(&["junk", "dog"], &[], "junk", None);
        assert_eq!((s, h), (v(&["dog"]), v(&[])));
    }

    #[test]
    fn photos_without_the_node_are_untouched() {
        let subj = v(&["Lisbon"]);
        let hier = v(&["Streets|Lisbon"]);
        // "Lisbon" here is under Streets, not the top-level keyword or Places|Portugal|Lisbon.
        assert_eq!(apply_move(&subj, &hier, &split("Places|Portugal|Lisbon"), Some(&split("X"))), (subj.clone(), hier.clone()));
        assert_eq!(apply_move(&subj, &hier, &split("Lisbon"), None), (subj, hier));
    }

    #[test]
    fn moves_are_checked() {
        assert!(check_move(&split("A|B"), Some(&split("A|B|C"))).is_err());
        assert!(check_move(&split("A|B"), Some(&split("A|B"))).is_err());
        assert!(check_move(&split("A|B"), Some(&split("a|b"))).is_ok(), "a change of case is a rename");
        assert!(check_move(&split("A|B"), Some(&[])).is_err());
        assert!(check_move(&[], None).is_err());
        assert!(check_move(&split("A|B"), Some(&split("A|BC"))).is_ok());
        assert!(check_move(&split("A|B"), None).is_ok());
    }

    #[test]
    fn library_tree() {
        let photos = [
            (v(&["Lisbon", "dog"]), v(&["Places|Portugal|Lisbon"])),
            (v(&["Porto"]), v(&["places|Portugal|Porto"])),
            (v(&["dog"]), v(&[])),
        ];
        let t = tree(photos.iter().map(|(s, h)| (s.as_slice(), h.as_slice())));
        let get = |p: &str| t.iter().find(|(k, _)| k.eq_ignore_ascii_case(p)).map(|x| x.1);
        assert_eq!(get("Places"), Some(2));
        assert_eq!(get("Places|Portugal"), Some(2));
        assert_eq!(get("Places|Portugal|Lisbon"), Some(1));
        assert_eq!(get("dog"), Some(2));
        assert_eq!(get("Lisbon"), None, "a leaf is not also a top-level node");
        // Sorted by path, case-insensitively.
        let keys: Vec<&str> = t.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(keys, vec!["dog", "Places", "Places|Portugal", "Places|Portugal|Lisbon", "places|Portugal|Porto"]);
    }
}
