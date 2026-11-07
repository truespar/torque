//! Facets (`facet_by`): per field the values of the search's documents and how many of
//! them have each, a document counting once per value. Numeric fields can also be counted in ranges
//! (`price(cheap:[0, 100], dear:[100, ])`, a range holding its lower bound and not its upper one)
//! and get stats. The values come most frequent first, in value order (`sort_by:_alpha:asc|desc`)
//! or by another field of their documents (`sort_by:points:asc|desc`), then at most
//! `max_facet_values` of them. A grouped search's facets count groups, a value the groups of its
//! documents (`DocGroups`); `top_k:true` counts the search's first hits only (the top 250, or
//! the hits up to the page's end when more); a facet_query keeps the values it matches
//! (`ValueQuery`).
//!
//! Counts are exact: every value of the documents is counted, whatever `facet_sample_percent` asks;
//! floats are kept and printed as stored; equal counts go in value order; a value sorted by another
//! field takes the largest value of it among its documents descending and the smallest ascending;
//! sorting and then keeping `max_facet_values` is one meaning for every order.

use crate::bitmap::Bitmap;
use crate::index::values::{f64_key, key_f64, BoolIndex, DocOrds, ValueIndex};
use crate::schema::{DocId, FieldDef, FieldId, FieldType, Schema};
use rustc_hash::FxHashMap;
use serde::Serialize;
use std::cmp::Ordering;
use std::ops::Bound;

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
#[derive(Debug, Clone, Serialize)]
pub struct FacetResult {
    pub counts: Vec<FacetCount>,
    pub field_name: String,
    /// How the values are ordered (for merging segments).
    #[serde(skip)]
    pub order: FacetOrder,
    /// Place in `facet_by`.
    #[serde(skip)]
    pub position: usize,
    /// Counted in ranges (`price(cheap:[0, 100])`): the values are the ranges' labels.
    #[serde(skip)]
    pub ranges: bool,
    pub sampled: bool,
    pub stats: FacetStats,
}

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
#[derive(Debug, Clone, Serialize)]
pub struct FacetCount {
    pub count: usize,
    /// A reference facet's filter for the value (`$brands(country: `Sweden`)`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facet_filter: Option<String>,
    /// A grouped search's facet in one of several segments: the groups counted, for the merge.
    #[serde(skip)]
    pub groups: Option<Vec<u64>>,
    pub highlighted: String,
    /// The value's place in value order: a number's key, a bool as 0 or 1, a range's index; None
    /// for strings, which order by their bytes.
    #[serde(skip)]
    pub key: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<serde_json::Value>,
    /// For `sort_by:<field>`: that field's largest value among the value's documents descending,
    /// its smallest ascending.
    #[serde(skip)]
    pub sort_value: Option<i64>,
    pub value: String,
}

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
/// Typesense's facet `stats`: min, max, sum and average of a numeric field's values, and for every
/// facet the number of values (ranges) found.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FacetStats {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg: Option<f64>,
    /// Values counted (for merging segments' stats).
    #[serde(skip)]
    pub count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum: Option<f64>,
    pub total_values: usize,
}

impl FacetStats {
    /// Add another segment's stats (`total_values` is set by the merge).
    pub fn merge(&mut self, other: &FacetStats) {
        if other.count == 0 {
            return;
        }
        let pick = |a: Option<f64>, b: Option<f64>, f: fn(f64, f64) -> f64| match (a, b) {
            (Some(a), Some(b)) => Some(f(a, b)),
            (a, b) => a.or(b),
        };
        self.min = pick(self.min, other.min, f64::min);
        self.max = pick(self.max, other.max, f64::max);
        self.sum = pick(self.sum, other.sum, |a, b| a + b);
        self.count += other.count;
        self.avg = self.sum.map(|s| s / self.count as f64);
    }
}

/// The order of a facet's values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FacetOrder {
    /// Most frequent first, equal counts in value order.
    #[default]
    Count,
    /// `sort_by:_alpha:asc|desc`.
    Value { desc: bool },
    /// `sort_by:<field>:asc|desc`: by the field's value of the value's documents, then by count
    /// the same way.
    Field { desc: bool },
}

// -- facet_by --------------------------------------------------------------------------------------

/// A `facet_by` entry on a field of the collection.
#[derive(Debug, Clone)]
pub struct FacetSpec {
    pub fid: FieldId,
    pub name: String,
    /// A range facet's ranges, ordered.
    pub ranges: Vec<FacetRange>,
    pub order: FacetOrder,
    /// The field of `sort_by:<field>`.
    pub sort_field: Option<FieldId>,
    /// Matched by a `name*` pattern: left out when none of the documents has a value.
    pub wildcard: bool,
    /// `top_k:true`: counts the search's first hits only.
    pub top_k: bool,
    /// Place in `facet_by`.
    pub position: usize,
}

/// A range of a range facet: the values with a key from `lo` up to `hi` (excluded); unbounded
/// where None.
#[derive(Debug, Clone)]
pub struct FacetRange {
    pub label: String,
    pub lo: Option<i64>,
    pub hi: Option<i64>,
}

/// A `$collection(...)` entry: facets on fields of a referenced collection.
#[derive(Debug, Clone)]
pub struct JoinFacet {
    pub collection: String,
    /// The entries inside the parentheses, for the referenced collection's schema.
    pub entries: String,
    pub position: usize,
}

#[derive(Debug, Default)]
pub struct FacetBy {
    pub local: Vec<FacetSpec>,
    pub joins: Vec<JoinFacet>,
}

