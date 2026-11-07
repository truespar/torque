//! GEO section (TQBF spec §7.6): one packed Hilbert R-tree per geopoint or geopoint[] field, read
//! in place, with Typesense's geo arithmetic (measured on a running server):
//!
//! - Coordinates are truncated to 1e-6 degrees (`int32(lat * 1e6)`), as Typesense packs them.
//! - Radius filter: the S2 cap of `radius / 6371010` radians (S2Earth) around the centre at full
//!   precision, tested on the truncated point as `S2Cap::Contains` does (squared chord between unit
//!   vectors); the radius number is read as a 32-bit float; exact at every radius.
//! - Sort and `geo_distance_meters`: the haversine angle (`S2LatLng::GetDistance`) between the
//!   truncated point and the truncated centre times 6369628.75 m, truncated to whole metres. A
//!   document without a point is 2147483647 m away; a geopoint[] document is as far as its closest
//!   point.
//! - Polygon filter: S2's loop, geodesic edges and the smaller region, exact on the truncated
//!   point (see `polygon`).
//!
//! ```text
//! u32 doc_count, u32 n_points, u32 n_nodes, u8 n_levels, u8 multi, u8 doc_width, u8 pos_width
//! u64 present_len, frozen roaring of the documents with a point, padded to 8
//! u32 level_end[n_levels], padded to 8      cumulative node counts per level, leaves first
//! i32 box[n_nodes][4]                       min_lat, min_lng, max_lat, max_lng (1e-6 degrees)
//! u32 leaf_offset[n_leaves + 1], padded 8   byte offset of each leaf's points
//! u8  leaf_width[n_leaves][2], padded 8     bits of lat - min_lat and of lng - min_lng
//! point_docs   each point's doc id at doc_width, in tree order; 8 zero bytes, padded to 8
//! points       per leaf, its points relative to the leaf box's minimum, lat then lng at the
//!              leaf's widths, each leaf from a byte boundary; 8 zero bytes, padded to 8
//! single:      per document the tree position of its point at pos_width (n_points = none)
//! multi:       monotonic element offsets (doc_count + 1), padded to 8; each document's point
//!              positions at pos_width, in array order
//! ```
//!
//! Points are ordered by the Hilbert value of their position on a 2^16 grid over the field's
//! extent, 16 to a leaf and 16 children to a node (the Flatbush layout), so a leaf box is small and
//! its points take few bits. Node `j` of level `l` covers nodes `16j..16j + 16` of level `l - 1`
//! and tree positions `16^(l+1) j .. 16^(l+1) (j + 1)`. A document's point is one packed read away,
//! so distance sorting needs no separate per-document column.

use crate::bitmap::Bitmap;
use crate::index::values::DocSet;
use crate::schema::{DocId, FieldId};
use crate::tqbf::bytes::Bytes;
use crate::tqbf::container::{kind, SegmentFile, SegmentWriter};
use crate::tqbf::packed::{self, BitWriter, Monotonic};
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::io;

mod polygon;
pub use polygon::Polygon;

/// Earth radius of Typesense's geo sort and `geo_distance_meters` (fitted to 33 distances from a
/// running server, up to 16,679 km: every one is reproduced, the radius is known to +-0.04 m).
pub const SORT_RADIUS_M: f64 = 6_369_628.75;
/// `S2Earth::RadiusMeters()`, the radius of geo filters.
pub const FILTER_RADIUS_M: f64 = 6_371_010.0;
/// Distance of a document without a point.
pub const MISSING_M: u32 = i32::MAX as u32;

const NODE: u64 = 16;
const RAD: f64 = std::f64::consts::PI / 180.0;

/// Degrees to 1e-6 degrees, truncated as Typesense packs coordinates.
#[inline]
pub fn e6(deg: f64) -> i32 {
    (deg * 1_000_000.0) as i32
}

/// 1e-6 degrees to degrees.
#[inline]
pub fn deg(v: i32) -> f64 {
    f64::from(v) / 1_000_000.0
}

/// `S2LatLng::GetDistance`: the haversine angle in radians between two points given in degrees.
#[inline]
pub fn angle(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let (lat1, lng1, lat2, lng2) = (RAD * lat1, RAD * lng1, RAD * lat2, RAD * lng2);
    let dlat = (0.5 * (lat2 - lat1)).sin();
    let dlng = (0.5 * (lng2 - lng1)).sin();
    let x = dlat * dlat + dlng * dlng * lat1.cos() * lat2.cos();
    2.0 * x.min(1.0).sqrt().asin()
}

/// Typesense's distance in whole metres from a sort centre to a point, both truncated.
#[inline]
pub fn sort_metres(point: (i32, i32), centre: (i32, i32)) -> u32 {
    (angle(deg(point.0), deg(point.1), deg(centre.0), deg(centre.1)) * SORT_RADIUS_M) as u32
}

/// A sort centre given in degrees, truncated as Typesense packs it.
#[inline]
pub fn sort_centre(lat: f64, lng: f64) -> (i32, i32) {
    (e6(lat), e6(lng))
}

#[inline]
fn unit(lat: f64, lng: f64) -> [f64; 3] {
    let (phi, theta) = (RAD * lat, RAD * lng);
    let c = phi.cos();
    [theta.cos() * c, theta.sin() * c, phi.sin()]
}

#[inline]
fn chord2(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    let (x, y, z) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    (x * x + y * y + z * z).min(4.0)
}

