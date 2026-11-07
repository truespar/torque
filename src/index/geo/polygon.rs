//! Geo polygon filters as Typesense runs them through S2 (an `S2Loop`), exact on the truncated
//! points:
//!
//! - The edges are geodesics (great-circle arcs) between the vertices, which are used at full
//!   precision; a document's point is its 1e-6-degree truncation, as for every geo filter.
//! - The polygon is the smaller of the two regions its loop bounds, whichever way the vertices run
//!   (`S2Loop::Normalize`), so one around a pole or across the antimeridian needs nothing special.
//! - S2's validation, with its messages: at least 3 vertices, none repeated, no antipodal
//!   neighbours, no edges crossing. A last vertex equal to the first (a closed ring) is dropped,
//!   and a latitude outside [-90, 90] is refused (S2 would fold it over the pole).
//! - A point on the boundary is in exactly one of two polygons that share the edge or the vertex
//!   (S2's semi-open model): the point is moved an infinitesimal step in a fixed direction
//!   (symbolic perturbation), and every orientation test is exactly antisymmetric.
//!
//! A query builds a grid over the polygon's latitude/longitude bound, about sixteen cells per edge.
//! Each cell has a reference point, knows whether that point is inside, and lists the edges that
//! come near the cell. A point in a cell without edges takes the cell's answer; any other counts
//! the listed edges that the arc from the reference point crosses. Two points of a cell are joined
//! by an arc that stays in the cell but for a geodesic's bulge toward the pole, which the lists
//! allow for. The R-tree prunes on the same grid through summed-area tables: a node box whose
//! cells list no edge is wholly inside or wholly outside.

use super::{deg, unit, Region, Relation, RAD};
use std::f64::consts::FRAC_PI_2;

type V3 = [f64; 3];

/// The direction of the symbolic perturbation (one no edge's plane holds).
const NUDGE: V3 = [0.271_828_182_845_904_5, 0.314_159_265_358_979_3, 0.577_215_664_901_532_9];
/// Degrees allowed for rounding where a point falls on the grid and where an edge passes.
const SLACK: f64 = 1e-8;
/// Degrees between the polygon's bound and its grid's.
const MARGIN: f64 = 1e-7;
/// A cell's reference point, as fractions of its rows and columns (off the centre, so that an
/// edge on round coordinates does not pass through it).
const REF: (f64, f64) = (0.500_123_456_789, 0.499_876_543_21);

