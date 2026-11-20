//! Face and pet regions (MWG `mwg-rs:Regions`) and the IPTC "person shown" list
//! (`Iptc4xmpExt:PersonInImage`) in XMP: read here, written through ExifTool (`ops`).
//!
//! MWG regions describe the stored image, before EXIF orientation: `people::regions`
//! turns them into the upright coordinates the rest of Hyperfocal uses, and back.

use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;
use serde::{Deserialize, Serialize};

const NS_RDF: &[u8] = b"http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const NS_MWG_RS: &[u8] = b"http://www.metadataworkinggroup.com/schemas/regions/";
const NS_ST_AREA: &[u8] = b"http://ns.adobe.com/xmp/sType/Area#";
const NS_ST_DIM: &[u8] = b"http://ns.adobe.com/xap/1.0/sType/Dimensions#";
const NS_IPTC_EXT: &[u8] = b"http://iptc.org/std/Iptc4xmpExt/2008-02-29/";

/// One region: centre and size normalized to the stored image (MWG's convention).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Region {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `Face`, `Pet`, `Focus`, `BarCode`, ... as written.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Region {
    /// A face or pet region, the kinds Hyperfocal manages.
    pub fn is_people(&self) -> bool {
        matches!(self.kind.as_deref().map(str::to_ascii_lowercase).as_deref(), Some("face" | "pet"))
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Regions {
    /// `AppliedToDimensions` (0 when absent).
    pub w: f64,
    pub h: f64,
    pub list: Vec<Region>,
}

#[derive(Default, Clone)]
struct Area {
    x: Option<f64>,
    y: Option<f64>,
    w: Option<f64>,
    h: Option<f64>,
    unit: Option<String>,
}

#[derive(Default)]
struct Partial {
    name: Option<String>,
    kind: Option<String>,
    area: Area,
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn ns_of<'a>(r: &ResolveResult<'a>) -> &'a [u8] {
    match r {
        ResolveResult::Bound(n) => n.0,
        _ => b"",
    }
}

/// Whether a packet may hold regions or person names (a cheap test before parsing).
pub fn mentions(data: &[u8]) -> bool {
    let has = |needle: &[u8]| data.windows(needle.len()).any(|w| w == needle);
    has(b"metadataworkinggroup.com/schemas/regions") || has(b"PersonInImage")
}