/// A radius filter: the S2 cap around a centre (degrees, full precision).
#[derive(Clone, Debug)]
pub struct Cap {
    centre: [f64; 3],
    length2: f64,
    /// 1e-6-degree box that holds the cap (min_lat, min_lng, max_lat, max_lng).
    bounds: [i32; 4],
    /// The bounds span less than 90 degrees of longitude, away from the poles, so a node box inside
    /// them is within the cap when its corners are.
    small: bool,
}

impl Cap {
    pub fn new(lat: f64, lng: f64, radius_m: f64) -> Self {
        let a = radius_m / FILTER_RADIUS_M;
        let length = 2.0 * (0.5 * a.min(std::f64::consts::PI)).sin();
        let length2 = if a < 0.0 { -1.0 } else { length * length };
        let margin = a + 1e-9;
        let (lat_lo, lat_hi) = (lat - margin / RAD, lat + margin / RAD);
        let full = [i32::MIN, i32::MIN, i32::MAX, i32::MAX];
        let mut small = false;
        let bounds = if lat_lo <= -90.0 || lat_hi >= 90.0 || margin >= std::f64::consts::FRAC_PI_4 {
            full
        } else {
            let s = margin.sin() / (RAD * lat).cos();
            let dlng = if s >= 1.0 { 180.0 } else { s.asin() / RAD };
            let (lng_lo, lng_hi) = (lng - dlng, lng + dlng);
            if lng_lo < -180.0 || lng_hi > 180.0 {
                [e6(lat_lo) - 1, i32::MIN, e6(lat_hi) + 1, i32::MAX]
            } else {
                small = dlng < 45.0;
                [e6(lat_lo) - 1, e6(lng_lo) - 1, e6(lat_hi) + 1, e6(lng_hi) + 1]
            }
        };
        Self { centre: unit(lat, lng), length2, bounds, small }
    }

    /// Whether the cap holds a truncated point.
    #[inline]
    pub fn contains(&self, lat: i32, lng: i32) -> bool {
        chord2(&self.centre, &unit(deg(lat), deg(lng))) <= self.length2
    }

    fn relate(&self, b: [i32; 4]) -> Relation {
        let o = self.bounds;
        if b[2] < o[0] || b[0] > o[2] || b[3] < o[1] || b[1] > o[3] {
            return Relation::Disjoint;
        }
        // On a sphere the farthest point of a box from a centre within 90 degrees of longitude is a
        // corner. The margin keeps rounding from admitting a point the exact test would refuse.
        if self.small && b[0] >= o[0] && b[1] >= o[1] && b[2] <= o[2] && b[3] <= o[3] {
            let limit = self.length2 * (1.0 - 1e-4);
            let corners = [(b[0], b[1]), (b[0], b[3]), (b[2], b[1]), (b[2], b[3])];
            if corners.iter().all(|&(la, ln)| chord2(&self.centre, &unit(deg(la), deg(ln))) <= limit) {
                return Relation::Within;
            }
        }
        Relation::Crosses
    }
}

/// A bounding-box filter (degrees, inclusive), tested on truncated points.
#[derive(Clone, Copy, Debug)]
pub struct GeoBox {
    pub min_lat: f64,
    pub min_lng: f64,
    pub max_lat: f64,
    pub max_lng: f64,
}

impl GeoBox {
    #[inline]
    pub fn contains(&self, lat: i32, lng: i32) -> bool {
        let (la, ln) = (deg(lat), deg(lng));
        la >= self.min_lat && la <= self.max_lat && ln >= self.min_lng && ln <= self.max_lng
    }