#[inline]
fn dot(a: &V3, b: &V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn cross(a: &V3, b: &V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[inline]
fn add(a: &V3, b: &V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
fn sub(a: &V3, b: &V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn normalize(a: &V3) -> V3 {
    let l = dot(a, a).sqrt();
    [a[0] / l, a[1] / l, a[2] / l]
}

/// `2 a × b` as `(a + b) × (b − a)`: accurate for close points (S2's `RobustCrossProd`), and
/// exactly the negation of `normal(b, a)`.
#[inline]
fn normal(a: &V3, b: &V3) -> V3 {
    cross(&add(a, b), &sub(b, a))
}

/// Latitude and longitude (degrees) of a unit vector.
fn lat_lng(p: &V3) -> (f64, f64) {
    (p[2].atan2(p[0].hypot(p[1])) / RAD, p[1].atan2(p[0]) / RAD)
}

/// A longitude in [-180, 180).
fn wrap(lng: f64) -> f64 {
    (lng + 180.0).rem_euclid(360.0) - 180.0
}

/// Whether two vertices are one point (a longitude 360 degrees on, any longitude at a pole).
fn same(a: (f64, f64), b: (f64, f64)) -> bool {
    a.0 == b.0 && (a.0.abs() == 90.0 || (a.1 - b.1).rem_euclid(360.0) == 0.0)
}

#[derive(Clone)]
struct Edge {
    a: V3,
    b: V3,
    /// `normal(a, b)`: the edge's left is where `n · x > 0`.
    n: V3,
}

impl Edge {
    fn new(a: V3, b: V3) -> Self {
        Self { n: normal(&a, &b), a, b }
    }

    fn chord2(&self) -> f64 {
        let d = sub(&self.b, &self.a);
        dot(&d, &d)
    }
}

/// Whether the perturbed point `p` is left of the edge (`det(a, b, p) > 0`).
#[inline]
fn left_of(e: &Edge, p: &V3) -> bool {
    if e.a != *p && e.b != *p {
        let s = dot(&e.n, p);
        if s != 0.0 {
            return s > 0.0;
        }
    }
    dot(&e.n, &NUDGE) > 0.0
}

/// Whether `x` is left of the arc from `c` to the perturbed point `p` (`det(c, p, x) > 0`), `m`
/// being `normal(c, p)`.
#[inline]
fn left_of_arc(c: &V3, p: &V3, m: &V3, x: &V3) -> bool {
    if x != p {
        let s = dot(m, x);
        if s != 0.0 {
            return s > 0.0;
        }
    }
    dot(&cross(c, &NUDGE), x) > 0.0
}

/// Whether the arc from `c` to the perturbed point `p` crosses the edge (S2's `CrossingSign`: the
/// arc's ends lie on either side of the edge and the edge's on either side of the arc, in the
/// orientation that puts the crossing on both rather than on their antipodes).
#[inline]
fn crosses(c: &V3, p: &V3, m: &V3, e: &Edge) -> bool {
    let p_left = left_of(e, p);
    if left_of(e, c) == p_left {
        return false;
    }
    let a_left = left_of_arc(c, p, m, &e.a);
    a_left != left_of_arc(c, p, m, &e.b) && a_left == p_left
}

/// Whether two edges cross at a point inside both (unperturbed: touching is not crossing).
fn edges_cross(e: &Edge, f: &Edge) -> bool {
    let opposite = |x: f64, y: f64| (x > 0.0 && y < 0.0) || (x < 0.0 && y > 0.0);
    let (s1, s2) = (dot(&e.n, &f.a), dot(&e.n, &f.b));
    let (t1, t2) = (dot(&f.n, &e.a), dot(&f.n, &e.b));
    opposite(s1, s2) && opposite(t1, t2) && (s2 > 0.0) == (t1 > 0.0)
}

/// Whether the arc from `c` to the perturbed point `p` crosses an odd number of the edges.
fn parity<'a>(edges: impl IntoIterator<Item = &'a Edge>, c: &V3, p: &V3) -> bool {
    let m = normal(c, p);
    edges.into_iter().fold(false, |odd, e| odd ^ crosses(c, p, &m, e))
}

/// Whether `to` is inside, given whether `from` is; far apart, through a point a quarter circle
/// from both (an arc between near-antipodal points is ill-defined).
fn reach(edges: &[Edge], from: &V3, inside: bool, to: &V3) -> bool {
    if dot(from, to) > -0.5 {
        return inside ^ parity(edges, from, to);
    }
    let x = cross(from, to);
    let l = 1e-3 * dot(&x, &x).sqrt();
    let via = if l > 0.0 { normalize(&[x[0] + l * NUDGE[0], x[1] + l * NUDGE[1], x[2] + l * NUDGE[2]]) } else { normalize(&NUDGE) };
    inside ^ parity(edges, from, &via) ^ parity(edges, &via, to)
}

/// Latitude (degrees) where a great circle, given by its normal, crosses a longitude.
fn lat_at(n: &V3, lng: f64) -> f64 {
    let (s, c) = (lng * RAD).sin_cos();
    let x = -(n[0] * c + n[1] * s);
    (if n[2] >= 0.0 { x.atan2(n[2]) } else { (-x).atan2(-n[2]) }) / RAD
}

/// Where an edge lies in latitude and longitude.
struct Span {
    /// Longitude of its western end (degrees) and how far east it runs (under 180 degrees).
    west: f64,
    width: f64,
    /// Latitudes of its western and eastern ends.
    lat_w: f64,
    lat_e: f64,
    lo: f64,
    hi: f64,
    /// Its northmost and southmost points between the ends: (degrees east of `west`, latitude).
    north: Option<(f64, f64)>,
    south: Option<(f64, f64)>,
    n: V3,
    /// From a pole or over one: not one run of longitudes.
    polar: bool,
    /// Within 1e-6 radians of a meridian's plane, where the latitude at a longitude is poorly
    /// conditioned: any of its longitudes may hold any of its latitudes.
    steep: bool,
}

impl Span {
    fn new(a: (f64, f64), b: (f64, f64), n: V3) -> Self {
        let (mut lo, mut hi) = (a.0.min(b.0), a.0.max(b.0));
        let d = (b.1 - a.1).rem_euclid(360.0);
        let at_pole = a.0.abs() == 90.0 || b.0.abs() == 90.0;
        if at_pole || d == 180.0 {
            if !at_pole {
                if a.0 + b.0 > 0.0 {
                    hi = 90.0;
                } else {
                    lo = -90.0;
                }
            }
            return Self { west: wrap(a.1), width: 0.0, lat_w: a.0, lat_e: b.0, lo, hi, north: None, south: None, n, polar: true, steep: true };
        }
        let (west, width, lat_w, lat_e) = if d < 180.0 { (wrap(a.1), d, a.0, b.0) } else { (wrap(b.1), 360.0 - d, b.0, a.0) };
        let h = n[0].hypot(n[1]);
        let (mut north, mut south) = (None, None);
        if width > 0.0 {
            // the great circle's northmost point, and opposite it the southmost
            let top = h.atan2(n[2].abs()) / RAD;
            let lng = (if n[2] > 0.0 { (-n[1]).atan2(-n[0]) } else { n[1].atan2(n[0]) }) / RAD;
            let offset = |l: f64| Some((l - west).rem_euclid(360.0)).filter(|&t| t <= width);
            north = offset(lng).map(|t| (t, top));
            south = offset(lng + 180.0).map(|t| (t, -top));
            hi = hi.max(north.map_or(hi, |p| p.1));
            lo = lo.min(south.map_or(lo, |p| p.1));
        }
        Self { west, width, lat_w, lat_e, lo, hi, north, south, n, polar: false, steep: n[2].abs() < 1e-6 * h }
    }

    /// The edge's latitudes between `t0` and `t1` degrees east of its western end.
    fn lat_range(&self, t0: f64, t1: f64) -> (f64, f64) {
        if self.steep {
            return (self.lo, self.hi);
        }
        let at = |t: f64| {
            if t <= 0.0 {
                self.lat_w
            } else if t >= self.width {
                self.lat_e
            } else {
                lat_at(&self.n, self.west + t)
            }
        };
        let (a, b) = (at(t0), at(t1));
        let (mut lo, mut hi) = (a.min(b), a.max(b));
        if let Some((_, lat)) = self.north.filter(|p| p.0 >= t0 && p.0 <= t1) {
            hi = hi.max(lat);
        }
        if let Some((_, lat)) = self.south.filter(|p| p.0 >= t0 && p.0 <= t1) {
            lo = lo.min(lat);
        }
        (lo, hi)
    }
}

/// The longitudes the edges cover, which are the polygon's when it holds no pole: from the end of
/// the largest gap between them eastward, with a margin; every longitude when there is no gap.
fn longitudes(spans: &[Span]) -> (f64, f64) {
    let mut arcs: Vec<(f64, f64)> = spans.iter().map(|s| (s.west, s.west + s.width)).collect();
    arcs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (first, mut reach) = arcs[0];
    let (mut gap, mut west) = (0.0, first);
    for &(s, e) in &arcs[1..] {
        if s - reach > gap {
            (gap, west) = (s - reach, s);
        }
        reach = reach.max(e);
    }
    // and the gap after the last arc, around the globe to the first
    if first + 360.0 - reach > gap {
        (gap, west) = (first + 360.0 - reach, first);
    }
    if gap <= 2.0 * MARGIN {
        return (-180.0, 360.0);
    }
    (west - MARGIN, 360.0 - gap + 2.0 * MARGIN)
}

/// The grid's placement: `rows` of latitude from `lat0` to `lat1`, `cols` of longitude from
/// `lng0` east over `width` degrees (360: every longitude).
#[derive(Clone, Copy)]
struct Frame {
    lat0: f64,
    lat1: f64,
    dlat: f64,
    rows: usize,
    lng0: f64,
    width: f64,
    dlng: f64,
    cols: usize,
}

impl Frame {
    /// Degrees east of `lng0`, in [0, 360].
    #[inline]
    fn u(&self, lng: f64) -> f64 {
        (lng - self.lng0).rem_euclid(360.0)
    }

    #[inline]
    fn row(&self, lat: f64) -> usize {
        (((lat - self.lat0) / self.dlat) as usize).min(self.rows - 1)
    }

    #[inline]
    fn col(&self, u: f64) -> usize {
        ((u / self.dlng) as usize).min(self.cols - 1)
    }

    /// The cell of a point, none outside the grid.
    #[inline]
    fn cell(&self, lat: f64, lng: f64) -> Option<usize> {
        let u = self.u(lng);
        (lat >= self.lat0 && lat <= self.lat1 && u <= self.width).then(|| self.row(lat) * self.cols + self.col(u))
    }

    /// The parts of `[a, b]` (grid longitudes, `a` in [-SLACK, 360)) on the grid, each with what
    /// to add to get back to `[a, b]`: on a grid around the globe it wraps.
    fn pieces(&self, a: f64, b: f64) -> impl Iterator<Item = (f64, f64, f64)> {
        let mut out = [(1.0, 0.0, 0.0); 3];
        if self.width < 360.0 {
            out[0] = (a.max(0.0), b.min(self.width), 0.0);
        } else {
            out[0] = (a.max(0.0), b.min(360.0), 0.0);
            if a < 0.0 {
                out[1] = (a + 360.0, 360.0, -360.0);
            }
            if b > 360.0 {
                out[2] = (0.0, b - 360.0, 360.0);
            }
        }
        out.into_iter().filter(|p| p.0 <= p.1)
    }
}

/// The sum of a summed-area table over rows `r0..=r1` and columns `c0..=c1`.
#[inline]
fn area(t: &[u32], w: usize, r0: usize, r1: usize, c0: usize, c1: usize) -> u32 {
    t[(r1 + 1) * w + c1 + 1].wrapping_sub(t[r0 * w + c1 + 1]).wrapping_sub(t[(r1 + 1) * w + c0]).wrapping_add(t[r0 * w + c0])
}

#[derive(Clone)]
struct Grid {
    frame: Frame,
    /// Per cell: its reference point, whether that is inside, its edges (`lists[start[c]..start[c + 1]]`).
    refs: Vec<V3>,
    inside: Vec<bool>,
    start: Vec<u32>,
    lists: Vec<u32>,
    /// Summed-area tables of the cells with edges and of those without whose reference is inside.
    busy: Vec<u32>,
    full: Vec<u32>,
}

impl Grid {
    fn relate(&self, b: [i32; 4]) -> Relation {
        let f = &self.frame;
        let (lat_lo, lng_lo, lat_hi, lng_hi) = (deg(b[0]), deg(b[1]), deg(b[2]), deg(b[3]));
        if lat_hi < f.lat0 || lat_lo > f.lat1 {
            return Relation::Disjoint;
        }
        // points of the box off the grid are outside
        let mut partial = lat_lo < f.lat0 || lat_hi > f.lat1;
        let (r0, r1) = (f.row(lat_lo.max(f.lat0)), f.row(lat_hi.min(f.lat1)));
        let (a, z) = (f.u(lng_lo), f.u(lng_hi));
        let runs = if lng_hi - lng_lo >= 360.0 {
            [(0.0, 360.0), (1.0, 0.0)]
        } else if a <= z {
            [(a, z), (1.0, 0.0)]
        } else {
            [(a, 360.0), (0.0, z)]
        };
        let (mut busy, mut full, mut cells) = (0u32, 0u32, 0u32);
        for (a, z) in runs.into_iter().filter(|r| r.0 <= r.1) {
            if a > f.width {
                partial = true;
                continue;
            }
            partial |= z > f.width;
            let (c0, c1) = (f.col(a), f.col(z.min(f.width)));
            busy += area(&self.busy, f.cols + 1, r0, r1, c0, c1);
            full += area(&self.full, f.cols + 1, r0, r1, c0, c1);
            cells += ((r1 - r0 + 1) * (c1 - c0 + 1)) as u32;
        }
        if busy > 0 {
            Relation::Crosses
        } else if full == 0 {
            Relation::Disjoint
        } else if full == cells && !partial {
            Relation::Within
        } else {
            Relation::Crosses
        }
    }
}

/// A geo polygon filter, ready to test points.
#[derive(Clone)]
pub struct Polygon {
    /// The vertices (latitude, longitude in degrees), as given less a closing repeat.
    vertices: Vec<(f64, f64)>,
    edges: Vec<Edge>,
    grid: Grid,
}

impl Polygon {
    /// The polygon of these vertices (finite degrees), or S2's complaint as Typesense reports it.
    pub fn new(vertices: &[(f64, f64)]) -> Result<Self, String> {
        let invalid = |m: String| Err(format!("Polygon is invalid: {m}"));
        let mut vertices = vertices.to_vec();
        if vertices.len() > 1 && same(vertices[0], vertices[vertices.len() - 1]) {
            vertices.pop();
        }
        if let Some(i) = vertices.iter().position(|v| !(-90.0..=90.0).contains(&v.0)) {
            return invalid(format!("Vertex {i} has a latitude outside [-90, 90]"));
        }
        let n = vertices.len();
        if n < 3 {
            return invalid("Non-empty, non-full loops must have at least 3 vertices".into());
        }
        let points: Vec<V3> = vertices.iter().map(|&(la, ln)| unit(la, ln)).collect();
        for i in 0..n {
            let j = (i + 1) % n;
            if same(vertices[i], vertices[j]) || points[i] == points[j] {
                return invalid(format!("Edge {i} is degenerate (duplicate vertex)"));
            }
            let s = add(&points[i], &points[j]);
            if dot(&s, &s) < 1e-20 {
                return invalid(format!("Vertices {i} and {j} are antipodal"));
            }
        }
        let mut keys: Vec<(f64, f64, usize)> = vertices
            .iter()
            .enumerate()
            .map(|(i, &(la, ln))| (la + 0.0, if la.abs() == 90.0 { 0.0 } else { (ln + 180.0).rem_euclid(360.0) + 0.0 }, i))
            .collect();
        keys.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
        if let Some((i, j)) = keys.windows(2).filter(|w| w[0].0 == w[1].0 && w[0].1 == w[1].1).map(|w| (w[0].2, w[1].2)).min() {
            return invalid(format!("Edge {i} has duplicate vertex with edge {j}"));
        }
        let edges: Vec<Edge> = (0..n).map(|i| Edge::new(points[i], points[(i + 1) % n])).collect();

        // The loop's left is the inside unless that is more than half the sphere: its area is 2π
        // less the loop's turning (Gauss-Bonnet).
        let turning: f64 = (0..n)
            .map(|i| {
                let (p, q) = (&edges[(i + n - 1) % n].n, &edges[i].n);
                dot(&cross(p, q), &points[i]).atan2(dot(p, q))
            })
            .sum();
        let flip = turning < 0.0;

        // A point beside the middle of the longest edge: on the edge's left, then across whatever
        // edges lie between.
        let k = (0..n).max_by(|&i, &j| edges[i].chord2().total_cmp(&edges[j].chord2())).unwrap_or(0);
        let e = &edges[k];
        let (mid, side, step) = (normalize(&add(&e.a, &e.b)), normalize(&e.n), 1e-3 * e.chord2().sqrt());
        let q = normalize(&[mid[0] + step * side[0], mid[1] + step * side[1], mid[2] + step * side[2]]);
        let crossed = parity(edges.iter().enumerate().filter(|&(i, _)| i != k).map(|(_, e)| e), &mid, &q);
        let inside_q = !crossed != flip;

        // The bound: the edges' latitudes and longitudes, to a pole and around it when the polygon
        // holds it (on its boundary too).
        let spans: Vec<Span> = (0..n).map(|i| Span::new(vertices[i], vertices[(i + 1) % n], edges[i].n)).collect();
        let pole = |z: f64| {
            spans.iter().any(|s| s.polar && if z > 0.0 { s.hi == 90.0 } else { s.lo == -90.0 })
                || reach(&edges, &q, inside_q, &[0.0, 0.0, z])
        };
        let (north, south) = (pole(1.0), pole(-1.0));
        let lat0 = if south { -90.0 } else { (spans.iter().map(|s| s.lo).fold(90.0, f64::min) - MARGIN).max(-90.0) };
        let lat1 = if north { 90.0 } else { (spans.iter().map(|s| s.hi).fold(-90.0, f64::max) + MARGIN).min(90.0) };
        let (lng0, width) = if north || south { (-180.0, 360.0) } else { longitudes(&spans) };

        // About square cells on the ground, under 90 degrees of longitude wide (a cell is then
        // convex but for the bulge of a geodesic toward the pole).
        let cells = (16 * n).clamp(1024, 16384);
        let height = lat1 - lat0;
        let ground = width * (0.5 * (lat0 + lat1) * RAD).cos().max(0.05);
        let min_cols = ((width / 90.0).ceil() as usize).max(3);
        let cols = ((cells as f64 * ground / height).sqrt().round() as usize).clamp(min_cols, cells);
        let rows = (cells / cols).max(1);
        let f = Frame { lat0, lat1, dlat: height / rows as f64, rows, lng0, width, dlng: width / cols as f64, cols };
        let half = 0.5 * f.dlng * RAD;
        let bulge = (0..=rows)
            .map(|r| {
                let phi = (lat0 + r as f64 * f.dlat).abs().min(90.0) * RAD;
                if phi > 0.0 && phi < FRAC_PI_2 { (phi.tan() / half.cos()).atan() - phi } else { 0.0 }
            })
            .fold(0.0, f64::max)
            / RAD
            + SLACK;

        // Each edge in the cells it passes near: per column, its latitudes there and the bulge.
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        for (i, s) in spans.iter().enumerate() {
            let mut put = |c: usize, lo: f64, hi: f64| {
                let (r0, r1) = (f.row(lo - bulge), f.row(hi + bulge));
                pairs.extend((r0..=r1).map(|r| ((r * cols + c) as u32, i as u32)));
            };
            if s.polar {
                (0..cols).for_each(|c| put(c, s.lo, s.hi));
                continue;
            }
            let start = f.u(s.west);
            for (from, to, shift) in f.pieces(start - SLACK, start + s.width + SLACK) {
                for c in f.col(from)..=f.col(to) {
                    let t0 = ((c as f64 * f.dlng - SLACK).max(from) + shift - start).clamp(0.0, s.width);
                    let t1 = (((c + 1) as f64 * f.dlng + SLACK).min(to) + shift - start).clamp(0.0, s.width);
                    let (lo, hi) = s.lat_range(t0, t1);
                    put(c, lo, hi);
                }
            }
        }
        pairs.sort_unstable();
        pairs.dedup();
        let ncell = rows * cols;
        let mut start = vec![0u32; ncell + 1];
        for &(c, _) in &pairs {
            start[c as usize + 1] += 1;
        }
        for c in 0..ncell {
            start[c + 1] += start[c];
        }
        let lists: Vec<u32> = pairs.into_iter().map(|p| p.1).collect();
        let list = |c: usize| &lists[start[c] as usize..start[c + 1] as usize];

        // Two crossing edges share the cell of their crossing.
        let mut first: Option<(usize, usize)> = None;
        for cell in 0..ncell {
            let l = list(cell);
            for (x, &i) in l.iter().enumerate() {
                for &j in &l[x + 1..] {
                    let (i, j) = (i as usize, j as usize);
                    if j == i + 1 || (i == 0 && j == n - 1) || first.is_some_and(|f| f <= (i, j)) {
                        continue;
                    }
                    if edges_cross(&edges[i], &edges[j]) {
                        first = Some((i, j));
                    }
                }
            }
        }
        if let Some((i, j)) = first {
            return invalid(format!("Edge {i} crosses edge {j}"));
        }

        // The reference points' answers: one from the point beside the longest edge, the rest a
        // neighbour's, counting the edges of the two cells (row by row from that cell's column).
        let refs: Vec<V3> = (0..ncell)
            .map(|c| unit(lat0 + ((c / cols) as f64 + REF.0) * f.dlat, lng0 + ((c % cols) as f64 + REF.1) * f.dlng))
            .collect();
        let (qlat, qlng) = lat_lng(&q);
        let u = f.u(qlng);
        let qcol = if u <= width { f.col(u) } else if 360.0 - u < u - width { 0 } else { cols - 1 };
        let anchor = f.row(qlat) * cols + qcol;
        let mut inside = vec![false; ncell];
        inside[anchor] = reach(&edges, &q, inside_q, &refs[anchor]);
        let (ar, ac) = (anchor / cols, anchor % cols);
        let mut order = Vec::with_capacity(ncell);
        let along = |r: usize, order: &mut Vec<(usize, usize)>| {
            order.extend((ac + 1..cols).map(|c| (r * cols + c - 1, r * cols + c)));
            order.extend((0..ac).rev().map(|c| (r * cols + c + 1, r * cols + c)));
        };
        along(ar, &mut order);
        for r in ar + 1..rows {
            order.push(((r - 1) * cols + ac, r * cols + ac));
            along(r, &mut order);
        }
        for r in (0..ar).rev() {
            order.push(((r + 1) * cols + ac, r * cols + ac));
            along(r, &mut order);
        }
        let mut seen = vec![usize::MAX; n];
        for (from, to) in order {
            let (c, p) = (&refs[from], &refs[to]);
            let m = normal(c, p);
            let mut odd = false;
            for &e in list(from).iter().chain(list(to)) {
                if seen[e as usize] != to {
                    seen[e as usize] = to;
                    odd ^= crosses(c, p, &m, &edges[e as usize]);
                }
            }
            inside[to] = inside[from] ^ odd;
        }

        let w = cols + 1;
        let (mut busy, mut full) = (vec![0u32; (rows + 1) * w], vec![0u32; (rows + 1) * w]);
        for r in 0..rows {
            for c in 0..cols {
                let cell = r * cols + c;
                let b = u32::from(start[cell + 1] > start[cell]);
                let i = u32::from(b == 0 && inside[cell]);
                let (at, up, left, diag) = ((r + 1) * w + c + 1, r * w + c + 1, (r + 1) * w + c, r * w + c);
                busy[at] = b + busy[up] + busy[left] - busy[diag];
                full[at] = i + full[up] + full[left] - full[diag];
            }
        }
        Ok(Self { vertices, edges, grid: Grid { frame: f, refs, inside, start, lists, busy, full } })
    }

    /// Whether a truncated point (1e-6 degrees) is inside.
    #[inline]
    pub fn contains(&self, lat: i32, lng: i32) -> bool {
        let (la, ln) = (deg(lat), deg(lng));
        let g = &self.grid;
        let Some(cell) = g.frame.cell(la, ln) else {
            return false;
        };
        let (s, e) = (g.start[cell] as usize, g.start[cell + 1] as usize);
        if s == e {
            return g.inside[cell];
        }
        let (c, p) = (&g.refs[cell], unit(la, ln));
        let m = normal(c, &p);
        g.lists[s..e].iter().fold(g.inside[cell], |inside, &i| inside ^ crosses(c, &p, &m, &self.edges[i as usize]))
    }
}

impl Region for Polygon {
    fn relate(&self, b: [i32; 4]) -> Relation {
        self.grid.relate(b)
    }
    fn contains(&self, lat: i32, lng: i32) -> bool {
        Polygon::contains(self, lat, lng)
    }
}

impl PartialEq for Polygon {
    fn eq(&self, other: &Self) -> bool {
        self.vertices == other.vertices
    }
}

impl std::fmt::Debug for Polygon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Polygon({:?})", self.vertices)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{e6, GeoIndex};
    use super::*;
    use crate::schema::DocId;

    fn poly(v: &[(f64, f64)]) -> Polygon {
        Polygon::new(v).unwrap()
    }

    fn has(p: &Polygon, lat: f64, lng: f64) -> bool {
        p.contains(e6(lat), e6(lng))
    }

    /// Inside when the loop winds around the point, seen from it (for loops well within a
    /// hemisphere and points near them).
    fn winding(v: &[(f64, f64)], lat: i32, lng: i32) -> bool {
        let p = unit(deg(lat), deg(lng));
        let pts: Vec<V3> = v.iter().map(|&(a, b)| unit(a, b)).collect();
        let sum: f64 = (0..pts.len())
            .map(|i| {
                let (a, b) = (&pts[i], &pts[(i + 1) % pts.len()]);
                dot(&cross(a, b), &p).atan2(dot(a, b) - dot(&p, a) * dot(&p, b))
            })
            .sum();
        sum.abs() > std::f64::consts::PI
    }

    fn rng(seed: u64) -> impl FnMut() -> f64 {
        let mut s = seed;
        move || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// A star-shaped polygon around a centre, its vertices at sorted random bearings.
    fn star(next: &mut impl FnMut() -> f64, lat: f64, lng: f64, radius: f64, n: usize) -> Vec<(f64, f64)> {
        let mut bearings: Vec<f64> = (0..n).map(|_| next() * std::f64::consts::TAU).collect();
        bearings.sort_by(f64::total_cmp);
        let v: Vec<(f64, f64)> = bearings
            .iter()
            .map(|&b| {
                let r = radius * (0.3 + 0.7 * next());
                (lat + r * b.sin(), lng + r * b.cos() / (lat * RAD).cos())
            })
            .collect();
        if next() < 0.5 { v.into_iter().rev().collect() } else { v }
    }

    #[test]
    fn either_direction_is_the_smaller_region() {
        let ccw = [(59.0, 18.0), (59.0, 19.0), (60.0, 19.0), (60.0, 18.0)];
        let mut cw = ccw;
        cw.reverse();
        for v in [&ccw[..], &cw[..]] {
            let p = poly(v);
            assert!(has(&p, 59.5, 18.5));
            assert!(!has(&p, 58.9, 18.5));
            assert!(!has(&p, 59.5, 19.1));
            assert!(!has(&p, -59.5, -161.5));
            // the edges are geodesics: the southern one bows north of its parallel
            assert!(!has(&p, 59.0005, 18.5));
            assert!(has(&p, 59.0015, 18.5));
            // boxes for the tree: wholly inside, wholly outside, across the boundary
            let rel = |b: [f64; 4]| p.relate([e6(b[0]), e6(b[1]), e6(b[2]), e6(b[3])]);
            assert!(rel([59.4, 18.4, 59.6, 18.6]) == Relation::Within);
            assert!(rel([61.0, 18.4, 61.5, 18.6]) == Relation::Disjoint);
            assert!(rel([59.4, 16.0, 59.6, 17.9]) == Relation::Disjoint);
            assert!(rel([59.4, 18.9, 59.6, 19.1]) == Relation::Crosses);
            assert!(rel([58.0, 17.0, 61.0, 20.0]) == Relation::Crosses);
        }
    }

    #[test]
    fn around_a_pole() {
        let north = [(80.0, 0.0), (80.0, 90.0), (80.0, 180.0), (80.0, -90.0)];
        let south = [(-70.0, 10.0), (-70.0, 130.0), (-70.0, -110.0)];
        for v in [north.to_vec(), north.iter().rev().copied().collect()] {
            let p = poly(&v);
            assert!(has(&p, 90.0, 0.0) && has(&p, 85.0, 45.0) && has(&p, 83.5, 45.0) && has(&p, 80.5, 0.0));
            assert!(!has(&p, 82.5, 45.0) && !has(&p, 79.5, 0.0) && !has(&p, -85.0, 10.0));
        }
        for v in [south.to_vec(), south.iter().rev().copied().collect()] {
            let p = poly(&v);
            assert!(has(&p, -90.0, 0.0) && has(&p, -80.0, 50.0) && has(&p, -71.0, 10.0));
            assert!(!has(&p, -69.0, 10.0) && !has(&p, 0.0, 0.0) && !has(&p, 90.0, 0.0));
        }
    }

    #[test]
    fn across_the_antimeridian() {
        let v = [(10.0, 170.0), (10.0, -170.0), (-10.0, -170.0), (-10.0, 170.0)];
        for v in [v.to_vec(), v.iter().rev().copied().collect()] {
            let p = poly(&v);
            for (lat, lng) in [(0.0, 175.0), (0.0, -175.0), (0.0, 180.0), (0.0, -180.0), (10.1, 180.0), (-9.9, 170.5)] {
                assert!(has(&p, lat, lng), "{lat},{lng}");
            }
            for (lat, lng) in [(0.0, 0.0), (0.0, 165.0), (0.0, -165.0), (11.0, 180.0), (-10.2, 180.0)] {
                assert!(!has(&p, lat, lng), "{lat},{lng}");
            }
        }
    }

    #[test]
    fn a_loop_round_the_globe() {
        // three vertices 120 degrees apart: the smaller side is the north, the edges bowing to 19.4
        let v = [(10.0, 0.0), (10.0, 120.0), (10.0, -120.0)];
        for v in [v.to_vec(), v.iter().rev().copied().collect()] {
            let p = poly(&v);
            assert!(has(&p, 90.0, 0.0) && has(&p, 30.0, 60.0) && has(&p, 20.0, 0.0) && has(&p, 19.5, -60.0));
            assert!(!has(&p, 0.0, 0.0) && !has(&p, -30.0, 60.0) && !has(&p, 15.0, 60.0) && !has(&p, 19.3, 180.0));
        }
    }

    #[test]
    fn s2s_validation() {
        let err = |v: &[(f64, f64)]| Polygon::new(v).unwrap_err();
        assert_eq!(err(&[(0.0, 0.0), (1.0, 1.0)]), "Polygon is invalid: Non-empty, non-full loops must have at least 3 vertices");
        assert_eq!(err(&[(0.0, 0.0), (1.0, 1.0), (0.0, 0.0)]), "Polygon is invalid: Non-empty, non-full loops must have at least 3 vertices");
        assert_eq!(err(&[(0.0, 0.0), (0.0, 0.0), (1.0, 1.0)]), "Polygon is invalid: Edge 0 is degenerate (duplicate vertex)");
        assert_eq!(err(&[(1.0, 1.0), (0.0, 0.0), (0.0, 360.0)]), "Polygon is invalid: Edge 1 is degenerate (duplicate vertex)");
        assert_eq!(err(&[(10.0, 20.0), (-10.0, -160.0), (5.0, 5.0)]), "Polygon is invalid: Vertices 0 and 1 are antipodal");
        assert_eq!(err(&[(0.0, 0.0), (1.0, 1.0), (1.0, 0.0), (0.0, 1.0)]), "Polygon is invalid: Edge 0 crosses edge 2");
        assert_eq!(
            err(&[(0.0, 0.0), (1.0, 1.0), (0.0, 2.0), (1.0, 3.0), (2.0, 2.0), (1.0, 1.0)]),
            "Polygon is invalid: Edge 1 has duplicate vertex with edge 5"
        );
        assert_eq!(err(&[(91.0, 0.0), (0.0, 1.0), (1.0, 1.0)]), "Polygon is invalid: Vertex 0 has a latitude outside [-90, 90]");
        // a closed ring is the same polygon
        let ring = poly(&[(59.0, 18.0), (59.0, 19.0), (60.0, 19.0), (59.0, 18.0)]);
        assert_eq!(ring, poly(&[(59.0, 18.0), (59.0, 19.0), (60.0, 19.0)]));
    }

    #[test]
    fn a_shared_boundary_point_is_in_one_polygon() {
        // four tiles meeting at (60, 19)
        let tiles: Vec<Polygon> = [(59.0, 18.0), (59.0, 19.0), (60.0, 18.0), (60.0, 19.0)]
            .iter()
            .map(|&(la, ln)| poly(&[(la, ln), (la, ln + 1.0), (la + 1.0, ln + 1.0), (la + 1.0, ln)]))
            .collect();
        let count = |lat: i32, lng: i32| tiles.iter().filter(|t| t.contains(lat, lng)).count();
        assert_eq!(count(e6(60.0), e6(19.0)), 1);
        for k in 1..200 {
            assert_eq!(count(e6(59.0) + k * 10_000, e6(19.0)), 1, "{k}");
            assert_eq!(count(e6(60.0), e6(18.0) + k * 10_000), 1, "{k}");
        }
    }

    #[test]
    fn matches_the_winding_number() {
        let mut next = rng(7);
        let mut tested = 0;
        for _ in 0..200 {
            let (lat, lng) = (-70.0 + 140.0 * next(), -180.0 + 360.0 * next());
            let radius = 0.01 + 5.0 * next();
            let n = 3 + (next() * 40.0) as usize;
            let v = star(&mut next, lat, lng, radius, n);
            let Ok(p) = Polygon::new(&v) else { continue };
            tested += 1;
            for _ in 0..300 {
                let la = e6((lat + radius * (2.5 * next() - 1.25)).clamp(-90.0, 90.0));
                let ln = e6(wrap(lng + radius * (2.5 * next() - 1.25) / (lat * RAD).cos()));
                assert_eq!(p.contains(la, ln), winding(&v, la, ln), "{v:?} {la} {ln}");
            }
        }
        assert!(tested > 150, "{tested}");
    }

    #[test]
    fn wide_cells_beside_the_edges() {
        // few vertices over tens of degrees: wide cells, where an arc bows out of its cell
        let mut next = rng(3);
        let mut tested = 0;
        for _ in 0..300 {
            let (lat, lng) = (-40.0 + 80.0 * next(), -180.0 + 360.0 * next());
            let (radius, n) = (10.0 + 35.0 * next(), 3 + (next() * 9.0) as usize);
            let v = star(&mut next, lat, lng, radius, n);
            let Ok(p) = Polygon::new(&v) else { continue };
            tested += 1;
            let pts: Vec<V3> = v.iter().map(|&(a, b)| unit(a, b)).collect();
            for i in 0..pts.len() {
                let (a, b) = (&pts[i], &pts[(i + 1) % pts.len()]);
                let side = normalize(&cross(a, b));
                for _ in 0..40 {
                    let t = next();
                    let x = normalize(&add(&[a[0] * (1.0 - t), a[1] * (1.0 - t), a[2] * (1.0 - t)], &[b[0] * t, b[1] * t, b[2] * t]));
                    let d = (if next() < 0.5 { -1.0 } else { 1.0 }) * 10f64.powf(-5.0 + 3.0 * next());
                    let (la, ln) = lat_lng(&normalize(&[x[0] + d * side[0], x[1] + d * side[1], x[2] + d * side[2]]));
                    let (la, ln) = (e6(la), e6(ln));
                    assert_eq!(p.contains(la, ln), winding(&v, la, ln), "{v:?} {la} {ln}");
                }
            }
        }
        assert!(tested > 200, "{tested}");
    }

    #[test]
    fn a_large_polygon() {
        // a wiggly circle (long spokes would bow across their neighbours: the edges are geodesics)
        let mut next = rng(5);
        let v: Vec<(f64, f64)> = (0..5000)
            .map(|k| {
                let (b, r) = (k as f64 * std::f64::consts::TAU / 5000.0, 6.0 * (0.95 + 0.05 * next()));
                (62.0 + r * b.sin(), 16.0 + r * b.cos() / (62.0 * RAD).cos())
            })
            .collect();
        let p = poly(&v);
        for _ in 0..20_000 {
            let (la, ln) = (e6(56.0 + 12.0 * next()), e6(4.0 + 24.0 * next()));
            assert_eq!(p.contains(la, ln), winding(&v, la, ln), "{la} {ln}");
        }
    }

    #[test]
    fn the_tree_agrees_with_every_point() {
        let mut next = rng(11);
        for multi in [false, true] {
            let mut pts: Vec<(DocId, i32, i32)> = Vec::new();
            for d in 0..6000u32 {
                for _ in 0..if multi { 3 } else { 1 } {
                    if next() < 0.8 {
                        pts.push((d, e6(55.0 + next() * 14.0), e6(11.0 + next() * 13.0)));
                    }
                }
            }
            let idx = GeoIndex::build(&pts, 6000, multi).unwrap();
            for (lat, lng, radius, n) in [(62.0, 17.0, 4.0, 25), (61.0, 16.0, 9.0, 6), (59.3, 18.0, 0.2, 40), (66.0, 20.0, 2.0, 3)] {
                let p = poly(&star(&mut next, lat, lng, radius, n));
                let mut want: Vec<DocId> = pts.iter().filter(|&&(_, la, ln)| p.contains(la, ln)).map(|t| t.0).collect();
                want.dedup();
                assert_eq!(idx.in_polygon(&p).to_vec(), want, "{lat},{lng},{radius}");
                for &(d, _, _) in pts.iter().step_by(7) {
                    assert_eq!(idx.doc_in_polygon(d, &p), want.binary_search(&d).is_ok());
                }
            }
        }
    }
}