/// The regions and the person names of an XMP packet.
pub fn parse(data: &[u8]) -> (Option<Regions>, Option<Vec<String>>) {
    let start = data.iter().position(|&b| b == b'<').unwrap_or(data.len());
    let mut reader = NsReader::from_reader(&data[start..]);
    reader.config_mut().trim_text(true);
    reader.config_mut().check_end_names = false;
    let mut buf = Vec::new();

    // (namespace, local name) of every open element.
    let mut stack: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut regions: Option<Regions> = None;
    let mut regions_depth: Option<usize> = None;
    let mut list_depth: Option<usize> = None;
    let mut region_depth: Option<usize> = None;
    let mut cur = Partial::default();
    let mut text = String::new();
    let mut persons: Option<Vec<String>> = None;
    let mut persons_depth: Option<usize> = None;

    // Attributes of interest on any element inside a region or the dimensions.
    let read_attrs = |e: &BytesStart, reader: &NsReader<&[u8]>, cur: &mut Partial, regions: &mut Option<Regions>, in_region: bool| {
        for a in e.attributes().flatten() {
            let (res, local) = reader.resolve_attribute(a.key);
            let ns = ns_of(&res);
            let v = a.unescape_value().map(|c| c.into_owned()).unwrap_or_default();
            match (ns, local.as_ref()) {
                (NS_MWG_RS, b"Name") if in_region => cur.name = Some(v.trim().to_string()).filter(|s| !s.is_empty()),
                (NS_MWG_RS, b"Type") if in_region => cur.kind = Some(v.trim().to_string()).filter(|s| !s.is_empty()),
                (NS_ST_AREA, b"x") if in_region => cur.area.x = num(&v),
                (NS_ST_AREA, b"y") if in_region => cur.area.y = num(&v),
                (NS_ST_AREA, b"w") if in_region => cur.area.w = num(&v),
                (NS_ST_AREA, b"h") if in_region => cur.area.h = num(&v),
                (NS_ST_AREA, b"unit") if in_region => cur.area.unit = Some(v),
                (NS_ST_DIM, b"w") if !in_region => {
                    if let Some(r) = regions.as_mut() {
                        r.w = num(&v).unwrap_or(0.0);
                    }
                }
                (NS_ST_DIM, b"h") if !in_region => {
                    if let Some(r) = regions.as_mut() {
                        r.h = num(&v).unwrap_or(0.0);
                    }
                }
                _ => {}
            }
        }
    };

    loop {
        let ev = reader.read_resolved_event_into(&mut buf);
        match ev {
            Err(_) | Ok((_, Event::Eof)) => break,
            Ok((ns, Event::Start(e))) => {
                let ns_b = ns_of(&ns).to_vec();
                let local = e.local_name().as_ref().to_vec();
                if regions_depth.is_none() && ns_b == NS_MWG_RS && local == b"Regions" {
                    regions = Some(Regions::default());
                    regions_depth = Some(stack.len());
                } else if regions_depth.is_some() && list_depth.is_none() && ns_b == NS_MWG_RS && local == b"RegionList" {
                    list_depth = Some(stack.len());
                } else if list_depth.is_some() && region_depth.is_none() && ns_b == NS_RDF && local == b"li" {
                    region_depth = Some(stack.len());
                    cur = Partial::default();
                } else if persons_depth.is_none() && ns_b == NS_IPTC_EXT && local == b"PersonInImage" {
                    persons = Some(Vec::new());
                    persons_depth = Some(stack.len());
                }
                if regions_depth.is_some() {
                    read_attrs(&e, &reader, &mut cur, &mut regions, region_depth.is_some());
                }
                text.clear();
                stack.push((ns_b, local));
            }
            Ok((ns, Event::Empty(e))) => {
                let ns_b = ns_of(&ns).to_vec();
                let local = e.local_name().as_ref().to_vec();
                if regions_depth.is_some() {
                    if list_depth.is_some() && region_depth.is_none() && ns_b == NS_RDF && local == b"li" {
                        // <rdf:li mwg-rs:Name=".." ... /> with everything in attributes.
                        let mut p = Partial::default();
                        read_attrs(&e, &reader, &mut p, &mut regions, true);
                        push_region(&mut regions, p);
                    } else {
                        read_attrs(&e, &reader, &mut cur, &mut regions, region_depth.is_some());
                    }
                }
            }
            Ok((_, Event::Text(t))) => {
                if let Ok(s) = t.unescape() {
                    text.push_str(&s);
                }
            }
            Ok((_, Event::CData(t))) => text.push_str(&String::from_utf8_lossy(&t)),
            Ok((_, Event::End(_))) => {
                let Some((ns_b, local)) = stack.pop() else { continue };
                let depth = stack.len();
                let v = std::mem::take(&mut text);
                if region_depth.is_some() {
                    match (ns_b.as_slice(), local.as_slice()) {
                        (NS_MWG_RS, b"Name") => cur.name = Some(v.trim().to_string()).filter(|s| !s.is_empty()),
                        (NS_MWG_RS, b"Type") => cur.kind = Some(v.trim().to_string()).filter(|s| !s.is_empty()),
                        (NS_ST_AREA, b"x") => cur.area.x = num(&v),
                        (NS_ST_AREA, b"y") => cur.area.y = num(&v),
                        (NS_ST_AREA, b"w") => cur.area.w = num(&v),
                        (NS_ST_AREA, b"h") => cur.area.h = num(&v),
                        (NS_ST_AREA, b"unit") => cur.area.unit = Some(v.trim().to_string()),
                        _ => {}
                    }
                } else if regions_depth.is_some() {
                    match (ns_b.as_slice(), local.as_slice()) {
                        (NS_ST_DIM, b"w") => {
                            if let Some(r) = regions.as_mut() {
                                r.w = num(&v).unwrap_or(0.0);
                            }
                        }
                        (NS_ST_DIM, b"h") => {
                            if let Some(r) = regions.as_mut() {
                                r.h = num(&v).unwrap_or(0.0);
                            }
                        }
                        _ => {}
                    }
                }
                if persons_depth.is_some() && ns_b == NS_RDF && local == b"li" {
                    let s = v.trim();
                    if !s.is_empty() {
                        persons.get_or_insert_with(Vec::new).push(s.to_string());
                    }
                }
                if region_depth == Some(depth) {
                    push_region(&mut regions, std::mem::take(&mut cur));
                    region_depth = None;
                }
                if list_depth == Some(depth) {
                    list_depth = None;
                }
                if regions_depth == Some(depth) {
                    regions_depth = None;
                }
                if persons_depth == Some(depth) {
                    persons_depth = None;
                }
            }
            Ok(_) => {}
        }
        buf.clear();
    }
    (regions, persons)
}