    fn relate(&self, b: [i32; 4]) -> Relation {
        if deg(b[2]) < self.min_lat || deg(b[0]) > self.max_lat || deg(b[3]) < self.min_lng || deg(b[1]) > self.max_lng {
            Relation::Disjoint
        } else if self.contains(b[0], b[1]) && self.contains(b[2], b[3]) {
            Relation::Within
        } else {
            Relation::Crosses
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Relation {
    Disjoint,
    Within,
    Crosses,
}

/// A region a point filter tests.
trait Region {
    fn relate(&self, b: [i32; 4]) -> Relation;
    fn contains(&self, lat: i32, lng: i32) -> bool;
}

impl Region for Cap {
    fn relate(&self, b: [i32; 4]) -> Relation {
        Cap::relate(self, b)
    }
    fn contains(&self, lat: i32, lng: i32) -> bool {
        Cap::contains(self, lat, lng)
    }
}

impl Region for GeoBox {
    fn relate(&self, b: [i32; 4]) -> Relation {
        GeoBox::relate(self, b)
    }
    fn contains(&self, lat: i32, lng: i32) -> bool {
        GeoBox::contains(self, lat, lng)
    }
}

/// Smallest angle from a point (degrees) to any point of a box: on the box's latitude span when
/// the point's longitude is inside the box, else on the nearer meridian edge, where the distance
/// is smallest at an end or at the edge's closest latitude.
fn box_min_angle(lat: f64, lng: f64, b: [i32; 4]) -> f64 {
    let (lat1, lng1, lat2, lng2) = (deg(b[0]), deg(b[1]), deg(b[2]), deg(b[3]));
    if lng >= lng1 && lng <= lng2 {
        return if lat < lat1 {
            angle(lat, lng, lat1, lng)
        } else if lat > lat2 {
            angle(lat, lng, lat2, lng)
        } else {
            0.0
        };
    }
    let wrap = |d: f64| {
        let d = d.abs() % 360.0;
        if d > 180.0 { 360.0 - d } else { d }
    };
    let edge = if wrap(lng1 - lng) <= wrap(lng2 - lng) { lng1 } else { lng2 };
    let dl = RAD * (edge - lng);
    let vertex = (RAD * lat).sin().atan2((RAD * lat).cos() * dl.cos()) / RAD;
    let mut best = angle(lat, lng, lat1, edge).min(angle(lat, lng, lat2, edge));
    if vertex > lat1 && vertex < lat2 {
        best = best.min(angle(lat, lng, vertex, edge));
    }
    best
}

/// A geopoint or geopoint[] field's GEO section.
#[derive(Clone)]
pub struct GeoIndex {
    bytes: Bytes,
    doc_count: u32,
    n_points: u64,
    multi: bool,
    doc_width: u32,
    pos_width: u32,
    present: Bitmap,
    level_end: Vec<u32>,
    boxes: Bytes,
    leaf_offset: Bytes,
    leaf_width: Bytes,
    point_docs: Bytes,
    points: Bytes,
    positions: Bytes,
    offsets: Option<Monotonic>,
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn pad8(out: &mut Vec<u8>) {
    out.resize(out.len().next_multiple_of(8), 0);
}

fn slack_pad(out: &mut Vec<u8>, bytes: Vec<u8>) {
    out.extend_from_slice(&bytes);
    pad8(out);
}

/// Hilbert value of a point on a 2^16 grid (rawrunprotected's method, as Flatbush uses it).
fn hilbert(x: u32, y: u32) -> u32 {
    let mut a = x ^ y;
    let mut b = 0xFFFF ^ a;
    let mut c = 0xFFFF ^ (x | y);
    let mut d = x & (y ^ 0xFFFF);

    let mut aa = a | (b >> 1);
    let mut bb = (a >> 1) ^ a;
    let mut cc = ((c >> 1) ^ (b & (d >> 1))) ^ c;
    let mut dd = ((a & (c >> 1)) ^ (d >> 1)) ^ d;

    a = aa;
    b = bb;
    c = cc;
    d = dd;
    aa = (a & (a >> 2)) ^ (b & (b >> 2));
    bb = (a & (b >> 2)) ^ (b & ((a ^ b) >> 2));
    cc ^= (a & (c >> 2)) ^ (b & (d >> 2));
    dd ^= (b & (c >> 2)) ^ ((a ^ b) & (d >> 2));

    a = aa;
    b = bb;
    c = cc;
    d = dd;
    aa = (a & (a >> 4)) ^ (b & (b >> 4));
    bb = (a & (b >> 4)) ^ (b & ((a ^ b) >> 4));
    cc ^= (a & (c >> 4)) ^ (b & (d >> 4));
    dd ^= (b & (c >> 4)) ^ ((a ^ b) & (d >> 4));

    a = aa;
    b = bb;
    c = cc;
    d = dd;
    cc ^= (a & (c >> 8)) ^ (b & (d >> 8));
    dd ^= (b & (c >> 8)) ^ ((a ^ b) & (d >> 8));

    a = cc ^ (cc >> 1);
    b = dd ^ (dd >> 1);

    let mut i0 = x ^ y;
    let mut i1 = b | (0xFFFF ^ (i0 | a));

    i0 = (i0 | (i0 << 8)) & 0x00FF_00FF;
    i0 = (i0 | (i0 << 4)) & 0x0F0F_0F0F;
    i0 = (i0 | (i0 << 2)) & 0x3333_3333;
    i0 = (i0 | (i0 << 1)) & 0x5555_5555;

    i1 = (i1 | (i1 << 8)) & 0x00FF_00FF;
    i1 = (i1 | (i1 << 4)) & 0x0F0F_0F0F;
    i1 = (i1 | (i1 << 2)) & 0x3333_3333;
    i1 = (i1 | (i1 << 1)) & 0x5555_5555;

    (i1 << 1) | i0
}

fn union_box(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

const EMPTY_BOX: [i32; 4] = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];

/// Documents gathered by a point query: a sorted list while few, a segment-wide bitset after.
enum Gather {
    Few(Vec<DocId>, u32),
    Many(DocSet),
}

impl Gather {
    fn new(doc_count: u32) -> Self {
        Self::Few(Vec::new(), doc_count)
    }

    #[inline]
    fn add(&mut self, doc: DocId) {
        match self {
            Self::Few(v, n) => {
                v.push(doc);
                if v.len() as u64 * 32 > u64::from(*n) {
                    let mut set = DocSet::new(*n);
                    for &d in v.iter() {
                        set.add(d);
                    }
                    *self = Self::Many(set);
                }
            }
            Self::Many(set) => set.add(doc),
        }
    }

    fn finish(self) -> Bitmap {
        match self {
            Self::Few(mut v, _) => {
                v.sort_unstable();
                v.dedup();
                Bitmap::of(&v)
            }
            Self::Many(set) => set.finish(),
        }
    }
}

/// A best-first search entry: a node (level, index) or a point (tree position), by angle.
#[derive(PartialEq)]
struct Entry(f64, u8, u64);

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0).then(self.1.cmp(&other.1)).then(self.2.cmp(&other.2))
    }
}

const POINT: u8 = u8::MAX;

impl GeoIndex {
    /// A field's geo index merged from the sources' (`index::merge`): every point of the documents
    /// carried over, renumbered, in document and array order. None without a point.
    pub(crate) fn merge(srcs: &[Option<&GeoIndex>], docs: &crate::index::merge::DocMap, multi: bool) -> io::Result<Option<Self>> {
        let mut pts: Vec<(DocId, i32, i32)> = Vec::new();
        for (k, &(s, d)) in docs.order().iter().enumerate() {
            if let Some(g) = srcs[s as usize] {
                pts.extend(g.doc_points(d).map(|(la, ln)| (k as DocId, la, ln)));
            }
        }
        if pts.is_empty() {
            return Ok(None);
        }
        Self::build(&pts, docs.len(), multi).map(Some)
    }

