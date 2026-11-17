//! Minimal, namespace-aware XMP reader for the fields Hyperfocal manages.
//!
//! Handles both the attribute form (`xmp:Rating="3"` on `rdf:Description`) and the
//! element form (`<xmp:Rating>3</xmp:Rating>`), plus `rdf:Bag/Seq/Alt` arrays.
//! Writing is never done here: changes go through ExifTool, which preserves every
//! other namespace (Lightroom develop settings, Excire data, ...).

use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

const NS_RDF: &[u8] = b"http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const NS_XMP: &[u8] = b"http://ns.adobe.com/xap/1.0/";
const NS_XMPDM: &[u8] = b"http://ns.adobe.com/xmp/1.0/DynamicMedia/";
const NS_DC: &[u8] = b"http://purl.org/dc/elements/1.1/";
const NS_LR: &[u8] = b"http://ns.adobe.com/lightroom/1.0/";
const NS_PS: &[u8] = b"http://ns.adobe.com/photoshop/1.0/";
const NS_IPTC: &[u8] = b"http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/";
const NS_EXIF: &[u8] = b"http://ns.adobe.com/exif/1.0/";
const NS_TIFF: &[u8] = b"http://ns.adobe.com/tiff/1.0/";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Xmp {
    pub rating: Option<i32>,
    pub label: Option<String>,
    pub pick: Option<i32>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub subject: Option<Vec<String>>,
    pub hsubject: Option<Vec<String>>,
    pub creator: Option<String>,
    pub rights: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt: Option<f64>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub sublocation: Option<String>,
    pub orientation: Option<u8>,
    pub date_taken: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prop {
    Rating,
    Label,
    Pick,
    Good,
    Title,
    Description,
    Subject,
    HSubject,
    Creator,
    Rights,
    Lat,
    Lon,
    Alt,
    AltRef,
    City,
    State,
    Country,
    CountryCode,
    Sublocation,
    Orientation,
    DateOriginal,
    DateCreated,
    CreateDate,
}

fn prop_of(ns: &[u8], local: &[u8]) -> Option<Prop> {
    use Prop::*;
    Some(match (ns, local) {
        (NS_XMP, b"Rating") => Rating,
        (NS_XMP, b"Label") => Label,
        (NS_XMP, b"CreateDate") => CreateDate,
        (NS_XMPDM, b"pick") => Pick,
        (NS_XMPDM, b"good") => Good,
        (NS_DC, b"title") => Title,
        (NS_DC, b"description") => Description,
        (NS_DC, b"subject") => Subject,
        (NS_DC, b"creator") => Creator,
        (NS_DC, b"rights") => Rights,
        (NS_LR, b"hierarchicalSubject") => HSubject,
        (NS_PS, b"City") => City,
        (NS_PS, b"State") => State,
        (NS_PS, b"Country") => Country,
        (NS_PS, b"DateCreated") => DateCreated,
        (NS_IPTC, b"CountryCode") => CountryCode,
        (NS_IPTC, b"Location") => Sublocation,
        (NS_EXIF, b"GPSLatitude") => Lat,
        (NS_EXIF, b"GPSLongitude") => Lon,
        (NS_EXIF, b"GPSAltitude") => Alt,
        (NS_EXIF, b"GPSAltitudeRef") => AltRef,
        (NS_EXIF, b"DateTimeOriginal") => DateOriginal,
        (NS_TIFF, b"Orientation") => Orientation,
        _ => return None,
    })
}

#[derive(Default)]
struct Acc {
    alt_ref: Option<String>,
    good: Option<bool>,
    date_created: Option<String>,
    create_date: Option<String>,
}

impl Xmp {
    /// Parse an XMP packet (sidecar file contents or an embedded packet).
    pub fn parse(data: &[u8]) -> Xmp {
        let mut out = Xmp::default();
        let mut acc = Acc::default();
        // Strip a UTF-8 BOM and leading garbage before the first '<'.
        let start = data.iter().position(|&b| b == b'<').unwrap_or(data.len());
        let data = &data[start..];
        let mut reader = NsReader::from_reader(data);
        reader.config_mut().trim_text(true);
        reader.config_mut().check_end_names = false;

        let mut buf = Vec::new();
        // Element stack: what each open element is.
        #[derive(Clone, Copy, PartialEq)]
        enum El {
            Description,
            Prop(Option<Prop>),
            Container,
            Li { is_default: bool },
            Other,
        }
        let mut stack: Vec<El> = Vec::new();
        let mut cur_prop: Option<Prop> = None;
        let mut prop_depth = 0usize;
        let mut values: Vec<String> = Vec::new();
        let mut default_value: Option<String> = None;
        let mut text = String::new();
        let mut li_text = String::new();

        loop {
            let ev = reader.read_resolved_event_into(&mut buf);
            match ev {
                Err(_) => break,
                Ok((_, Event::Eof)) => break,
                Ok((ns, Event::Start(e))) => {
                    let ns_b = match ns {
                        ResolveResult::Bound(n) => n.0.to_vec(),
                        _ => Vec::new(),
                    };
                    let local = e.local_name().as_ref().to_vec();
                    let parent = stack.last().copied();
                    if ns_b == NS_RDF && local == b"Description" && cur_prop.is_none() {
                        read_attrs(&reader, &e, &mut out, &mut acc);
                        stack.push(El::Description);
                    } else if cur_prop.is_none() && parent == Some(El::Description) {
                        let p = prop_of(&ns_b, &local);
                        if p.is_some() {
                            cur_prop = p;
                            prop_depth = stack.len();
                            values.clear();
                            default_value = None;
                            text.clear();
                        }
                        stack.push(El::Prop(p));
                    } else if cur_prop.is_some() && ns_b == NS_RDF && (local == b"Bag" || local == b"Seq" || local == b"Alt") {
                        stack.push(El::Container);
                    } else if cur_prop.is_some() && ns_b == NS_RDF && local == b"li" {
                        let mut is_default = false;
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"xml:lang" {
                                is_default = a.unescape_value().map(|v| v == "x-default").unwrap_or(false);
                            }
                        }
                        li_text.clear();
                        stack.push(El::Li { is_default });
                    } else {
                        stack.push(El::Other);
                    }
                }
                Ok((ns, Event::Empty(e))) => {
                    let ns_b = match ns {
                        ResolveResult::Bound(n) => n.0.to_vec(),
                        _ => Vec::new(),
                    };
                    if ns_b == NS_RDF && e.local_name().as_ref() == b"Description" && cur_prop.is_none() {
                        read_attrs(&reader, &e, &mut out, &mut acc);
                    } else if cur_prop.is_none() && stack.last() == Some(&El::Description) {
                        // <xmp:Rating/> style empty property: an explicit empty value.
                        if let Some(p) = prop_of(&ns_b, e.local_name().as_ref()) {
                            assign(&mut out, &mut acc, p, Vec::new(), None, String::new());
                        }
                    }
                }
                Ok((_, Event::Text(t))) => {
                    let s = t.unescape().map(|c| c.into_owned()).unwrap_or_default();
                    match stack.last() {
                        Some(El::Li { .. }) => li_text.push_str(&s),
                        Some(El::Prop(Some(_))) => text.push_str(&s),
                        _ => {}
                    }
                }
                Ok((_, Event::CData(t))) => {
                    let s = String::from_utf8_lossy(&t).into_owned();
                    match stack.last() {
                        Some(El::Li { .. }) => li_text.push_str(&s),
                        Some(El::Prop(Some(_))) => text.push_str(&s),
                        _ => {}
                    }
                }
                Ok((_, Event::End(_))) => {
                    let top = stack.pop();
                    match top {
                        Some(El::Li { is_default }) => {
                            let v = li_text.trim().to_string();
                            if is_default && default_value.is_none() {
                                default_value = Some(v.clone());
                            }
                            values.push(v);
                            li_text.clear();
                        }
                        Some(El::Prop(Some(p))) if stack.len() == prop_depth && cur_prop == Some(p) => {
                            assign(
                                &mut out,
                                &mut acc,
                                p,
                                std::mem::take(&mut values),
                                default_value.take(),
                                std::mem::take(&mut text),
                            );
                            cur_prop = None;
                        }
                        _ => {}
                    }
                }
                Ok(_) => {}
            }
            buf.clear();
        }

        // Post-processing of values that depend on each other.
        if let (Some(a), Some(r)) = (out.alt, acc.alt_ref.as_deref()) {
            if r.trim() == "1" {
                out.alt = Some(-a.abs());
            }
        }
        if out.pick.is_none() && acc.good == Some(true) {
            out.pick = Some(1);
        }
        if out.date_taken.is_none() {
            out.date_taken = acc.date_created.or(acc.create_date);
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        *self == Xmp::default()
    }
}

fn read_attrs(reader: &NsReader<&[u8]>, e: &BytesStart, out: &mut Xmp, acc: &mut Acc) {
    for a in e.attributes().flatten() {
        let (res, local) = reader.resolve_attribute(a.key);
        let ns = match res {
            ResolveResult::Bound(n) => n.0,
            _ => continue,
        };
        if let Some(p) = prop_of(ns, local.as_ref()) {
            let v = a.unescape_value().map(|c| c.into_owned()).unwrap_or_default();
            assign(out, acc, p, Vec::new(), None, v);
        }
    }
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

fn assign(out: &mut Xmp, acc: &mut Acc, p: Prop, values: Vec<String>, default: Option<String>, text: String) {
    // For simple properties `text` is set; for arrays `values` (and `default` for Alt).
    let simple = || -> Option<String> {
        if !values.is_empty() {
            default.clone().or_else(|| values.first().cloned()).and_then(non_empty)
        } else {
            non_empty(text.clone())
        }
    };
    let list = || -> Vec<String> {
        if !values.is_empty() {
            values.iter().filter_map(|v| non_empty(v.clone())).collect()
        } else {
            non_empty(text.clone()).into_iter().collect()
        }
    };
    match p {
        Prop::Rating => out.rating = simple().and_then(|s| s.parse::<f64>().ok()).map(|v| v.round().clamp(-1.0, 5.0) as i32),
        Prop::Label => out.label = simple(),
        Prop::Pick => out.pick = simple().and_then(|s| s.parse::<f64>().ok()).map(|v| v.round().clamp(-1.0, 1.0) as i32),
        Prop::Good => acc.good = simple().map(|s| s.eq_ignore_ascii_case("true")),
        Prop::Title => out.title = simple(),
        Prop::Description => out.description = simple(),
        Prop::Subject => out.subject = Some(list()),
        Prop::HSubject => out.hsubject = Some(list()),
        Prop::Creator => out.creator = { let l = list(); if l.is_empty() { None } else { Some(l.join("; ")) } },
        Prop::Rights => out.rights = simple(),
        Prop::Lat => out.lat = simple().and_then(|s| parse_coord(&s)),
        Prop::Lon => out.lon = simple().and_then(|s| parse_coord(&s)),
        Prop::Alt => out.alt = simple().and_then(|s| parse_rational(&s)),
        Prop::AltRef => acc.alt_ref = simple(),
        Prop::City => out.city = simple(),
        Prop::State => out.state = simple(),
        Prop::Country => out.country = simple(),
        Prop::CountryCode => out.country_code = simple(),
        Prop::Sublocation => out.sublocation = simple(),
        Prop::Orientation => out.orientation = simple().and_then(|s| s.parse::<u8>().ok()).filter(|v| (1..=8).contains(v)),
        Prop::DateOriginal => out.date_taken = simple(),
        Prop::DateCreated => acc.date_created = simple(),
        Prop::CreateDate => acc.create_date = simple(),
    }
}

/// XMP GPS coordinates: `"35,30.1234N"`, `"35,30,7.4N"` or a plain signed decimal.
pub fn parse_coord(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let last = s.chars().last()?;
    let (body, dir) = if "NSEWnsew".contains(last) {
        (&s[..s.len() - last.len_utf8()], Some(last.to_ascii_uppercase()))
    } else {
        (s, None)
    };
    let parts: Option<Vec<f64>> = body.split(',').map(|p| p.trim().parse::<f64>().ok()).collect();
    let parts = parts?;
    let v = match parts.as_slice() {
        [d] => *d,
        [d, m] => d.abs() + m / 60.0,
        [d, m, s] => d.abs() + m / 60.0 + s / 3600.0,
        _ => return None,
    };
    let v = match dir {
        Some('S') | Some('W') => -v.abs(),
        Some(_) => v.abs(),
        None => v,
    };
    if v.is_finite() { Some(v) } else { None }
}

fn parse_rational(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some((a, b)) = s.split_once('/') {
        let a: f64 = a.trim().parse().ok()?;
        let b: f64 = b.trim().parse().ok()?;
        if b == 0.0 { None } else { Some(a / b) }
    } else {
        s.parse().ok()
    }
}

/// Canonical colour of a label, including the localised names Excire and Bridge write.
pub fn label_color(label: &str) -> Option<&'static str> {
    let l = label.trim().to_lowercase();
    Some(match l.as_str() {
        "red" | "rot" | "rouge" | "rosso" | "rojo" | "vermelho" | "rood" | "czerwony" | "select" => "red",
        "yellow" | "gelb" | "jaune" | "giallo" | "amarillo" | "amarelo" | "geel" | "żółty" | "second" => "yellow",
        "green" | "grün" | "gruen" | "vert" | "verde" | "groen" | "zielony" | "approved" => "green",
        "blue" | "blau" | "bleu" | "blu" | "azul" | "blauw" | "niebieski" | "review" => "blue",
        "purple" | "lila" | "violett" | "violet" | "viola" | "púrpura" | "morado" | "roxo" | "paars" | "fioletowy" | "to do" => "purple",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_form() {
        let x = br#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:xmpDM="http://ns.adobe.com/xmp/1.0/DynamicMedia/"
 xmlns:exif="http://ns.adobe.com/exif/1.0/" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"
 xmp:Rating="4" xmp:Label="Rot" xmpDM:pick="1" exif:GPSLatitude="35,7.38N" exif:GPSLongitude="82,30.6W"
 photoshop:City="Asheville"/>
</rdf:RDF></x:xmpmeta>"#;
        let p = Xmp::parse(x);
        assert_eq!(p.rating, Some(4));
        assert_eq!(p.label.as_deref(), Some("Rot"));
        assert_eq!(label_color("Rot"), Some("red"));
        assert_eq!(p.pick, Some(1));
        assert!((p.lat.unwrap() - 35.123).abs() < 1e-9);
        assert!((p.lon.unwrap() + 82.51).abs() < 1e-9);
        assert_eq!(p.city.as_deref(), Some("Asheville"));
    }

    #[test]
    fn element_form_and_arrays() {
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:lr="http://ns.adobe.com/lightroom/1.0/"
  xmlns:xap="http://ns.adobe.com/xap/1.0/" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:exif="http://ns.adobe.com/exif/1.0/">
 <xap:Rating>2</xap:Rating>
 <tiff:Orientation>6</tiff:Orientation>
 <exif:GPSAltitude>1234/10</exif:GPSAltitude>
 <exif:GPSAltitudeRef>1</exif:GPSAltitudeRef>
 <dc:title><rdf:Alt><rdf:li xml:lang="de">Titel</rdf:li><rdf:li xml:lang="x-default">Blue Ridge &amp; fog</rdf:li></rdf:Alt></dc:title>
 <dc:subject><rdf:Bag><rdf:li>Mountains</rdf:li><rdf:li>Fog</rdf:li></rdf:Bag></dc:subject>
 <lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|NC|Mountains</rdf:li></rdf:Bag></lr:hierarchicalSubject>
 <dc:creator><rdf:Seq><rdf:li>Adam</rdf:li></rdf:Seq></dc:creator>
 <exif:Flash rdf:parseType="Resource"><exif:Fired>False</exif:Fired></exif:Flash>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let p = Xmp::parse(x.as_bytes());
        assert_eq!(p.rating, Some(2));
        assert_eq!(p.orientation, Some(6));
        assert_eq!(p.title.as_deref(), Some("Blue Ridge & fog"));
        assert_eq!(p.subject, Some(vec!["Mountains".to_string(), "Fog".to_string()]));
        assert_eq!(p.hsubject, Some(vec!["Places|NC|Mountains".to_string()]));
        assert_eq!(p.creator.as_deref(), Some("Adam"));
        assert!((p.alt.unwrap() + 123.4).abs() < 1e-9);
    }

    #[test]
    fn coords() {
        assert_eq!(parse_coord("12,30N"), Some(12.5));
        assert_eq!(parse_coord("12,30,36S"), Some(-12.51));
        assert_eq!(parse_coord("-82.5"), Some(-82.5));
        assert_eq!(parse_coord(""), None);
    }

    #[test]
    fn garbage_is_safe() {
        let p = Xmp::parse(b"\xef\xbb\xbf   not xml at all <<<");
        assert!(p.is_empty());
        let p = Xmp::parse(&[0u8; 100]);
        assert!(p.is_empty());
    }
}