/// The parts of `s` between commas outside parentheses and brackets.
pub fn split_top(s: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut start) = (Vec::new(), 0i32, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn is_string(f: &FieldDef) -> bool {
    matches!(f.field_type, FieldType::String | FieldType::Text | FieldType::StringArray | FieldType::StringStar)
}

fn is_number(f: &FieldDef) -> bool {
    matches!(
        f.field_type,
        FieldType::Int32 | FieldType::Int64 | FieldType::Float64 | FieldType::Int32Array | FieldType::Int64Array | FieldType::FloatArray { num_dim: 0 }
    )
}

fn is_float(f: &FieldDef) -> bool {
    matches!(f.field_type, FieldType::Float64 | FieldType::FloatArray { .. })
}

/// Parse `facet_by` with Typesense's syntax and messages. Entries `i` are numbered `i << 16`, a
/// pattern's fields after it.
pub fn parse_facet_by(facet_by: &str, schema: &Schema) -> Result<FacetBy, String> {
    let mut out = FacetBy::default();
    for (i, entry) in split_top(facet_by).into_iter().map(str::trim).filter(|e| !e.is_empty()).enumerate() {
        let position = i << 16;
        if let Some(rest) = entry.strip_prefix('$') {
            let open = rest.find('(').filter(|_| rest.ends_with(')')).ok_or_else(|| format!("Error parsing reference facet: `{entry}`."))?;
            out.joins.push(JoinFacet { collection: rest[..open].trim().to_string(), entries: rest[open + 1..rest.len() - 1].to_string(), position });
        } else {
            parse_entry(entry, schema, position, &mut out.local)?;
        }
    }
    Ok(out)
}

fn parse_entry(entry: &str, schema: &Schema, position: usize, out: &mut Vec<FacetSpec>) -> Result<(), String> {
    let facet_field = |name: &str| -> Result<FieldId, String> {
        schema
            .name_to_id
            .get(name)
            .copied()
            .filter(|&f| schema.fields[f as usize].facet)
            .ok_or_else(|| format!("Could not find a facet field named `{name}` in the schema."))
    };
    let plain = |fid: FieldId, name: &str, wildcard: bool, position: usize| FacetSpec {
        fid,
        name: name.to_string(),
        ranges: Vec::new(),
        order: FacetOrder::Count,
        sort_field: None,
        wildcard,
        top_k: false,
        position,
    };
    if let Some(open) = entry.find('(') {
        if !entry.ends_with(')') {
            return Err("Invalid facet format.".into());
        }
        let name = entry[..open].trim();
        let fid = facet_field(name)?;
        let def = &schema.fields[fid as usize];
        let mut spec = plain(fid, name, false, position);
        for param in split_top(&entry[open + 1..entry.len() - 1]).into_iter().map(str::trim) {
            let (key, value) = param.split_once(':').ok_or_else(|| format!("Invalid facet param `{param}`."))?;
            match key.trim() {
                "sort_by" => {
                    let (by, dir) = value.split_once(':').filter(|(_, d)| !d.contains(':')).ok_or("Invalid sort format.")?;
                    let desc = match dir.trim() {
                        "asc" => false,
                        "desc" => true,
                        _ => return Err("Invalid sort param.".into()),
                    };
                    if by.trim() == "_alpha" {
                        if !is_string(def) {
                            return Err("Facet field should be string type to apply alpha sort.".into());
                        }
                        spec.order = FacetOrder::Value { desc };
                    } else {
                        let sf = facet_field(by.trim())?;
                        if is_string(&schema.fields[sf as usize]) {
                            return Err("Sort field should be non string type to apply sort.".into());
                        }
                        spec.order = FacetOrder::Field { desc };
                        spec.sort_field = Some(sf);
                    }
                }
                "top_k" => {
                    spec.top_k = match value.trim() {
                        "true" => true,
                        "false" => false,
                        _ => return Err("top_k string format is invalid.".into()),
                    };
                }
                label => {
                    let inner = value.trim().strip_prefix('[').and_then(|v| v.strip_suffix(']')).ok_or_else(|| format!("Invalid facet param `{param}`."))?;
                    if !is_number(def) {
                        return Err("Range facet is restricted to only integer and float fields.".into());
                    }
                    if !def.sort {
                        return Err("Range facets require sort enabled for the field.".into());
                    }
                    if label.is_empty() {
                        return Err("Facet range value is not valid.".into());
                    }
                    let (lo, hi) = inner.split_once(',').filter(|(_, h)| !h.contains(',')).ok_or("Error splitting the facet range values.")?;
                    let bound = |s: &str| -> Result<Option<i64>, String> {
                        let s = s.trim();
                        let invalid = || "Facet range value is not valid.".to_string();
                        Ok(match s {
                            "" => None,
                            _ if is_float(def) => Some(f64_key(s.parse::<f64>().map_err(|_| invalid())?)),
                            _ => Some(s.parse::<i64>().map_err(|_| invalid())?),
                        })
                    };
                    spec.ranges.push(FacetRange { label: label.to_string(), lo: bound(lo)?, hi: bound(hi)? });
                }
            }
        }
        // Ranges in order, each starting where an earlier one ends.
        let lo = |r: &FacetRange| r.lo.unwrap_or(i64::MIN);
        let hi = |r: &FacetRange| r.hi.unwrap_or(i64::MAX);
        spec.ranges.sort_by(|a, b| lo(a).cmp(&lo(b)).then(hi(a).cmp(&hi(b))).then_with(|| a.label.cmp(&b.label)));
        for i in 1..spec.ranges.len() {
            if !spec.ranges[..i].iter().any(|r| hi(r) == lo(&spec.ranges[i])) {
                return Err("Ranges in range facet syntax should be continous.".into());
            }
        }
        out.push(spec);
    } else if let Some(prefix) = entry.strip_suffix('*') {
        if prefix.contains('*') {
            return Err("Only prefix matching with a wildcard is allowed.".into());
        }
        let before = out.len();
        for f in schema.fields.iter().filter(|f| f.facet && f.name.starts_with(prefix)) {
            out.push(plain(f.id, &f.name, true, position + out.len() - before));
        }
        if out.len() == before {
            return Err(format!("Could not find a facet field for `{entry}` in the schema."));
        }
    } else {
        if entry.contains('*') {
            return Err("Only prefix matching with a wildcard is allowed.".into());
        }
        out.push(plain(facet_field(entry)?, entry, false, position));
    }
    Ok(())
}

// -- counting ----------------------------------------------------------------------------------------

/// Per-ordinal counts of the candidates' values, ascending by ordinal, only the ordinals in
/// `allowed` (ascending) when given. The candidates' ordinals are counted (O(candidates)) when the
/// field keeps per-document ordinals and that costs less than intersecting each value's postings
/// with them (O(values)).
pub fn ordinal_counts(candidates: &Bitmap, idx: &ValueIndex, allowed: Option<&[u64]>) -> Vec<(u64, u32)> {
    let n = idx.len();
    let card = candidates.cardinality();
    if card == 0 || n == 0 {
        return Vec::new();
    }
    // about: a document's ordinal a few nanoseconds to read, an array's elements tens; a value's
    // postings a microsecond to intersect for every 65,536 documents of the segment
    let values = allowed.map_or(n, |a| a.len() as u64);
    match idx.doc_ords() {
        Some(ords) if card * if ords.is_multi() { 40 } else { 4 } < values * 1000 * u64::from(idx.doc_count()).div_ceil(65536) => {
            counts_by_documents(candidates, idx, ords, allowed)
        }
        _ => counts_by_values(candidates, idx, allowed),
    }
}

/// `ordinal_counts` from the candidates' ordinals.
fn counts_by_documents(candidates: &Bitmap, idx: &ValueIndex, ords: &DocOrds, allowed: Option<&[u64]>) -> Vec<(u64, u32)> {
    let (n, card) = (idx.len(), candidates.cardinality());
    let keep = |o: u64| allowed.is_none_or(|a| a.binary_search(&o).is_ok());
    let mut found: Vec<(u64, u32)> = Vec::new();
    let mut buf: Vec<u64> = Vec::new();
    let mut each = |f: &mut dyn FnMut(u64)| {
        for doc in candidates.iter() {
            buf.clear();
            buf.extend(ords.elems(doc).flatten());
            if buf.len() > 1 {
                buf.sort_unstable();
                buf.dedup();
            }
            buf.iter().for_each(|&o| f(o));
        }
    };
    if n > 64 * card {
        let mut counts: FxHashMap<u64, u32> = FxHashMap::default();
        each(&mut |o| *counts.entry(o).or_default() += 1);
        found.extend(counts.into_iter().filter(|&(o, _)| keep(o)));
        found.sort_unstable();
    } else {
        let mut counts = vec![0u32; n as usize];
        each(&mut |o| counts[o as usize] += 1);
        found.extend(counts.into_iter().enumerate().filter(|&(o, k)| k > 0 && keep(o as u64)).map(|(o, k)| (o as u64, k)));
    }
    found
}

/// `ordinal_counts` from each value's postings intersected with the candidates.
fn counts_by_values(candidates: &Bitmap, idx: &ValueIndex, allowed: Option<&[u64]>) -> Vec<(u64, u32)> {
    let mut found: Vec<(u64, u32)> = Vec::new();
    // Every document a candidate (an unfiltered facet): the counts are the postings' sizes.
    let all = candidates.cardinality() == u64::from(idx.doc_count());
    let mut count = |o: u64| {
        let k = if all { idx.df(o) } else { idx.count_in(o, candidates) };
        if k > 0 {
            found.push((o, k as u32));
        }
    };
    match allowed {
        Some(a) => a.iter().for_each(|&o| count(o)),
        None => (0..idx.len()).for_each(count),
    }
    found
}

/// For `sort_by:<field>`: per counted ordinal the largest key of `by` among the candidates with
/// the value (descending), else the smallest.
fn ordinal_sort_values(candidates: &Bitmap, idx: &ValueIndex, by: &ValueIndex, desc: bool, counts: &[(u64, u32)]) -> FxHashMap<u64, i64> {
    let mut best: FxHashMap<u64, i64> = FxHashMap::default();
    let mut take = |o: u64, k: i64| {
        best.entry(o).and_modify(|b| *b = if desc { (*b).max(k) } else { (*b).min(k) }).or_insert(k);
    };
    match idx.doc_ords() {
        Some(ords) => {
            for doc in candidates.iter() {
                if let Some(k) = by.doc_key(doc) {
                    ords.elems(doc).flatten().for_each(|o| take(o, k));
                }
            }
        }
        None => {
            for &(o, _) in counts {
                for doc in (&idx.docs(o) & candidates).iter() {
                    if let Some(k) = by.doc_key(doc) {
                        take(o, k);
                    }
                }
            }
        }
    }
    best
}

/// Order `items` as `order` asks and keep the first `max`.
pub fn rank<T>(
    items: &mut Vec<T>,
    order: FacetOrder,
    max: usize,
    count: impl Fn(&T) -> u64,
    sort_value: impl Fn(&T) -> Option<i64>,
    by_value: impl Fn(&T, &T) -> Ordering,
) {
    let cmp = |a: &T, b: &T| -> Ordering {
        match order {
            FacetOrder::Count => count(b).cmp(&count(a)).then_with(|| by_value(a, b)),
            FacetOrder::Value { desc } => {
                let o = by_value(a, b);
                if desc { o.reverse() } else { o }
            }
            FacetOrder::Field { desc } => {
                let o = match (sort_value(a), sort_value(b)) {
                    (Some(x), Some(y)) => {
                        let o = x.cmp(&y).then_with(|| count(a).cmp(&count(b)));
                        if desc { o.reverse() } else { o }
                    }
                    // without one, last
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                };
                o.then_with(|| by_value(a, b))
            }
        }
    };
    if items.len() > max {
        if max > 0 {
            items.select_nth_unstable_by(max - 1, &cmp);
        }
        items.truncate(max);
    }
    items.sort_unstable_by(&cmp);
}

/// A number as Typesense prints facet values: integers in full, floats as their shortest exact
/// form ("4.5", "3").
pub fn number_text(float: bool, key: i64) -> String {
    if float { key_f64(key).to_string() } else { key.to_string() }
}

/// A value's text: a string itself, a number printed.
fn value_text(idx: &ValueIndex, o: u64) -> Option<String> {
    if idx.is_str() {
        idx.str_at(o)
    } else {
        idx.key_at(o).map(|k| number_text(idx.is_float(), k))
    }
}

/// Each document's group, for facets of a grouped search (`group_by`), which count groups as
/// the search counts them: a value's count is the number of groups among its
/// documents, the `found` a filter on the value gives.
pub struct DocGroups {
    docs: Vec<DocId>,
    ids: Vec<u64>,
}

impl DocGroups {
    /// The groups of `docs` (every document a facet of the search may count).
    pub fn new(docs: &Bitmap, mut group: impl FnMut(DocId) -> u64) -> Self {
        let docs: Vec<DocId> = docs.iter().collect();
        let ids = docs.iter().map(|&d| group(d)).collect();
        Self { docs, ids }
    }

    /// The document's group.
    #[inline]
    pub fn of(&self, doc: DocId) -> u64 {
        self.docs.binary_search(&doc).map_or(u64::MAX, |i| self.ids[i])
    }

    /// The distinct groups of `docs`, ascending.
    fn distinct(&self, docs: impl Iterator<Item = DocId>) -> Vec<u64> {
        let mut g: Vec<u64> = docs.map(|d| self.of(d)).collect();
        g.sort_unstable();
        g.dedup();
        g
    }
}

/// A facet_query on a facet's values (`search::facet_query`).
pub trait ValueQuery: Sync {
    /// The typos by which the value matches; None when it does not.
    fn cost(&self, value: &str) -> Option<u32>;
    /// The value with what matched within `level` typos marked.
    fn highlight(&self, value: &str, level: u32) -> String;
}

/// How a facet counts.
#[derive(Clone, Copy)]
pub struct Counting<'a> {
    /// The values to give (`max_facet_values`).
    pub max: usize,
    /// One of several segments: every value, with its groups, for the merge.
    pub multi: bool,
    /// group_by: the documents' groups, counted in place of the documents.
    pub groups: Option<&'a DocGroups>,
    /// facet_query on the field: only the values it matches within the typos given (the fewest by
    /// which any result of the search matches, `query_level`), marked.
    pub query: Option<(&'a dyn ValueQuery, u32)>,
}

/// Where a facet's values are read from.
pub enum FacetSource<'a> {
    /// A string or numeric field, with the value index of a `sort_by:<field>`.
    Values { idx: &'a ValueIndex, sort_by: Option<&'a ValueIndex> },
    Bools(&'a BoolIndex),
    /// No document of the segment has the field.
    Absent,
}

/// The fewest typos by which a value of `candidates` matches `query`; None when none does.
pub fn query_level(source: &FacetSource<'_>, candidates: &Bitmap, query: &dyn ValueQuery) -> Option<u32> {
    match source {
        FacetSource::Values { idx, .. } => ordinal_counts(candidates, idx, None).into_iter().filter_map(|(o, _)| query.cost(&value_text(idx, o)?)).min(),
        FacetSource::Bools(b) => [false, true].into_iter().filter(|&v| b.docs(v).intersects(candidates)).filter_map(|v| query.cost(&v.to_string())).min(),
        FacetSource::Absent => None,
    }
}

/// Whether `value` passes the facet_query of `how`, when there is one.
fn passes(how: &Counting<'_>, value: &str) -> bool {
    how.query.is_none_or(|(q, level)| q.cost(value).is_some_and(|c| c <= level))
}

/// The facet of `spec` over `candidates`. None for a pattern's field none of them has.
pub fn compute_facet(spec: &FacetSpec, source: FacetSource<'_>, candidates: &Bitmap, how: Counting<'_>) -> Option<FacetResult> {
    let mut result = empty(spec);
    match source {
        FacetSource::Values { idx, sort_by } => {
            let mut counts = ordinal_counts(candidates, idx, None);
            if how.query.is_some() && spec.ranges.is_empty() {
                counts.retain(|&(o, _)| value_text(idx, o).is_some_and(|v| passes(&how, &v)));
            }
            let groups = how.groups.map(|g| ordinal_groups(candidates, idx, g, &counts));
            let sort_values = match (spec.order, sort_by) {
                (FacetOrder::Field { desc }, Some(by)) if spec.ranges.is_empty() => Some(ordinal_sort_values(candidates, idx, by, desc, &counts)),
                _ => None,
            };
            let ranges = match &groups {
                Some(g) => range_groups(&spec.ranges, idx, &counts, g),
                None => {
                    let multi = idx.doc_ords().is_some_and(|o| o.is_multi());
                    range_counts(&spec.ranges, idx, &counts, multi.then_some(candidates)).into_iter().map(|n| (n, None)).collect()
                }
            };
            finish(&mut result, spec, idx, counts, groups, sort_values, ranges, how);
        }
        FacetSource::Bools(b) => {
            let mut items: Vec<(bool, u64, Option<Vec<u64>>)> = [false, true]
                .into_iter()
                .filter(|v| passes(&how, &v.to_string()))
                .filter_map(|v| {
                    let docs = b.docs(v) & candidates;
                    let groups = how.groups.map(|g| g.distinct(docs.iter()));
                    let n = groups.as_ref().map_or(docs.cardinality(), |g| g.len() as u64);
                    (n > 0).then_some((v, n, groups))
                })
                .collect();
            result.stats.total_values = items.len();
            rank(&mut items, FacetOrder::Count, how.max, |x| x.1, |_| None, |a, b| a.0.cmp(&b.0));
            result.counts = items
                .into_iter()
                .map(|(v, n, groups)| {
                    let value = v.to_string();
                    FacetCount {
                        parent: None,
                        highlighted: how.query.map_or_else(|| value.clone(), |(q, level)| q.highlight(&value, level)),
                        value,
                        count: n as usize,
                        facet_filter: None,
                        key: Some(i64::from(v)),
                        sort_value: None,
                        groups: groups.filter(|_| how.multi),
                    }
                })
                .collect();
        }
        FacetSource::Absent => {}
    }
    (!spec.wildcard || !result.counts.is_empty()).then_some(result)
}

/// A spec's facet with nothing counted.
pub fn empty(spec: &FacetSpec) -> FacetResult {
    FacetResult {
        field_name: spec.name.clone(),
        sampled: false,
        counts: Vec::new(),
        stats: FacetStats::default(),
        order: spec.order,
        position: spec.position,
        ranges: !spec.ranges.is_empty(),
    }
}

/// Per counted ordinal (`counts`, ascending) the distinct groups of the candidates with it.
fn ordinal_groups(candidates: &Bitmap, idx: &ValueIndex, groups: &DocGroups, counts: &[(u64, u32)]) -> Vec<Vec<u64>> {
    let mut out: Vec<Vec<u64>> = vec![Vec::new(); counts.len()];
    match idx.doc_ords() {
        Some(ords) => {
            for doc in candidates.iter() {
                let g = groups.of(doc);
                for o in ords.elems(doc).flatten() {
                    if let Ok(i) = counts.binary_search_by_key(&o, |c| c.0) {
                        out[i].push(g);
                    }
                }
            }
        }
        None => {
            for (i, &(o, _)) in counts.iter().enumerate() {
                out[i].extend((&idx.docs(o) & candidates).iter().map(|d| groups.of(d)));
            }
        }
    }
    for g in &mut out {
        g.sort_unstable();
        g.dedup();
    }
    out
}

/// Per range of a range facet its count over the candidates: the counts of its values summed, or
/// for a multi-valued field the candidates with a value in it.
pub fn range_counts(ranges: &[FacetRange], idx: &ValueIndex, counts: &[(u64, u32)], multi: Option<&Bitmap>) -> Vec<u64> {
    ranges
        .iter()
        .map(|r| {
            let ords = idx.key_ords(r.lo.map_or(Bound::Unbounded, Bound::Included), r.hi.map_or(Bound::Unbounded, Bound::Excluded));
            match multi {
                Some(candidates) => idx.union(ords).and_cardinality(candidates),
                None => {
                    let from = counts.partition_point(|&(o, _)| o < ords.start);
                    counts[from..].iter().take_while(|&&(o, _)| o < ords.end).map(|&(_, k)| u64::from(k)).sum()
                }
            }
        })
        .collect()
}

/// Per range the distinct groups of its values' documents (`groups` aligned with `counts`).
fn range_groups(ranges: &[FacetRange], idx: &ValueIndex, counts: &[(u64, u32)], groups: &[Vec<u64>]) -> Vec<(u64, Option<Vec<u64>>)> {
    ranges
        .iter()
        .map(|r| {
            let ords = idx.key_ords(r.lo.map_or(Bound::Unbounded, Bound::Included), r.hi.map_or(Bound::Unbounded, Bound::Excluded));
            let from = counts.partition_point(|&(o, _)| o < ords.start);
            let to = counts.partition_point(|&(o, _)| o < ords.end);
            let mut g: Vec<u64> = groups[from..to].iter().flatten().copied().collect();
            g.sort_unstable();
            g.dedup();
            (g.len() as u64, Some(g))
        })
        .collect()
}

/// What `finish` reads of a facet's values by ordinal: a segment's value index, or the values a
/// join facet gathered from several segments (`MergedValues`).
pub trait OrdValues {
    fn is_str(&self) -> bool;
    fn is_float(&self) -> bool;
    fn key_at(&self, ord: u64) -> Option<i64>;
    fn str_at(&self, ord: u64) -> Option<String>;
}

impl OrdValues for ValueIndex {
    fn is_str(&self) -> bool {
        ValueIndex::is_str(self)
    }
    fn is_float(&self) -> bool {
        ValueIndex::is_float(self)
    }
    fn key_at(&self, ord: u64) -> Option<i64> {
        ValueIndex::key_at(self, ord)
    }
    fn str_at(&self, ord: u64) -> Option<String> {
        ValueIndex::str_at(self, ord)
    }
}

/// Values from several segments in value order, an ordinal being a value's place.
pub enum MergedValues {
    Strs(Vec<String>),
    Keys { keys: Vec<i64>, float: bool },
}

impl OrdValues for MergedValues {
    fn is_str(&self) -> bool {
        matches!(self, Self::Strs(_))
    }
    fn is_float(&self) -> bool {
        matches!(self, Self::Keys { float: true, .. })
    }
    fn key_at(&self, ord: u64) -> Option<i64> {
        match self {
            Self::Keys { keys, .. } => keys.get(ord as usize).copied(),
            Self::Strs(_) => None,
        }
    }
    fn str_at(&self, ord: u64) -> Option<String> {
        match self {
            Self::Strs(s) => s.get(ord as usize).cloned(),
            Self::Keys { .. } => None,
        }
    }
}

/// Fill a value facet from its per-ordinal document counts (ascending by ordinal) and, grouped,
/// groups: the stats of a numeric field, then the ranges or the values in order, at most `max`.
#[allow(clippy::too_many_arguments)]
pub fn finish(
    result: &mut FacetResult,
    spec: &FacetSpec,
    idx: &dyn OrdValues,
    counts: Vec<(u64, u32)>,
    groups: Option<Vec<Vec<u64>>>,
    sort_values: Option<FxHashMap<u64, i64>>,
    ranges: Vec<(u64, Option<Vec<u64>>)>,
    how: Counting<'_>,
) {
    let float = idx.is_float();
    if !idx.is_str() && !counts.is_empty() {
        let value = |o: u64| idx.key_at(o).map_or(0.0, |k| if float { key_f64(k) } else { k as f64 });
        let (n, sum) = counts.iter().fold((0u64, 0.0f64), |(n, s), &(o, k)| (n + u64::from(k), s + f64::from(k) * value(o)));
        result.stats = FacetStats {
            min: Some(value(counts[0].0)),
            max: Some(value(counts[counts.len() - 1].0)),
            sum: Some(sum),
            avg: Some(sum / n as f64),
            total_values: 0,
            count: n,
        };
    }
    if !spec.ranges.is_empty() {
        let mut items: Vec<(usize, u64, Option<Vec<u64>>)> = ranges.into_iter().enumerate().filter(|(_, r)| r.0 > 0).map(|(i, (n, g))| (i, n, g)).collect();
        result.stats.total_values = items.len();
        rank(&mut items, FacetOrder::Count, how.max, |x| x.1, |_| None, |a, b| a.0.cmp(&b.0));
        result.counts = items
            .into_iter()
            .map(|(i, n, groups)| {
                let label = spec.ranges[i].label.clone();
                FacetCount {
                    parent: None,
                    highlighted: label.clone(),
                    value: label,
                    count: n as usize,
                    facet_filter: None,
                    key: Some(i as i64),
                    sort_value: None,
                    groups: groups.filter(|_| how.multi),
                }
            })
            .collect();
        return;
    }
    let mut groups = groups.map(|g| g.into_iter().map(Some).collect::<Vec<_>>());
    let mut items: Vec<(u64, u64, usize)> = counts
        .iter()
        .enumerate()
        .map(|(i, &(o, k))| (o, groups.as_ref().map_or(u64::from(k), |g| g[i].as_ref().map_or(0, |g| g.len() as u64)), i))
        .collect();
    result.stats.total_values = items.len();
    let sort_value = |o: u64| sort_values.as_ref().and_then(|m| m.get(&o).copied());
    rank(&mut items, spec.order, how.max, |x| x.1, |x| sort_value(x.0), |a, b| a.0.cmp(&b.0));
    result.counts = items
        .into_iter()
        .filter_map(|(o, n, i)| {
            let (value, key) = if idx.is_str() {
                (idx.str_at(o)?, None)
            } else {
                let k = idx.key_at(o)?;
                (number_text(float, k), Some(k))
            };
            Some(FacetCount {
                parent: None,
                highlighted: how.query.map_or_else(|| value.clone(), |(q, level)| q.highlight(&value, level)),
                value,
                count: n as usize,
                facet_filter: None,
                key,
                sort_value: sort_value(o),
                groups: groups.as_mut().and_then(|g| g[i].take()).filter(|_| how.multi),
            })
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::values::{Ords, ValueCollector};

    fn spec(order: FacetOrder) -> FacetSpec {
        FacetSpec { fid: 0, name: "f".into(), ranges: Vec::new(), order, sort_field: None, wildcard: false, top_k: false, position: 0 }
    }

    fn how(max: usize) -> Counting<'static> {
        Counting { max, multi: false, groups: None, query: None }
    }

    /// A facet_query matching values that start with it.
    struct Prefix(&'static str);

    impl ValueQuery for Prefix {
        fn cost(&self, value: &str) -> Option<u32> {
            value.to_lowercase().starts_with(self.0).then_some(0)
        }
        fn highlight(&self, value: &str, _: u32) -> String {
            format!("<mark>{}</mark>{}", &value[..self.0.len()], &value[self.0.len()..])
        }
    }

    fn facet(idx: &ValueIndex, candidates: &Bitmap, max: usize, prefix: Option<&'static str>) -> FacetResult {
        let query = prefix.map(Prefix);
        let how = Counting { query: query.as_ref().map(|q| (q as &dyn ValueQuery, 0)), ..how(max) };
        compute_facet(&spec(FacetOrder::Count), FacetSource::Values { idx, sort_by: None }, candidates, how).unwrap()
    }

    fn got(f: &FacetResult) -> Vec<(&str, usize)> {
        f.counts.iter().map(|c| (c.value.as_str(), c.count)).collect()
    }

    fn colors(n: u32) -> ValueIndex {
        let mut c = ValueCollector::<String>::new(n, Ords::Single);
        for d in 0..n.min(6) {
            let v = match d {
                0..=2 => "red",
                3 | 4 => "blue",
                _ => "green",
            };
            c.add(d, v.to_string());
        }
        ValueIndex::build_strings(c).unwrap()
    }

    fn numbers(vals: &[i64], n: u32, float: bool) -> ValueIndex {
        let mut c = ValueCollector::<i64>::new(n, Ords::Single);
        for (d, &v) in vals.iter().enumerate() {
            c.add(d as u32, if float { f64_key(v as f64 / 2.0) } else { v });
        }
        ValueIndex::build_numbers(c, float).unwrap()
    }

    #[test]
    fn string_facet_counts_by_both_strategies() {
        // 6 docs: the candidate set is not selective; 100 docs: it is.
        for n in [6, 100] {
            let idx = colors(n);
            let mut result = Bitmap::new();
            result.add_range(0..5);
            let f = facet(&idx, &result, 10, None);
            assert_eq!(f.stats.total_values, 2);
            assert_eq!(got(&f), [("red", 3), ("blue", 2)]);
            assert_eq!(f.counts[0].highlighted, "red");
            assert!(f.stats.min.is_none());
        }
    }

    #[test]
    fn counts_by_documents_are_counts_by_values() {
        let mut rng = 3u64;
        let mut next = |n: u64| {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) % n
        };
        for multi in [false, true] {
            let docs = 3000u32;
            let mut c = ValueCollector::<String>::new(docs, if multi { Ords::Multi } else { Ords::Single });
            for d in 0..docs {
                if next(10) == 0 {
                    continue;
                }
                if multi {
                    let elems: Vec<Option<String>> = (0..next(4)).map(|_| (next(8) != 0).then(|| format!("v{}", next(30)))).collect();
                    c.add_many(d, elems);
                } else {
                    c.add(d, format!("v{}", next(30)));
                }
            }
            let idx = ValueIndex::build_strings(c).unwrap();
            let ords = idx.doc_ords().unwrap();
            for share in [1, 3, 50, 1000] {
                let candidates: Bitmap = (0..docs).filter(|_| next(share) == 0).collect();
                let allowed: Vec<u64> = (0..idx.len()).filter(|o| o % 3 == 0).collect();
                for allowed in [None, Some(&allowed[..])] {
                    assert_eq!(counts_by_documents(&candidates, &idx, ords, allowed), counts_by_values(&candidates, &idx, allowed), "multi {multi} share {share}");
                }
            }
        }
    }

    #[test]
    fn grouped_facets_count_groups() {
        // red 0, 1, 2; blue 3, 4; green 5. Groups: 0 and 1 in 7, 2 in 8, 3 to 5 in 9.
        let idx = colors(100);
        let all: Bitmap = (0..6).collect();
        let groups = DocGroups::new(&all, |d| [7, 7, 8, 9, 9, 9][d as usize]);
        let f = compute_facet(&spec(FacetOrder::Count), FacetSource::Values { idx: &idx, sort_by: None }, &all, Counting { groups: Some(&groups), ..how(10) }).unwrap();
        assert_eq!(got(&f), [("red", 2), ("blue", 1), ("green", 1)]);
        assert_eq!(f.stats.total_values, 3);
        assert!(f.counts[0].groups.is_none());
        let f = compute_facet(&spec(FacetOrder::Count), FacetSource::Values { idx: &idx, sort_by: None }, &all, Counting { groups: Some(&groups), multi: true, ..how(10) }).unwrap();
        assert_eq!(f.counts[0].groups, Some(vec![7, 8]));
    }

    #[test]
    fn string_facet_max_values_and_ties_in_value_order() {
        for n in [20u32, 1000] {
            let mut c = ValueCollector::<String>::new(n, Ords::Single);
            for i in 0..20u32 {
                c.add(i, format!("val{:02}", 19 - i));
            }
            let idx = ValueIndex::build_strings(c).unwrap();
            let mut result = Bitmap::new();
            result.add_range(0..20);
            let f = facet(&idx, &result, 5, None);
            let values: Vec<&str> = f.counts.iter().map(|c| c.value.as_str()).collect();
            assert_eq!(values, ["val00", "val01", "val02", "val03", "val04"]);
            assert_eq!(f.stats.total_values, 20);
        }
    }

    #[test]
    fn string_facet_prefix_and_empty_intersection() {
        let idx = colors(100);
        let mut result = Bitmap::new();
        result.add_range(0..6);
        let f = facet(&idx, &result, 10, Some("bl"));
        assert_eq!(f.counts.len(), 1);
        assert_eq!((f.counts[0].value.as_str(), f.counts[0].highlighted.as_str()), ("blue", "<mark>bl</mark>ue"));
        let f = facet(&idx, &Bitmap::of(&[50]), 10, None);
        assert!(f.counts.is_empty());
        assert_eq!(f.stats.total_values, 0);
    }

    #[test]
    fn multi_valued_counts_a_document_once_per_value() {
        let mut c = ValueCollector::<String>::new(50, Ords::Multi);
        c.add_many(0, ["a", "b", "a"].map(|s| Some(s.to_string())));
        c.add_many(1, [Some("a".to_string()), None]);
        let idx = ValueIndex::build_strings(c).unwrap();
        assert_eq!(got(&facet(&idx, &Bitmap::of(&[0, 1]), 10, None)), [("a", 2), ("b", 1)]);
    }

    #[test]
    fn numeric_values_counted_with_stats() {
        // 3 x 2019, 2 x 2021, 1 x 2020, one document without
        let idx = numbers(&[2019, 2021, 2019, 2020, 2021, 2019], 8, false);
        let all: Bitmap = (0..8).collect();
        for n in [all.clone(), Bitmap::of(&[0, 1, 2, 3, 4, 5])] {
            let f = facet(&idx, &n, 10, None);
            assert_eq!(got(&f), [("2019", 3), ("2021", 2), ("2020", 1)]);
            assert_eq!(f.counts[0].key, Some(2019));
            let s = &f.stats;
            assert_eq!((s.min, s.max, s.sum, s.total_values, s.count), (Some(2019.0), Some(2021.0), Some(12119.0), 3, 6));
            assert_eq!(s.avg, Some(12119.0 / 6.0));
        }
        assert_eq!(got(&facet(&idx, &all, 10, Some("202"))), [("2021", 2), ("2020", 1)]);
    }

    #[test]
    fn floats_print_as_stored() {
        // stored as v / 2
        let idx = numbers(&[9, 9, 6, 1], 4, true);
        let f = facet(&idx, &(0..4).collect(), 10, None);
        assert_eq!(got(&f), [("4.5", 2), ("0.5", 1), ("3", 1)]);
        assert_eq!(f.stats.sum, Some(12.5));
    }

    #[test]
    fn range_facets_count_from_the_lower_bound_up_to_the_upper() {
        let vals: Vec<i64> = vec![0, 50, 99, 100, 150, 1000, 5000];
        let idx = numbers(&vals, 8, false);
        let mut s = spec(FacetOrder::Count);
        let r = |label: &str, lo: Option<i64>, hi: Option<i64>| FacetRange { label: label.into(), lo, hi };
        s.ranges = vec![r("cheap", None, Some(100)), r("mid", Some(100), Some(1000)), r("dear", Some(1000), None)];
        let f = compute_facet(&s, FacetSource::Values { idx: &idx, sort_by: None }, &(0..8).collect(), how(10)).unwrap();
        assert_eq!(got(&f), [("cheap", 3), ("mid", 2), ("dear", 2)]);
        assert_eq!(f.stats.total_values, 3);
        assert_eq!(f.stats.count, 7);
        let f = compute_facet(&s, FacetSource::Values { idx: &idx, sort_by: None }, &Bitmap::of(&[3, 4]), how(10)).unwrap();
        assert_eq!(got(&f), [("mid", 2)]);
        // grouped: the groups of each range's documents (0, 50 and 99 in two groups)
        let all: Bitmap = (0..8).collect();
        let groups = DocGroups::new(&all, |d| u64::from(d.min(2) / 2 + d / 4 * 10));
        let f = compute_facet(&s, FacetSource::Values { idx: &idx, sort_by: None }, &all, Counting { groups: Some(&groups), ..how(10) }).unwrap();
        assert_eq!(got(&f), [("cheap", 2), ("mid", 2), ("dear", 1)]);
    }

    #[test]
    fn bools_counted() {
        let b = BoolIndex::build(&[0, 1, 2], &[3]).unwrap();
        let f = compute_facet(&spec(FacetOrder::Count), FacetSource::Bools(&b), &(0..5).collect(), how(10)).unwrap();
        assert_eq!(got(&f), [("true", 3), ("false", 1)]);
        assert!(f.stats.min.is_none());
        let groups = DocGroups::new(&(0..5).collect(), |d| u64::from(d / 2));
        let f = compute_facet(&spec(FacetOrder::Count), FacetSource::Bools(&b), &(0..5).collect(), Counting { groups: Some(&groups), multi: true, ..how(10) }).unwrap();
        assert_eq!(got(&f), [("true", 2), ("false", 1)]);
        assert_eq!(f.counts[0].groups, Some(vec![0, 1]));
    }

    #[test]
    fn value_order_then_max() {
        let idx = colors(100);
        let all: Bitmap = (0..6).collect();
        let by = |desc| compute_facet(&spec(FacetOrder::Value { desc }), FacetSource::Values { idx: &idx, sort_by: None }, &all, how(2)).unwrap();
        assert_eq!(got(&by(false)), [("blue", 2), ("green", 1)]);
        assert_eq!(got(&by(true)), [("red", 3), ("green", 1)]);
    }

    #[test]
    fn field_order_by_the_documents_extreme_value() {
        let idx = colors(6);
        // red docs 0..3 rank 5, 1, 9; blue 3, 4 rank 4, 2; green 5 none
        let rank_idx = {
            let mut c = ValueCollector::<i64>::new(6, Ords::Single);
            for (d, v) in [5, 1, 9, 4, 2].into_iter().enumerate() {
                c.add(d as u32, v);
            }
            ValueIndex::build_numbers(c, false).unwrap()
        };
        let mut s = spec(FacetOrder::Field { desc: true });
        s.sort_field = Some(1);
        let src = || FacetSource::Values { idx: &idx, sort_by: Some(&rank_idx) };
        let f = compute_facet(&s, src(), &(0..6).collect(), how(10)).unwrap();
        assert_eq!(got(&f), [("red", 3), ("blue", 2), ("green", 1)]);
        s.order = FacetOrder::Field { desc: false };
        let f = compute_facet(&s, src(), &(0..6).collect(), how(10)).unwrap();
        assert_eq!(got(&f), [("red", 3), ("blue", 2), ("green", 1)]);
        let f = compute_facet(&s, src(), &Bitmap::of(&[1, 3, 4, 5]), how(10)).unwrap();
        assert_eq!(f.counts.iter().map(|c| (c.value.as_str(), c.sort_value)).collect::<Vec<_>>(), [("red", Some(1)), ("blue", Some(2)), ("green", None)]);
    }

    #[test]
    fn stats_merge() {
        let mut a = FacetStats { min: Some(1.0), max: Some(5.0), sum: Some(6.0), avg: Some(3.0), total_values: 2, count: 2 };
        a.merge(&FacetStats { min: Some(0.0), max: Some(2.0), sum: Some(2.0), avg: Some(2.0), total_values: 1, count: 1 });
        assert_eq!((a.min, a.max, a.sum, a.avg, a.count), (Some(0.0), Some(5.0), Some(8.0), Some(8.0 / 3.0), 3));
        a.merge(&FacetStats::default());
        assert_eq!(a.count, 3);
    }

    #[test]
    fn split_keeps_parentheses_and_brackets() {
        assert_eq!(split_top("a, p(x:[0, 1], y:[1, ]), $b(c, d)"), ["a", " p(x:[0, 1], y:[1, ])", " $b(c, d)"]);
    }
}