    /// Build from every point of the field: (doc, lat, lng) in 1e-6 degrees, in document order
    /// and array order. `multi` for geopoint[] fields.
    pub fn build(pts: &[(DocId, i32, i32)], doc_count: u32, multi: bool) -> io::Result<Self> {
        let n = pts.len() as u64;
        if !multi && pts.windows(2).any(|w| w[0].0 >= w[1].0) {
            return Err(invalid("geopoint field with more than one point per document"));
        }
        let (mut lo, mut hi) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
        for &(_, la, ln) in pts {
            lo = (lo.0.min(la), lo.1.min(ln));
            hi = (hi.0.max(la), hi.1.max(ln));
        }
        let scale = |v: i32, lo: i32, hi: i32| -> u32 {
            let span = (i64::from(hi) - i64::from(lo)).max(1);
            ((i64::from(v) - i64::from(lo)) * 0xFFFF / span) as u32
        };
        let mut order: Vec<(u32, u32)> = pts
            .iter()
            .enumerate()
            .map(|(i, &(_, la, ln))| (hilbert(scale(ln, lo.1, hi.1), scale(la, lo.0, hi.0)), i as u32))
            .collect();
        order.sort_unstable_by_key(|&(h, i)| {
            let (d, la, ln) = pts[i as usize];
            (h, d, la, ln, i)
        });
        let mut pos_of = vec![0u64; pts.len()];
        for (p, &(_, i)) in order.iter().enumerate() {
            pos_of[i as usize] = p as u64;
        }

        // Leaves: boxes, widths, packed points.
        let n_leaves = n.div_ceil(NODE) as usize;
        let mut boxes: Vec<[i32; 4]> = Vec::with_capacity(n_leaves + n_leaves / 15 + 1);
        let mut leaf_offset = Vec::with_capacity(n_leaves + 1);
        let mut leaf_width = Vec::with_capacity(2 * n_leaves);
        let mut points = Vec::new();
        for leaf in order.chunks(NODE as usize) {
            let b = leaf.iter().fold(EMPTY_BOX, |b, &(_, i)| {
                let (_, la, ln) = pts[i as usize];
                union_box(b, [la, ln, la, ln])
            });
            let wl = packed::bits_for((i64::from(b[2]) - i64::from(b[0])) as u64);
            let wg = packed::bits_for((i64::from(b[3]) - i64::from(b[1])) as u64);
            let off = u32::try_from(points.len()).map_err(|_| invalid("GEO points past 4 GB"))?;
            leaf_offset.push(off);
            leaf_width.extend_from_slice(&[wl as u8, wg as u8]);
            let mut w = BitWriter::new();
            for &(_, i) in leaf {
                let (_, la, ln) = pts[i as usize];
                w.push((i64::from(la) - i64::from(b[0])) as u64, wl);
                w.push((i64::from(ln) - i64::from(b[1])) as u64, wg);
            }
            let mut bytes = w.into_bytes();
            bytes.truncate(packed::packed_len(leaf.len() as u64, wl + wg) - 8);
            points.extend_from_slice(&bytes);
            boxes.push(b);
        }
        leaf_offset.push(u32::try_from(points.len()).map_err(|_| invalid("GEO points past 4 GB"))?);
        points.extend_from_slice(&[0; 8]);

        // Upper levels until a single root.
        let mut level_end = vec![boxes.len() as u32];
        let mut start = 0;
        while boxes.len() - start > 1 {
            let end = boxes.len();
            let parents: Vec<[i32; 4]> = boxes[start..end].chunks(NODE as usize).map(|c| c.iter().fold(EMPTY_BOX, |a, &b| union_box(a, b))).collect();
            boxes.extend(parents);
            start = end;
            level_end.push(boxes.len() as u32);
        }

        let doc_width = packed::bits_for(u64::from(doc_count.saturating_sub(1)));
        let pos_width = packed::bits_for(n);
        let mut docs_w = BitWriter::with_bits(n * u64::from(doc_width));
        for &(_, i) in &order {
            docs_w.push(u64::from(pts[i as usize].0), doc_width);
        }
        let mut present: Vec<DocId> = pts.iter().map(|p| p.0).collect();
        present.dedup();

        let mut out = Vec::new();
        out.extend_from_slice(&doc_count.to_le_bytes());
        out.extend_from_slice(&u32::try_from(n).map_err(|_| invalid("GEO with more than 2^32 points"))?.to_le_bytes());
        out.extend_from_slice(&(boxes.len() as u32).to_le_bytes());
        out.extend_from_slice(&[level_end.len() as u8, u8::from(multi), doc_width as u8, pos_width as u8]);
        let mut bm = Vec::new();
        Bitmap::of(&present).write_frozen(&mut bm);
        out.extend_from_slice(&(bm.len() as u64).to_le_bytes());
        out.extend_from_slice(&bm);
        pad8(&mut out);
        for e in &level_end {
            out.extend_from_slice(&e.to_le_bytes());
        }
        pad8(&mut out);
        for b in &boxes {
            for v in b {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        for o in &leaf_offset {
            out.extend_from_slice(&o.to_le_bytes());
        }
        pad8(&mut out);
        out.extend_from_slice(&leaf_width);
        pad8(&mut out);
        slack_pad(&mut out, docs_w.into_bytes());
        slack_pad(&mut out, points);
        if multi {
            let mut offsets = vec![0u64; doc_count as usize + 1];
            for &(d, _, _) in pts {
                offsets[d as usize + 1] += 1;
            }
            for i in 1..offsets.len() {
                offsets[i] += offsets[i - 1];
            }
            packed::write_monotonic(&offsets, &mut out);
            pad8(&mut out);
            let mut w = BitWriter::with_bits(n * u64::from(pos_width));
            for p in &pos_of {
                w.push(*p, pos_width);
            }
            slack_pad(&mut out, w.into_bytes());
        } else {
            let mut w = BitWriter::with_bits(u64::from(doc_count) * u64::from(pos_width));
            let mut it = pts.iter().zip(&pos_of).peekable();
            for d in 0..doc_count {
                match it.peek() {
                    Some(&(&(pd, _, _), &p)) if pd == d => {
                        w.push(p, pos_width);
                        it.next();
                    }
                    _ => w.push(n, pos_width),
                }
            }
            slack_pad(&mut out, w.into_bytes());
        }
        Self::parse(Bytes::from_vec(out), doc_count)
    }

    /// The field's GEO section in `file`, if it has one.
    pub fn open(file: &SegmentFile, field: FieldId) -> io::Result<Option<Self>> {
        file.section(kind::GEO, field).map(|b| Self::parse(b, file.doc_count)).transpose()
    }

    fn parse(bytes: Bytes, segment_docs: u32) -> io::Result<Self> {
        let need = |at: usize, len: usize| -> io::Result<()> {
            if at.checked_add(len).is_some_and(|e| e <= bytes.len()) { Ok(()) } else { Err(invalid("GEO truncated")) }
        };
        need(0, 24)?;
        let (doc_count, n_points, n_nodes) = (bytes.u32_at(0), u64::from(bytes.u32_at(4)), bytes.u32_at(8) as usize);
        let (n_levels, multi, doc_width, pos_width) = (bytes[12] as usize, bytes[13] != 0, u32::from(bytes[14]), u32::from(bytes[15]));
        if doc_count != segment_docs || n_points == 0 || n_levels == 0 || doc_width > 32 || pos_width != packed::bits_for(n_points)
            || doc_width != packed::bits_for(u64::from(doc_count.saturating_sub(1)))
        {
            return Err(invalid("GEO header"));
        }
        let plen = bytes.u64_at(16) as usize;
        need(24, plen)?;
        let present = Bitmap::from_frozen(bytes.slice(24, plen))?;
        if present.maximum().is_some_and(|m| m >= doc_count) {
            return Err(invalid("GEO presence past the segment"));
        }
        let mut at = (24 + plen).next_multiple_of(8);
        need(at, 4 * n_levels)?;
        let level_end: Vec<u32> = (0..n_levels).map(|l| bytes.u32_at(at + 4 * l)).collect();
        let n_leaves = n_points.div_ceil(NODE) as usize;
        let mut count = n_leaves;
        let mut total = 0usize;
        for (l, &e) in level_end.iter().enumerate() {
            total += count;
            if e as usize != total || (l + 1 < n_levels) != (count > 1) {
                return Err(invalid("GEO levels"));
            }
            count = count.div_ceil(NODE as usize);
        }
        if total != n_nodes {
            return Err(invalid("GEO node count"));
        }
        at = (at + 4 * n_levels).next_multiple_of(8);
        need(at, 16 * n_nodes)?;
        let boxes = bytes.slice(at, 16 * n_nodes);
        at += 16 * n_nodes;
        need(at, 4 * (n_leaves + 1))?;
        let leaf_offset = bytes.slice(at, 4 * (n_leaves + 1));
        at = (at + 4 * (n_leaves + 1)).next_multiple_of(8);
        need(at, 2 * n_leaves)?;
        let leaf_width = bytes.slice(at, 2 * n_leaves);
        if leaf_width.iter().any(|&w| w > 32) {
            return Err(invalid("GEO leaf width"));
        }
        at = (at + 2 * n_leaves).next_multiple_of(8);
        let dlen = packed::packed_len(n_points, doc_width);
        need(at, dlen)?;
        let point_docs = bytes.slice(at, dlen);
        at = (at + dlen).next_multiple_of(8);
        let points_len = leaf_offset.u32_at(4 * n_leaves) as usize + 8;
        need(at, points_len)?;
        for leaf in 0..n_leaves {
            let (o, e) = (leaf_offset.u32_at(4 * leaf) as u64, leaf_offset.u32_at(4 * leaf + 4) as u64);
            let k = (n_points - leaf as u64 * NODE).min(NODE);
            let w = u64::from(leaf_width[2 * leaf]) + u64::from(leaf_width[2 * leaf + 1]);
            if e < o || (e - o) * 8 < k * w {
                return Err(invalid("GEO leaf offsets"));
            }
        }
        let points = bytes.slice(at, points_len);
        at = (at + points_len).next_multiple_of(8);
        let (offsets, n_pos) = if multi {
            need(at, 0)?;
            let (offsets, olen) = Monotonic::parse(bytes.slice(at, bytes.len() - at))?;
            if offsets.len() != doc_count as usize + 1 || offsets.get(0) != 0 || offsets.get(doc_count as usize) != n_points {
                return Err(invalid("GEO element offsets"));
            }
            at = (at + olen).next_multiple_of(8);
            (Some(offsets), n_points)
        } else {
            (None, u64::from(doc_count))
        };
        let plen = packed::packed_len(n_pos, pos_width);
        need(at, plen)?;
        let positions = bytes.slice(at, plen);
        Ok(Self {
            doc_count,
            n_points,
            multi,
            doc_width,
            pos_width,
            present,
            level_end,
            boxes,
            leaf_offset,
            leaf_width,
            point_docs,
            points,
            positions,
            offsets,
            bytes,
        })
    }

    pub fn add_sections(&self, field: FieldId, w: &mut SegmentWriter) {
        w.add_bytes(kind::GEO, field, self.bytes.clone());
    }

    /// The GEO section.
    pub(crate) fn section(&self) -> &Bytes {
        &self.bytes
    }

    /// Bytes of the section (heap before the segment is persisted, mapped after).
    pub fn section_bytes(&self) -> usize {
        self.bytes.len()
    }

    /// The documents with at least one point.
    #[inline]
    pub fn present_docs(&self) -> &Bitmap {
        &self.present
    }

    #[inline]
    fn box_at(&self, node: usize) -> [i32; 4] {
        let o = 16 * node;
        [
            self.boxes.u32_at(o) as i32,
            self.boxes.u32_at(o + 4) as i32,
            self.boxes.u32_at(o + 8) as i32,
            self.boxes.u32_at(o + 12) as i32,
        ]
    }

    /// The point at tree position `pos`.
    #[inline]
    fn point(&self, pos: u64) -> (i32, i32) {
        let leaf = (pos / NODE) as usize;
        let b = self.box_at(leaf);
        let (wl, wg) = (u32::from(self.leaf_width[2 * leaf]), u32::from(self.leaf_width[2 * leaf + 1]));
        let at = u64::from(self.leaf_offset.u32_at(4 * leaf)) * 8 + (pos % NODE) * u64::from(wl + wg);
        let la = packed::read(&self.points, at, wl) as i64;
        let ln = packed::read(&self.points, at + u64::from(wl), wg) as i64;
        ((i64::from(b[0]) + la) as i32, (i64::from(b[1]) + ln) as i32)
    }

    #[inline]
    fn doc_at(&self, pos: u64) -> DocId {
        packed::read(&self.point_docs, pos * u64::from(self.doc_width), self.doc_width) as DocId
    }

    /// Tree positions of a document's points.
    #[inline]
    fn positions(&self, doc: DocId) -> std::ops::Range<u64> {
        if doc >= self.doc_count {
            return 0..0;
        }
        match &self.offsets {
            Some(o) => o.get(doc as usize)..o.get(doc as usize + 1),
            None => {
                let p = packed::read(&self.positions, u64::from(doc) * u64::from(self.pos_width), self.pos_width);
                if p < self.n_points { 0..1 } else { 0..0 }
            }
        }
    }

    #[inline]
    fn position(&self, doc: DocId, i: u64) -> u64 {
        if self.multi {
            packed::read(&self.positions, i * u64::from(self.pos_width), self.pos_width)
        } else {
            packed::read(&self.positions, u64::from(doc) * u64::from(self.pos_width), self.pos_width)
        }
    }

    /// The truncated points of a document (one for a geopoint field, any number for geopoint[]).
    pub fn doc_points(&self, doc: DocId) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.positions(doc).map(move |i| self.position(doc, i)).filter(|&p| p < self.n_points).map(|p| self.point(p))
    }

    /// Typesense's sort distance of a document from a truncated centre, in whole metres: its
    /// closest point, or `MISSING_M` without one.
    #[inline]
    pub fn distance_m(&self, doc: DocId, centre: (i32, i32)) -> u32 {
        self.doc_points(doc).map(|p| sort_metres(p, centre)).min().unwrap_or(MISSING_M)
    }

    /// Documents with a point inside the cap.
    pub fn radius(&self, cap: &Cap) -> Bitmap {
        self.search(cap)
    }

    /// Documents with a point inside the box.
    pub fn in_box(&self, b: &GeoBox) -> Bitmap {
        self.search(b)
    }

    /// Whether one of the document's points is inside the cap.
    pub fn doc_in_cap(&self, doc: DocId, cap: &Cap) -> bool {
        self.doc_points(doc).any(|(la, ln)| cap.contains(la, ln))
    }

    /// Whether one of the document's points is inside the box.
    pub fn doc_in_box(&self, doc: DocId, b: &GeoBox) -> bool {
        self.doc_points(doc).any(|(la, ln)| b.contains(la, ln))
    }

    /// Documents with a point inside the polygon.
    pub fn in_polygon(&self, p: &Polygon) -> Bitmap {
        self.search(p)
    }

    /// Whether one of the document's points is inside the polygon.
    pub fn doc_in_polygon(&self, doc: DocId, p: &Polygon) -> bool {
        self.doc_points(doc).any(|(la, ln)| p.contains(la, ln))
    }

    fn level_start(&self, level: usize) -> usize {
        if level == 0 { 0 } else { self.level_end[level - 1] as usize }
    }

    fn search(&self, region: &impl Region) -> Bitmap {
        let mut out = Gather::new(self.doc_count);
        let root = self.level_end.len() - 1;
        let mut stack = vec![(root, 0usize)];
        while let Some((level, j)) = stack.pop() {
            match region.relate(self.box_at(self.level_start(level) + j)) {
                Relation::Disjoint => {}
                Relation::Within => {
                    let span = NODE.pow(level as u32 + 1);
                    let (from, to) = (j as u64 * span, ((j as u64 + 1) * span).min(self.n_points));
                    for p in from..to {
                        self.gather(&mut out, p);
                    }
                }
                Relation::Crosses if level == 0 => {
                    for p in j as u64 * NODE..((j as u64 + 1) * NODE).min(self.n_points) {
                        let (la, ln) = self.point(p);
                        if region.contains(la, ln) {
                            self.gather(&mut out, p);
                        }
                    }
                }
                Relation::Crosses => {
                    let below = self.level_end[level - 1] as usize - self.level_start(level - 1);
                    for c in j * NODE as usize..((j + 1) * NODE as usize).min(below) {
                        stack.push((level - 1, c));
                    }
                }
            }
        }
        out.finish()
    }

    #[inline]
    fn gather(&self, out: &mut Gather, pos: u64) {
        let d = self.doc_at(pos);
        if d < self.doc_count {
            out.add(d);
        }
    }

    /// The first `needed` candidates by Typesense's distance from a truncated centre, nearest
    /// first, ties by highest doc id: (metres, doc). Candidates without a point follow at
    /// `MISSING_M`. Best-first over the tree, so only the boxes near the centre are opened.
    pub fn nearest(&self, centre: (i32, i32), candidates: &Bitmap, needed: usize) -> Vec<(u32, DocId)> {
        if needed == 0 {
            return Vec::new();
        }
        let (clat, clng) = (deg(centre.0), deg(centre.1));
        let to_m = |a: f64| a * SORT_RADIUS_M;
        let mut heap = BinaryHeap::new();
        let root = self.level_end.len() - 1;
        heap.push(Reverse(Entry(0.0, root as u8, 0)));
        let mut seen = if self.multi { Some(Bitmap::new()) } else { None };
        let mut found: Vec<(u32, DocId)> = Vec::new();
        let mut kth = u32::MAX;
        while let Some(Reverse(Entry(key, level, j))) = heap.pop() {
            if found.len() >= needed && to_m(key) * (1.0 - 1e-9) - 1e-6 >= f64::from(kth) + 1.0 {
                break;
            }
            if level == POINT {
                let d = self.doc_at(j);
                if d >= self.doc_count || !candidates.contains(d) {
                    continue;
                }
                if let Some(s) = &mut seen {
                    if s.contains(d) {
                        continue;
                    }
                    s.add(d);
                }
                // Points leave the heap nearest first, so once `needed` are found the largest of
                // them bounds the page; ties at that distance are still collected.
                let m = sort_metres(self.point(j), centre);
                found.push((m, d));
                if found.len() == needed {
                    kth = found.iter().map(|f| f.0).max().unwrap_or(0);
                }
                continue;
            }
            let level = level as usize;
            if level == 0 {
                for p in j * NODE..((j + 1) * NODE).min(self.n_points) {
                    let (la, ln) = self.point(p);
                    heap.push(Reverse(Entry(angle(deg(la), deg(ln), clat, clng), POINT, p)));
                }
                continue;
            }
            let below_start = self.level_start(level - 1);
            let below = self.level_end[level - 1] as usize - below_start;
            for c in j as usize * NODE as usize..((j as usize + 1) * NODE as usize).min(below) {
                let lb = box_min_angle(clat, clng, self.box_at(below_start + c));
                heap.push(Reverse(Entry((lb * (1.0 - 1e-9) - 1e-15).max(0.0), (level - 1) as u8, c as u64)));
            }
        }
        found.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        found.truncate(needed);
        if found.len() < needed {
            let missing = candidates - &self.present;
            let mut rest: Vec<DocId> = missing.iter().collect();
            rest.reverse();
            found.extend(rest.into_iter().take(needed - found.len()).map(|d| (MISSING_M, d)));
        }
        found
    }

    pub fn n_points(&self) -> u64 {
        self.n_points
    }
}

impl std::fmt::Debug for GeoIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GeoIndex({} points, {} docs)", self.n_points, self.doc_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(points: &[(DocId, f64, f64)], docs: u32, multi: bool) -> GeoIndex {
        let pts: Vec<(DocId, i32, i32)> = points.iter().map(|&(d, la, ln)| (d, e6(la), e6(ln))).collect();
        GeoIndex::build(&pts, docs, multi).unwrap()
    }