fn push_region(regions: &mut Option<Regions>, p: Partial) {
    let Some(r) = regions.as_mut() else { return };
    let a = p.area;
    // Only normalized areas (the MWG default); pixel areas are rare and need the
    // dimensions, which `AppliedToDimensions` gives.
    let pixel = a.unit.as_deref().is_some_and(|u| u.eq_ignore_ascii_case("pixel"));
    let (Some(mut x), Some(mut y), Some(mut w), Some(mut h)) = (a.x, a.y, a.w, a.h) else { return };
    if pixel {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        x /= r.w;
        y /= r.h;
        w /= r.w;
        h /= r.h;
    }
    if !(0.0..=1.5).contains(&w) || !(0.0..=1.5).contains(&h) {
        return;
    }
    r.list.push(Region { name: p.name, kind: p.kind, x, y, w, h });
}

/// ExifTool's struct syntax needs `,` `=` `{` `}` `[` `]` and `|` escaped with `|`.
pub fn escape_struct(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if matches!(c, ',' | '=' | '{' | '}' | '[' | ']' | '|') {
            out.push('|');
        }
        out.push(c);
    }
    out
}

/// The ExifTool argument value for `-XMP-mwg-rs:RegionInfo=`.
pub fn exiftool_value(r: &Regions) -> String {
    let f = |v: f64| {
        let s = format!("{:.6}", v.clamp(0.0, 1.0));
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        if s.is_empty() { "0".to_string() } else { s }
    };
    let mut out = format!("{{AppliedToDimensions={{W={},H={},Unit=pixel}},RegionList=[", r.w.round() as i64, r.h.round() as i64);
    for (i, g) in r.list.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{{Area={{X={},Y={},W={},H={},Unit=normalized}}", f(g.x), f(g.y), f(g.w), f(g.h)));
        if let Some(n) = &g.name {
            out.push_str(",Name=");
            out.push_str(&escape_struct(n));
        }
        if let Some(k) = &g.kind {
            out.push_str(",Type=");
            out.push_str(&escape_struct(k));
        }
        out.push('}');
    }
    out.push_str("]}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exiftool_and_lightroom_forms() {
        // As ExifTool writes it (elements, parseType Resource).
        let et = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about='' xmlns:Iptc4xmpExt='http://iptc.org/std/Iptc4xmpExt/2008-02-29/'>
  <Iptc4xmpExt:PersonInImage><rdf:Bag><rdf:li>Smith, John</rdf:li></rdf:Bag></Iptc4xmpExt:PersonInImage>
 </rdf:Description>
 <rdf:Description rdf:about='' xmlns:mwg-rs='http://www.metadataworkinggroup.com/schemas/regions/'
  xmlns:stArea='http://ns.adobe.com/xmp/sType/Area#' xmlns:stDim='http://ns.adobe.com/xap/1.0/sType/Dimensions#'>
  <mwg-rs:Regions rdf:parseType='Resource'>
   <mwg-rs:AppliedToDimensions rdf:parseType='Resource'><stDim:h>4000</stDim:h><stDim:unit>pixel</stDim:unit><stDim:w>6000</stDim:w></mwg-rs:AppliedToDimensions>
   <mwg-rs:RegionList><rdf:Bag>
     <rdf:li rdf:parseType='Resource'>
      <mwg-rs:Area rdf:parseType='Resource'><stArea:h>0.15</stArea:h><stArea:unit>normalized</stArea:unit><stArea:w>0.1</stArea:w><stArea:x>0.5</stArea:x><stArea:y>0.4</stArea:y></mwg-rs:Area>
      <mwg-rs:Name>Smith, John</mwg-rs:Name><mwg-rs:Type>Face</mwg-rs:Type>
     </rdf:li>
     <rdf:li rdf:parseType='Resource'>
      <mwg-rs:Area rdf:parseType='Resource'><stArea:h>0.3</stArea:h><stArea:unit>normalized</stArea:unit><stArea:w>0.3</stArea:w><stArea:x>0.2</stArea:x><stArea:y>0.7</stArea:y></mwg-rs:Area>
      <mwg-rs:Name>Rex</mwg-rs:Name><mwg-rs:Type>Pet</mwg-rs:Type>
     </rdf:li>
   </rdf:Bag></mwg-rs:RegionList>
  </mwg-rs:Regions>
 </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (r, p) = parse(et);
        let r = r.unwrap();
        assert_eq!((r.w, r.h), (6000.0, 4000.0));
        assert_eq!(r.list.len(), 2);
        assert_eq!(r.list[0], Region { name: Some("Smith, John".into()), kind: Some("Face".into()), x: 0.5, y: 0.4, w: 0.1, h: 0.15 });
        assert_eq!(r.list[1].name.as_deref(), Some("Rex"));
        assert!(r.list[1].is_people());
        assert_eq!(p, Some(vec!["Smith, John".to_string()]));

        // Lightroom's form: attributes on rdf:Description inside each li.
        let lr = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
   xmlns:stDim="http://ns.adobe.com/xap/1.0/sType/Dimensions#" xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#">
  <mwg-rs:Regions rdf:parseType="Resource">
   <mwg-rs:AppliedToDimensions stDim:w="5472" stDim:h="3648" stDim:unit="pixel"/>
   <mwg-rs:RegionList><rdf:Bag>
     <rdf:li><rdf:Description mwg-rs:Name="Anna" mwg-rs:Type="Face">
       <mwg-rs:Area stArea:x="0.31" stArea:y="0.42" stArea:w="0.08" stArea:h="0.12" stArea:unit="normalized"/>
     </rdf:Description></rdf:li>
     <rdf:li mwg-rs:Type="Focus" mwg-rs:Name="AF"><mwg-rs:Area stArea:x="0.5" stArea:y="0.5" stArea:w="0.02" stArea:h="0.02" stArea:unit="normalized"/></rdf:li>
   </rdf:Bag></mwg-rs:RegionList>
  </mwg-rs:Regions>
 </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (r, p) = parse(lr);
        let r = r.unwrap();
        assert_eq!((r.w, r.h), (5472.0, 3648.0));
        assert_eq!(r.list.len(), 2);
        assert_eq!(r.list[0].name.as_deref(), Some("Anna"));
        assert!((r.list[0].x - 0.31).abs() < 1e-12);
        assert!(!r.list[1].is_people());
        assert_eq!(p, None);
    }

    #[test]
    fn struct_value() {
        let r = Regions {
            w: 6000.0,
            h: 4000.0,
            list: vec![Region { name: Some("Smith, John {jr}".into()), kind: Some("Face".into()), x: 0.5, y: 0.4, w: 0.1, h: 0.15 }],
        };
        assert_eq!(
            exiftool_value(&r),
            "{AppliedToDimensions={W=6000,H=4000,Unit=pixel},RegionList=[{Area={X=0.5,Y=0.4,W=0.1,H=0.15,Unit=normalized},Name=Smith|, John |{jr|},Type=Face}]}"
        );
        assert!(mentions(b"..metadataworkinggroup.com/schemas/regions/.."));
        assert!(!mentions(b"<x:xmpmeta/>"));
        assert_eq!(parse(b"garbage <<<"), (None, None));
    }
}