    #[test]
    fn truncates_like_typesense() {
        assert_eq!(e6(64.75298), 64_752_979);
        assert_eq!(e6(-12.5), -12_500_000);
        assert_eq!(deg(e6(20.95547)), 20.95547);
    }

    #[test]
    fn typesense_distances() {
        // geo_distance_meters returned by a running Typesense for a document at (64.75298, 20.95547).
        let p = sort_centre(64.75298, 20.95547);
        for (lat, lng, m) in [
            (64.74832372104774, 20.958301833427807, 534),
            (71.05163169826919, 26.95688544177177, 742996),
            (70.75298, 20.95547, 667026),
            (58.752979999999994, 20.95547, 667025),
            (64.75298, 32.95547, 568151),
            (0.5, 80.25, 8555418),
            (-5.5, 120.75, 11021923),
            (-40.5, 170.25, 16678776),
            (64.72796945727094, 20.971857795995003, 2887),
        ] {
            assert_eq!(sort_metres(p, sort_centre(lat, lng)), m, "{lat},{lng}");
        }
    }

    #[test]
    fn typesense_radius_filters() {
        // A running Typesense found 0 / 0 / 1 for these radii around a centre north of the point.
        let p = (e6(64.75298), e6(20.95547));
        for (r_km, inside) in [(5.0003266f32, false), (5.0008109, false), (5.0014109, true)] {
            let cap = Cap::new(64.797955, 20.95547, f64::from(r_km) * 1000.0);
            assert_eq!(cap.contains(p.0, p.1), inside, "{r_km}");
        }
    }

    #[test]
    fn hilbert_is_injective_on_a_grid() {
        let mut seen = std::collections::HashSet::new();
        for x in 0..64u32 {
            for y in 0..64u32 {
                assert!(seen.insert(hilbert(x * 1024, y * 1024)));
            }
        }
        assert_eq!(hilbert(0, 0), 0);
    }

    fn brute_radius(points: &[(DocId, f64, f64)], cap: &Cap) -> Vec<DocId> {
        let mut v: Vec<DocId> = points.iter().filter(|&&(_, la, ln)| cap.contains(e6(la), e6(ln))).map(|p| p.0).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    fn random_points(n: usize, seed: u64, per_doc: usize) -> Vec<(DocId, f64, f64)> {
        let mut s = seed;
        let mut next = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut out = Vec::new();
        for d in 0..n as u32 {
            for _ in 0..per_doc {
                if next() < 0.8 {
                    out.push((d, 55.0 + next() * 14.0, 11.0 + next() * 13.0));
                }
            }
        }
        out
    }

    #[test]
    fn radius_matches_brute_force() {
        for (per_doc, multi) in [(1, false), (3, true)] {
            let points = random_points(5000, 3 + per_doc as u64, per_doc);
            let idx = index(&points, 5000, multi);
            for (lat, lng, r) in [(59.33, 18.07, 5_000.0), (57.7, 11.97, 50_000.0), (62.0, 17.0, 300_000.0), (60.0, 15.0, 2_000_000.0), (55.6, 13.0, 1.0)] {
                let cap = Cap::new(lat, lng, r);
                assert_eq!(idx.radius(&cap).to_vec(), brute_radius(&points, &cap), "{lat},{lng},{r}");
            }
        }
    }

    #[test]
    fn box_matches_brute_force() {
        let points = random_points(3000, 9, 2);
        let idx = index(&points, 3000, true);
        let b = GeoBox { min_lat: 58.0, min_lng: 12.0, max_lat: 60.5, max_lng: 16.25 };
        let mut want: Vec<DocId> = points.iter().filter(|&&(_, la, ln)| b.contains(e6(la), e6(ln))).map(|p| p.0).collect();
        want.dedup();
        assert_eq!(idx.in_box(&b).to_vec(), want);
    }

    #[test]
    fn nearest_matches_a_full_sort() {
        for (per_doc, multi) in [(1, false), (3, true)] {
            let points = random_points(4000, 21 + per_doc as u64, per_doc);
            let idx = index(&points, 4000, multi);
            let centre = sort_centre(59.0, 16.0);
            let mut cand = Bitmap::new();
            cand.add_range(0..4000);
            let odd: Vec<u32> = (0..4000).filter(|d| d % 3 != 0).collect();
            for candidates in [cand, Bitmap::of(&odd)] {
                let mut all: Vec<(u32, DocId)> = candidates.iter().map(|d| (idx.distance_m(d, centre), d)).collect();
                all.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
                for needed in [1, 10, 250, 3990] {
                    let want: Vec<_> = all.iter().take(needed).copied().collect();
                    assert_eq!(idx.nearest(centre, &candidates, needed), want, "needed {needed}");
                }
            }
        }
    }

    #[test]
    fn missing_documents_are_last() {
        let idx = index(&[(0, 59.0, 18.0), (2, 59.1, 18.0)], 4, false);
        let mut c = Bitmap::new();
        c.add_range(0..4);
        let got = idx.nearest(sort_centre(59.0, 18.0), &c, 4);
        assert_eq!(got.iter().map(|g| g.1).collect::<Vec<_>>(), vec![0, 2, 3, 1]);
        assert_eq!(got[2].0, MISSING_M);
        assert_eq!(idx.distance_m(1, sort_centre(59.0, 18.0)), MISSING_M);
    }

    #[test]
    fn multi_uses_the_closest_point() {
        let idx = index(&[(0, 59.34922, 18.04626), (0, 58.41043, 15.62114), (1, 60.0, 15.0)], 2, true);
        let centre = sort_centre(58.41143, 15.62114);
        assert_eq!(idx.distance_m(0, centre), sort_metres((e6(58.41043), e6(15.62114)), centre));
        assert_eq!(idx.doc_points(0).count(), 2);
    }

    #[test]
    fn round_trips_through_a_segment() {
        let points = random_points(700, 5, 2);
        let idx = index(&points, 700, true);
        let mut w = SegmentWriter::new();
        idx.add_sections(4, &mut w);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("segment.tqbf");
        w.write(&path, 700, 0).unwrap();
        {
            let file = SegmentFile::open(&path).unwrap();
            let back = GeoIndex::open(&file, 4).unwrap().unwrap();
            for d in 0..700 {
                assert_eq!(back.doc_points(d).collect::<Vec<_>>(), idx.doc_points(d).collect::<Vec<_>>());
            }
            let cap = Cap::new(60.0, 17.0, 120_000.0);
            assert_eq!(back.radius(&cap).to_vec(), idx.radius(&cap).to_vec());
        }
    }
}
