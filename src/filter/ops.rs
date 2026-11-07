use crate::bitmap::Bitmap;
use crate::collection::CollectionView;
use crate::index::geo::{Cap, GeoBox};
use crate::index::reference::ReferenceIndex;
use crate::index::values::{f64_key, ValueIndex};
use crate::index::Segment;
use crate::schema::{DocId, Document, FieldDef, FieldId, FieldType, FieldValue, Language, Schema};
use rustc_hash::FxHashMap;
use std::ops::Bound;
use std::sync::{Arc, Mutex};

use super::{FilterOp, FilterValue, Match, TextFilter};

/// What a search needs to resolve JOINs: the referenced collections, each as of its first use in
/// the search (every segment, with its deletions), and each JOIN filter's
/// referenced documents, found once for all the search's segments.
pub struct JoinContext<'a> {
    pub collections: &'a crate::collection::CollectionRegistry,
    pub aliases: &'a crate::collection::alias::AliasRegistry,
    pub local_collection_name: &'a str,
    targets: Mutex<FxHashMap<String, Option<JoinTarget>>>,
    matches: Mutex<Matches>,
}

/// Each JOIN filter's ids (`JoinContext::matching`), by collection, filter and direction.
type Matches = FxHashMap<(String, String, bool), Arc<Vec<Box<str>>>>;

/// A referenced collection as a search sees it.
#[derive(Clone)]
pub struct JoinTarget {
    /// Its name (an alias resolved).
    pub name: String,
    pub schema: Arc<Schema>,
    pub view: Arc<CollectionView>,
}

impl<'a> JoinContext<'a> {
    pub fn new(
        collections: &'a crate::collection::CollectionRegistry,
        aliases: &'a crate::collection::alias::AliasRegistry,
        local_collection_name: &'a str,
    ) -> Self {
        Self { collections, aliases, local_collection_name, targets: Mutex::default(), matches: Mutex::default() }
    }

    /// The referenced collection `name` (or alias), None when there is none.
    pub fn target(&self, name: &str) -> Option<JoinTarget> {
        let resolved = self.aliases.resolve(name);
        let mut targets = self.targets.lock().unwrap_or_else(|e| e.into_inner());
        targets
            .entry(resolved.clone())
            .or_insert_with(|| {
                self.collections.get(&resolved).map(|c| JoinTarget { name: resolved, schema: Arc::clone(&c.schema), view: Arc::new(c.view()) })
            })
            .clone()
    }

    /// A segment's reference field to the referenced collection (`name` as the search wrote it).
    pub fn references<'s>(seg: &'s Segment, target: &JoinTarget, name: &str) -> Option<&'s ReferenceIndex> {
        seg.reference_indexes.values().find(|ri| ri.target_collection == target.name || ri.target_collection == name)
    }

    /// The ids of the referenced collection's live documents matching `filter`, or (`back`) the
    /// ids of the local documents those reference; once per search.
    fn matching(&self, target: &JoinTarget, filter: &str, back: bool) -> Arc<Vec<Box<str>>> {
        let key = (target.name.clone(), filter.to_string(), back);
        if let Some(ids) = self.matches.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return Arc::clone(ids);
        }
        let ids = Arc::new(self.find_matching(target, filter, back));
        self.matches.lock().unwrap_or_else(|e| e.into_inner()).insert(key, Arc::clone(&ids));
        ids
    }

    fn find_matching(&self, target: &JoinTarget, filter: &str, back: bool) -> Vec<Box<str>> {
        let op = match crate::filter::parser::parse_filter(filter, &target.schema) {
            Ok(Some(op)) => op,
            Ok(None) => return Vec::new(),
            Err(e) => {
                tracing::warn!(collection = target.name.as_str(), error = %e, "Failed to parse JOIN inner filter");
                return Vec::new();
            }
        };
        let mut ids = Vec::new();
        let mut linked = false;
        for (seg, deleted) in target.view.segments() {
            let mut docs = execute_filter(&op, seg, &target.schema);
            if let Some(d) = deleted {
                docs.andnot_inplace(d);
            }
            if !back {
                ids.extend(docs.iter().filter_map(|d| seg.doc_store.external_id(d)).map(String::into_boxed_str));
            } else if let Some(ri) = seg.reference_indexes.values().find(|ri| ri.target_collection == self.local_collection_name) {
                linked = true;
                for d in docs.iter() {
                    ids.extend(ri.forward_lookup(d));
                }
            }
        }
        if back && !linked {
            tracing::warn!(
                local = self.local_collection_name,
                target = target.name.as_str(),
                "No reference field found between collections (checked both directions)"
            );
        }
        ids
    }
}

/// A segment's references to a referenced collection, each id resolved once to the live document
/// it names: its segment in the collection's view and doc id there.
pub struct Referenced<'s> {
    refs: &'s ReferenceIndex,
    view: &'s CollectionView,
    seen: FxHashMap<u64, Option<(u32, DocId)>>,
}

impl<'s> Referenced<'s> {
    pub fn new(refs: &'s ReferenceIndex, view: &'s CollectionView) -> Self {
        Self { refs, view, seen: FxHashMap::default() }
    }

    fn resolve(&mut self, ord: u64) -> Option<(usize, DocId)> {
        let (refs, view) = (self.refs, self.view);
        let found = *self.seen.entry(ord).or_insert_with(|| refs.target_id(ord).and_then(|id| view.lookup(&id)).map(|(s, d)| (s as u32, d)));
        found.map(|(s, d)| (s as usize, d))
    }

    /// The live documents `doc` references, in array order (an id naming none left out).
    pub fn each(&mut self, doc: DocId, out: &mut Vec<(usize, DocId)>) {
        let refs = self.refs;
        for ord in refs.ords(doc) {
            if let Some(t) = self.resolve(ord) {
                out.push(t);
            }
        }
    }

    /// The first live document `doc` references.
    pub fn first(&mut self, doc: DocId) -> Option<(usize, DocId)> {
        let refs = self.refs;
        refs.ords(doc).find_map(|ord| self.resolve(ord))
    }
}

/// Execute a filter with JOIN support. Falls back to regular `execute_filter` for non-join ops.
pub fn execute_filter_with_joins(op: &FilterOp, seg: &Segment, schema: &Schema, join_ctx: &JoinContext<'_>) -> Bitmap {
    match op {
        FilterOp::JoinFilter {
            target_collection,
            inner_filter,
        } => execute_join_filter(target_collection, inner_filter, seg, join_ctx),
        FilterOp::And(ops) => {
            let mut result: Option<Bitmap> = None;
            for child in ops {
                let bm = execute_filter_with_joins(child, seg, schema, join_ctx);
                result = Some(match result {
                    None => bm,
                    Some(mut acc) => { acc &= bm; acc }
                });
            }
            result.unwrap_or_default()
        }
        FilterOp::Or(ops) => {
            let mut result = Bitmap::new();
            for child in ops {
                result |= execute_filter_with_joins(child, seg, schema, join_ctx);
            }
            result
        }
        FilterOp::Not(inner) => &seg.all_docs - execute_filter_with_joins(inner, seg, schema, join_ctx),
        // All non-join ops delegate to the regular execute_filter
        _ => execute_filter(op, seg, schema),
    }
}

/// A JOIN filter on one segment: the segment's documents referencing a referenced document that
/// matches the inner filter, or (the reference the other way) referenced by one.
fn execute_join_filter(target_collection: &str, inner_filter: &str, seg: &Segment, join_ctx: &JoinContext<'_>) -> Bitmap {
    let Some(target) = join_ctx.target(target_collection) else {
        tracing::warn!(collection = target_collection, "JOIN filter references unknown collection");
        return Bitmap::new();
    };
    // the segment references the target's documents
    if let Some(refs) = JoinContext::references(seg, &target, target_collection) {
        let ids = join_ctx.matching(&target, inner_filter, false);
        return refs.reverse_lookup_many(ids.iter().map(|s| &**s));
    }
    // the target's documents reference the local collection's
    let ids = join_ctx.matching(&target, inner_filter, true);
    let mut result = Bitmap::new();
    for id in ids.iter() {
        if let Some(doc) = seg.ext_id_lookup(id) {
            result.add(doc);
        }
    }
    result
}

pub fn execute_filter(op: &FilterOp, seg: &Segment, schema: &Schema) -> Bitmap {
    match op {
        FilterOp::Eq(fid, val) => eq_bitmap(*fid, val, seg, schema),

        FilterOp::Text(fid, filter) => super::text::docs(*fid, filter, seg, schema),

        FilterOp::Neq(fid, val) => &seg.all_docs - eq_bitmap(*fid, val, seg, schema),

        FilterOp::In(fid, vals) => {
            let mut result = Bitmap::new();
            for val in vals {
                result |= eq_bitmap(*fid, val, seg, schema);
            }
            result
        }

        FilterOp::AllOf(fid, vals) => {
            let mut result: Option<Bitmap> = None;
            for val in vals {
                let bm = eq_bitmap(*fid, val, seg, schema);
                result = Some(match result {
                    None => bm,
                    Some(mut acc) => { acc &= bm; acc }
                });
            }
            result.unwrap_or_default()
        }

        FilterOp::Gt(fid, val) => numeric_range(*fid, Bound::Excluded(val), Bound::Unbounded, seg),
        FilterOp::Gte(fid, val) => numeric_range(*fid, Bound::Included(val), Bound::Unbounded, seg),
        FilterOp::Lt(fid, val) => numeric_range(*fid, Bound::Unbounded, Bound::Excluded(val), seg),
        FilterOp::Lte(fid, val) => numeric_range(*fid, Bound::Unbounded, Bound::Included(val), seg),
        FilterOp::Range(fid, min, max) => numeric_range(*fid, Bound::Included(min), Bound::Included(max), seg),

        FilterOp::GeoRadius(fid, lat, lng, radius_m) => {
            seg.geo_indexes
                .get(fid)
                .map(|idx| idx.radius(&Cap::new(*lat, *lng, *radius_m)))
                .unwrap_or_default()
        }

        FilterOp::GeoBoundingBox(fid, min_lat, min_lng, max_lat, max_lng) => {
            let b = GeoBox { min_lat: *min_lat, min_lng: *min_lng, max_lat: *max_lat, max_lng: *max_lng };
            seg.geo_indexes.get(fid).map(|idx| idx.in_box(&b)).unwrap_or_default()
        }

        FilterOp::GeoPolygon(fid, polygon) => seg.geo_indexes.get(fid).map(|idx| idx.in_polygon(polygon)).unwrap_or_default(),

        FilterOp::And(ops) => {
            let mut result: Option<Bitmap> = None;
            for child in ops {
                let bm = execute_filter(child, seg, schema);
                result = Some(match result {
                    None => bm,
                    Some(mut acc) => { acc &= bm; acc }
                });
            }
            result.unwrap_or_default()
        }

        FilterOp::Or(ops) => {
            if ops.len() >= 3 {
                // Parallel evaluation for 3+ OR branches
                use rayon::prelude::*;
                let bitmaps: Vec<Bitmap> = ops
                    .par_iter()
                    .map(|child| execute_filter(child, seg, schema))
                    .collect();
                let mut result = Bitmap::new();
                for bm in bitmaps {
                    result |= bm;
                }
                result
            } else {
                let mut result = Bitmap::new();
                for child in ops {
                    result |= execute_filter(child, seg, schema);
                }
                result
            }
        }

        FilterOp::Not(inner) => &seg.all_docs - execute_filter(inner, seg, schema),

        FilterOp::NestedAnd(conditions) => {
            // Step 1: Compute candidate bitmap by ANDing individual Eq lookups
            let mut candidates: Option<Bitmap> = None;
            for (fid, val) in conditions {
                let bm = eq_bitmap(*fid, val, seg, schema);
                candidates = Some(match candidates {
                    None => bm,
                    Some(mut acc) => { acc &= bm; acc }
                });
            }
            let Some(candidates) = candidates else {
                return Bitmap::new();
            };

            // Step 2: Verify element-level match for each candidate, from the fields' element
            // sequences where it can, else from its record
            let strings: Vec<Option<TextFilter>> = conditions.iter().map(|(fid, val)| exact_string(*fid, val, schema)).collect();
            let mut by_ords = NestedOrds::new(conditions, &strings, seg, schema);
            let mut verified = Bitmap::new();
            for doc_id in candidates.iter() {
                let hit = match by_ords.as_mut().and_then(|o| o.matches(doc_id)) {
                    Some(hit) => hit,
                    None => seg.get_doc(doc_id, schema).is_some_and(|doc| nested_and_matches(&doc, conditions, &strings, schema)),
                };
                if hit {
                    verified.add(doc_id);
                }
            }
            verified
        }

        FilterOp::JoinFilter { .. } => {
            // JoinFilter requires a JoinContext — use execute_filter_with_joins instead.
            // Return empty bitmap to avoid panic.
            Bitmap::new()
        }

        FilterOp::Ids(ids) => ids.iter().filter_map(|id| seg.ext_id_lookup(id)).collect(),
    }
}

/// The `:=` filter of a string value on a string field (None for other values and fields).
fn exact_string(fid: FieldId, val: &FilterValue, schema: &Schema) -> Option<TextFilter> {
    let FilterValue::String(s) = val else { return None };
    let def = schema.fields.get(fid as usize).filter(|f| f.field_type.is_searchable())?;
    TextFilter::new(s, Match::Exact, schema, def)
}

/// `NestedAnd` from the fields' element sequences (DOC_ORDS, read in place), the way
/// `nested_and_matches` decides it from a record: an element matches a condition by its value
/// ordinal, each ordinal decided once. None when a field keeps no sequences or a condition is one the
/// records answer (a value that is not the field's type).
struct NestedOrds<'a> {
    conditions: Vec<OrdCondition<'a>>,
    elems: Vec<Vec<Option<u64>>>,
}

struct OrdCondition<'a> {
    ords: &'a crate::index::values::DocOrds,
    values: &'a ValueIndex,
    want: Want<'a>,
}

enum Want<'a> {
    /// The number's ordinal (None: no document has it).
    Ord(Option<u64>),
    /// A string filter, decided per ordinal (None: it depends on the document's language).
    Text { filter: &'a TextFilter, def: &'a FieldDef, languages: &'a [Language], schema: &'a Schema, seen: FxHashMap<u64, Option<bool>> },
}

impl<'a> NestedOrds<'a> {
    fn new(conditions: &[(FieldId, FilterValue)], strings: &'a [Option<TextFilter>], seg: &'a Segment, schema: &'a Schema) -> Option<Self> {
        let mut out = Vec::with_capacity(conditions.len());
        for ((fid, val), string) in conditions.iter().zip(strings) {
            let def = schema.fields.get(*fid as usize)?;
            let values = seg.values.get(fid)?;
            let ords = values.doc_ords()?;
            let want = match (string, val, &def.field_type) {
                (Some(filter), FilterValue::String(_), FieldType::String | FieldType::StringArray | FieldType::StringStar) if values.is_str() => {
                    // a stemmed field's value can match in one language and not in another
                    let languages: &'a [Language] = if def.stem { &Language::ALL } else { std::slice::from_ref(&schema.default_language) };
                    Want::Text { filter, def, languages, schema, seen: FxHashMap::default() }
                }
                (None, FilterValue::Int(_), FieldType::Int32 | FieldType::Int64 | FieldType::Int32Array | FieldType::Int64Array)
                    if !values.is_str() && !values.is_float() =>
                {
                    Want::Ord(value_ord(values, val))
                }
                _ => return None,
            };
            out.push(OrdCondition { ords, values, want });
        }
        let elems = vec![Vec::new(); out.len()];
        Some(Self { conditions: out, elems })
    }

    /// Whether every condition holds at one element index; None when that depends on the
    /// document's language (its record decides).
    fn matches(&mut self, doc: DocId) -> Option<bool> {
        let Self { conditions, elems } = self;
        for (c, e) in conditions.iter().zip(elems.iter_mut()) {
            e.clear();
            e.extend(c.ords.elems(doc));
        }
        let len = elems.iter().map(Vec::len).min().unwrap_or(0);
        let mut unknown = false;
        for idx in 0..len {
            let mut verdict = Some(true);
            for (c, e) in conditions.iter_mut().zip(elems.iter()) {
                match e[idx].map(|o| c.holds(o)) {
                    Some(Some(true)) => {}
                    Some(None) => verdict = None,
                    // a null element, or a value that does not match
                    _ => {
                        verdict = Some(false);
                        break;
                    }
                }
            }
            match verdict {
                Some(true) => return Some(true),
                None => unknown = true,
                Some(false) => {}
            }
        }
        (!unknown).then_some(false)
    }
}

impl OrdCondition<'_> {
    fn holds(&mut self, ord: u64) -> Option<bool> {
        match &mut self.want {
            Want::Ord(want) => Some(*want == Some(ord)),
            Want::Text { filter, def, languages, schema, seen } => *seen.entry(ord).or_insert_with(|| {
                let value = self.values.str_at(ord)?;
                let mut each = languages.iter().map(|&l| super::text::value_holds(&value, filter, schema, def, l));
                let first = each.next()?;
                each.all(|h| h == first).then_some(first)
            }),
        }
    }
}

/// Check if a document satisfies all nested AND conditions at the same array index (`strings`: each
/// condition's filter when a string field's, whose values compare by their tokens).
fn nested_and_matches(doc: &Document, conditions: &[(FieldId, FilterValue)], strings: &[Option<TextFilter>], schema: &Schema) -> bool {
    let string_is = |c: usize, v: &str| match (&strings[c], &conditions[c]) {
        (Some(f), (fid, _)) => super::text::value_holds(v, f, schema, &schema.fields[*fid as usize], doc.language),
        (None, (_, FilterValue::String(s))) => v == s,
        _ => false,
    };
    // Find the minimum array length across all condition fields
    let mut min_len = usize::MAX;
    for (fid, _) in conditions {
        let len = match doc.fields.get(*fid as usize) {
            Some(FieldValue::StringArray(arr)) => arr.len(),
            Some(FieldValue::Int32Array(arr)) => arr.len(),
            Some(FieldValue::Int64Array(arr)) => arr.len(),
            // Non-array fields: treat as single element
            Some(FieldValue::Null) => return false,
            Some(_) => 1,
            None => return false,
        };
        min_len = min_len.min(len);
    }

    if min_len == 0 || min_len == usize::MAX {
        return false;
    }

    // For each element index, check if ALL conditions match at that index
    for idx in 0..min_len {
        let all_match = conditions.iter().enumerate().all(|(c, (fid, val))| {
            match doc.fields.get(*fid as usize) {
                Some(FieldValue::StringArray(arr)) => {
                    arr.get(idx).and_then(|v| v.as_ref()).is_some_and(|v| string_is(c, v))
                }
                Some(FieldValue::Int32Array(arr)) => {
                    if let FilterValue::Int(i) = val {
                        arr.get(idx).and_then(|v| v.as_ref()).map_or(false, |v| *v as i64 == *i)
                    } else {
                        false
                    }
                }
                Some(FieldValue::Int64Array(arr)) => {
                    if let FilterValue::Int(i) = val {
                        arr.get(idx).and_then(|v| v.as_ref()).map_or(false, |v| *v == *i)
                    } else {
                        false
                    }
                }
                Some(FieldValue::Text(s)) => {
                    // Non-array field: always index 0
                    idx == 0 && string_is(c, s)
                }
                Some(FieldValue::Int32(v)) => {
                    idx == 0 && matches!(val, FilterValue::Int(i) if *v as i64 == *i)
                }
                Some(FieldValue::Int64(v)) => {
                    idx == 0 && matches!(val, FilterValue::Int(i) if *v == *i)
                }
                _ => false,
            }
        });
        if all_match {
            return true;
        }
    }

    false
}

/// Estimate filter cardinality without materializing the full bitmap.
/// Returns `(estimated_matches, is_simple)` where `is_simple` means deferral is safe.
pub fn estimate_filter_cardinality(op: &FilterOp, seg: &Segment, total_docs: u64) -> (u64, bool) {
    match op {
        FilterOp::Eq(fid, val) => (eq_cardinality(*fid, val, seg), true),
        FilterOp::Neq(fid, val) => (total_docs.saturating_sub(eq_cardinality(*fid, val, seg)), true),
        FilterOp::In(fid, vals) => {
            let sum: u64 = vals.iter().map(|v| eq_cardinality(*fid, v, seg)).sum();
            (sum.min(total_docs), true)
        }
        FilterOp::AllOf(fid, vals) => {
            let min = vals.iter().map(|v| eq_cardinality(*fid, v, seg)).min().unwrap_or(0);
            (min, true)
        }
        FilterOp::Gt(fid, _) | FilterOp::Gte(fid, _) | FilterOp::Lt(fid, _) | FilterOp::Lte(fid, _) => {
            // Rough estimate: half the docs
            let total_keys = seg.values.get(fid).map_or(0, ValueIndex::len);
            (total_docs.min(total_keys * total_docs / total_keys.max(1) / 2), true)
        }
        FilterOp::Range(fid, _, _) => {
            let total_keys = seg.values.get(fid).map_or(0, ValueIndex::len);
            (total_docs.min(total_keys * total_docs / total_keys.max(1) / 4), true)
        }
        FilterOp::GeoRadius(..) | FilterOp::GeoBoundingBox(..) | FilterOp::GeoPolygon(..) => {
            (total_docs / 4, true)
        }
        FilterOp::And(ops) => {
            let mut min_est = total_docs;
            let mut all_simple = true;
            for child in ops {
                let (est, simple) = estimate_filter_cardinality(child, seg, total_docs);
                min_est = min_est.min(est);
                all_simple = all_simple && simple;
            }
            (min_est, all_simple)
        }
        FilterOp::Ids(ids) => ((ids.len() as u64).min(total_docs), true),
        FilterOp::Text(..)
        | FilterOp::Or(..) | FilterOp::Not(..) | FilterOp::NestedAnd(..) | FilterOp::JoinFilter { .. } => {
            (total_docs, false)
        }
    }
}

/// The ordinal of an exact filter value in a field's value index.
fn value_ord(v: &ValueIndex, val: &FilterValue) -> Option<u64> {
    match val {
        FilterValue::String(s) => v.str_ord(s),
        FilterValue::Int(i) if v.is_float() => v.key_ord(f64_key(*i as f64)),
        FilterValue::Int(i) => v.key_ord(*i),
        FilterValue::Float(f) if v.is_float() => v.key_ord(f64_key(*f)),
        FilterValue::Float(f) if f.fract() == 0.0 && f.abs() < 9.2e18 => v.key_ord(*f as i64),
        _ => None,
    }
}

fn eq_cardinality(fid: FieldId, val: &FilterValue, seg: &Segment) -> u64 {
    match val {
        FilterValue::Bool(b) => seg.bools.get(&fid).map_or(0, |idx| idx.docs(*b).cardinality()),
        _ => seg.values.get(&fid).and_then(|v| Some(v.df(value_ord(v, val)?))).unwrap_or(0),
    }
}

fn eq_bitmap(fid: FieldId, val: &FilterValue, seg: &Segment, schema: &Schema) -> Bitmap {
    match val {
        FilterValue::Bool(b) => seg.bools.get(&fid).map(|idx| idx.docs(*b).clone()).unwrap_or_default(),
        // a string field's values compare by their tokens
        FilterValue::String(_) if schema.fields.get(fid as usize).is_some_and(|f| f.field_type.is_searchable()) => {
            exact_string(fid, val, schema).map(|f| super::text::docs(fid, &f, seg, schema)).unwrap_or_default()
        }
        _ => seg.values.get(&fid).and_then(|v| Some(v.docs(value_ord(v, val)?))).unwrap_or_default(),
    }
}

/// Evaluate filter for a single doc_id without materializing the full bitmap.
pub fn evaluate_filter_single_doc(op: &FilterOp, doc_id: DocId, seg: &Segment, schema: &Schema) -> bool {
    match op {
        FilterOp::Eq(fid, val) => eq_contains(*fid, val, doc_id, seg, schema),
        FilterOp::Text(fid, filter) => super::text::docs(*fid, filter, seg, schema).contains(doc_id),
        FilterOp::Neq(fid, val) => !eq_contains(*fid, val, doc_id, seg, schema),
        FilterOp::In(fid, vals) => vals.iter().any(|v| eq_contains(*fid, v, doc_id, seg, schema)),
        FilterOp::AllOf(fid, vals) => vals.iter().all(|v| eq_contains(*fid, v, doc_id, seg, schema)),
        FilterOp::Gt(fid, val) => in_range(*fid, Bound::Excluded(val), Bound::Unbounded, doc_id, op, seg, schema),
        FilterOp::Gte(fid, val) => in_range(*fid, Bound::Included(val), Bound::Unbounded, doc_id, op, seg, schema),
        FilterOp::Lt(fid, val) => in_range(*fid, Bound::Unbounded, Bound::Excluded(val), doc_id, op, seg, schema),
        FilterOp::Lte(fid, val) => in_range(*fid, Bound::Unbounded, Bound::Included(val), doc_id, op, seg, schema),
        FilterOp::Range(fid, min, max) => in_range(*fid, Bound::Included(min), Bound::Included(max), doc_id, op, seg, schema),
        FilterOp::GeoRadius(fid, lat, lng, radius_m) => seg
            .geo_indexes
            .get(fid)
            .is_some_and(|idx| idx.doc_in_cap(doc_id, &Cap::new(*lat, *lng, *radius_m))),
        FilterOp::GeoBoundingBox(fid, min_lat, min_lng, max_lat, max_lng) => {
            let b = GeoBox { min_lat: *min_lat, min_lng: *min_lng, max_lat: *max_lat, max_lng: *max_lng };
            seg.geo_indexes.get(fid).is_some_and(|idx| idx.doc_in_box(doc_id, &b))
        }
        FilterOp::GeoPolygon(fid, polygon) => seg.geo_indexes.get(fid).is_some_and(|idx| idx.doc_in_polygon(doc_id, polygon)),
        FilterOp::And(ops) => ops.iter().all(|child| evaluate_filter_single_doc(child, doc_id, seg, schema)),
        FilterOp::Or(ops) => ops.iter().any(|child| evaluate_filter_single_doc(child, doc_id, seg, schema)),
        FilterOp::Not(inner) => !evaluate_filter_single_doc(inner, doc_id, seg, schema),
        FilterOp::NestedAnd(_) => {
            // Fall back to full evaluation for nested AND
            execute_filter(op, seg, schema).contains(doc_id)
        }
        FilterOp::JoinFilter { .. } => {
            // JoinFilter requires JoinContext — cannot evaluate per-doc without it
            false
        }
        FilterOp::Ids(ids) => ids.iter().any(|id| seg.ext_id_lookup(id) == Some(doc_id)),
    }
}

fn eq_contains(fid: FieldId, val: &FilterValue, doc_id: DocId, seg: &Segment, schema: &Schema) -> bool {
    if let FilterValue::Bool(b) = val {
        return seg.bools.get(&fid).is_some_and(|idx| idx.docs(*b).contains(doc_id));
    }
    if matches!(val, FilterValue::String(_)) && schema.fields.get(fid as usize).is_some_and(|f| f.field_type.is_searchable()) {
        return eq_bitmap(fid, val, seg, schema).contains(doc_id);
    }
    let Some(v) = seg.values.get(&fid) else { return false };
    let Some(ord) = value_ord(v, val) else { return false };
    match v.doc_ords() {
        Some(o) => o.elems(doc_id).any(|e| e == Some(ord)),
        None => v.docs(ord).contains(doc_id),
    }
}

/// A range filter for one document: its value from the field's ordinals when it has single-valued
/// ones, else the filter's bitmap.
fn in_range(fid: FieldId, lo: Bound<&FilterValue>, hi: Bound<&FilterValue>, doc_id: DocId, op: &FilterOp, seg: &Segment, schema: &Schema) -> bool {
    match seg.values.get(&fid) {
        Some(v) if v.doc_ords().is_some_and(|o| !o.is_multi()) => match v.doc_ord(doc_id) {
            Some(ord) => key_bounds(v, lo, hi).is_some_and(|(lo, hi)| v.key_ords(lo, hi).contains(&ord)),
            None => false,
        },
        _ => execute_filter(op, seg, schema).contains(doc_id),
    }
}

/// Filter bounds as keys of the field: floats keyed as floats; a float bound on an integer field
/// becomes the nearest integer bound that selects the same integers.
fn key_bound(v: &ValueIndex, b: Bound<&FilterValue>, lower: bool) -> Option<Bound<i64>> {
    use Bound::*;
    let f = match b {
        Unbounded => return Some(Unbounded),
        Included(x) | Excluded(x) => match x {
            FilterValue::Int(i) if v.is_float() => return Some(map_bound(b, f64_key(*i as f64))),
            FilterValue::Int(i) => return Some(map_bound(b, *i)),
            FilterValue::Float(f) if v.is_float() => return Some(map_bound(b, f64_key(*f))),
            FilterValue::Float(f) => *f,
            _ => return None,
        },
    };
    if f.is_nan() {
        return None;
    }
    let exact = f.fract() == 0.0;
    let (rounded, inclusive) = match (lower, exact, matches!(b, Included(_))) {
        (_, true, inc) => (f, inc),
        (true, false, _) => (f.ceil(), true),
        (false, false, _) => (f.floor(), true),
    };
    // Saturating cast: a bound beyond i64 keeps every integer on its side.
    let k = rounded as i64;
    Some(if inclusive { Included(k) } else { Excluded(k) })
}

fn map_bound(b: Bound<&FilterValue>, k: i64) -> Bound<i64> {
    match b {
        Bound::Included(_) => Bound::Included(k),
        Bound::Excluded(_) => Bound::Excluded(k),
        Bound::Unbounded => Bound::Unbounded,
    }
}

fn key_bounds(v: &ValueIndex, lo: Bound<&FilterValue>, hi: Bound<&FilterValue>) -> Option<(Bound<i64>, Bound<i64>)> {
    Some((key_bound(v, lo, true)?, key_bound(v, hi, false)?))
}

/// Documents with a value inside the bounds: the union of the postings of the ordinals in range.
fn numeric_range(fid: FieldId, lo: Bound<&FilterValue>, hi: Bound<&FilterValue>, seg: &Segment) -> Bitmap {
    let Some(v) = seg.values.get(&fid) else {
        return Bitmap::new();
    };
    match key_bounds(v, lo, hi) {
        Some((lo, hi)) => v.union(v.key_ords(lo, hi)),
        None => Bitmap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::values::{BoolIndex, Ords, ValueCollector};

    fn schema() -> Schema {
        Schema {
            name: "test".into(),
            fields: Vec::new(),
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: Vec::new(),
            token_separators: Vec::new(),
            default_language: crate::schema::Language::English,
            persistence_enabled: false,
            index_mode: Default::default(),
            name_to_id: FxHashMap::default(),
            geopoint_field_names: Vec::new(),
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        }
    }

    fn segment(n: u32) -> Segment {
        let mut seg = Segment::empty();
        seg.all_docs.add_range(0..n);
        seg
    }

    fn strings(pairs: &[(&str, &[DocId])], n: u32) -> ValueIndex {
        let mut per_doc: Vec<Vec<Option<String>>> = vec![Vec::new(); n as usize];
        for &(val, docs) in pairs {
            for &d in docs {
                per_doc[d as usize].push(Some(val.to_string()));
            }
        }
        let mut c = ValueCollector::<String>::new(n, Ords::Multi);
        for (d, vals) in per_doc.into_iter().enumerate() {
            if !vals.is_empty() {
                c.add_many(d as DocId, vals);
            }
        }
        ValueIndex::build_strings(c).unwrap()
    }

    fn numbers(pairs: &[(i64, &[DocId])], n: u32) -> ValueIndex {
        let mut per_doc: Vec<Option<i64>> = vec![None; n as usize];
        for &(val, docs) in pairs {
            for &d in docs {
                per_doc[d as usize] = Some(val);
            }
        }
        let mut c = ValueCollector::<i64>::new(n, Ords::Single);
        for (d, v) in per_doc.into_iter().enumerate() {
            if let Some(v) = v {
                c.add(d as DocId, v);
            }
        }
        ValueIndex::build_numbers(c, false).unwrap()
    }

    fn run(seg: &Segment, op: FilterOp) -> Bitmap {
        execute_filter(&op, seg, &schema())
    }

    #[test]
    fn eq_string() {
        let mut seg = segment(4);
        seg.values.insert(0, strings(&[("red", &[0, 2]), ("blue", &[1, 3])], 4));
        let result = run(&seg, FilterOp::Eq(0, FilterValue::String("red".into())));
        assert_eq!(result.to_vec(), [0, 2]);
    }

    #[test]
    fn neq_string() {
        let mut seg = segment(4);
        seg.values.insert(0, strings(&[("red", &[0, 2])], 4));
        let result = run(&seg, FilterOp::Neq(0, FilterValue::String("red".into())));
        assert_eq!(result.to_vec(), [1, 3]);
    }

    #[test]
    fn in_or_semantics() {
        let mut seg = segment(4);
        seg.values.insert(0, strings(&[("red", &[0]), ("blue", &[1]), ("green", &[2])], 4));
        let result = run(&seg, FilterOp::In(0, vec![FilterValue::String("red".into()), FilterValue::String("blue".into())]));
        assert_eq!(result.to_vec(), [0, 1]);
    }

    #[test]
    fn allof_and_semantics() {
        // Doc 0 has both "red" and "blue" tags, doc 1 has only "red"
        let mut seg = segment(4);
        seg.values.insert(0, strings(&[("red", &[0, 1]), ("blue", &[0, 2])], 4));
        let result = run(&seg, FilterOp::AllOf(0, vec![FilterValue::String("red".into()), FilterValue::String("blue".into())]));
        assert_eq!(result.to_vec(), [0]);
    }

    #[test]
    fn numeric_gt_gte_lt_lte() {
        let mut seg = segment(3);
        seg.values.insert(1, numbers(&[(50, &[0]), (100, &[1]), (200, &[2])], 3));
        assert_eq!(run(&seg, FilterOp::Gt(1, FilterValue::Int(50))).to_vec(), [1, 2]);
        assert_eq!(run(&seg, FilterOp::Gte(1, FilterValue::Int(50))).to_vec(), [0, 1, 2]);
        assert_eq!(run(&seg, FilterOp::Lt(1, FilterValue::Int(100))).to_vec(), [0]);
        assert_eq!(run(&seg, FilterOp::Lte(1, FilterValue::Int(100))).to_vec(), [0, 1]);
        // Float bounds on an integer field select the integers on their side.
        assert_eq!(run(&seg, FilterOp::Gt(1, FilterValue::Float(99.5))).to_vec(), [1, 2]);
        assert_eq!(run(&seg, FilterOp::Lte(1, FilterValue::Float(100.5))).to_vec(), [0, 1]);
        assert_eq!(run(&seg, FilterOp::Eq(1, FilterValue::Float(100.0))).to_vec(), [1]);
    }

    /// Regression: a range covering most values must not match documents without the field (the
    /// earlier complement shortcut subtracted from all documents).
    #[test]
    fn numeric_range_excludes_docs_without_the_field() {
        // 20 distinct keys, each with 1 doc; docs 20..25 have no value.
        let pairs: Vec<(i64, Vec<u32>)> = (0..20).map(|i| ((i + 1) * 10, vec![i as u32])).collect();
        let pair_refs: Vec<(i64, &[u32])> = pairs.iter().map(|(k, v)| (*k, v.as_slice())).collect();
        let mut seg = segment(25);
        seg.values.insert(1, numbers(&pair_refs, 25));
        let gte = run(&seg, FilterOp::Gte(1, FilterValue::Int(50)));
        assert_eq!(gte.cardinality(), 16);
        assert!(!gte.contains(3) && gte.contains(4) && !gte.contains(22));
        assert_eq!(run(&seg, FilterOp::Gt(1, FilterValue::Int(50))).cardinality(), 15);
        assert_eq!(run(&seg, FilterOp::Lte(1, FilterValue::Int(150))).cardinality(), 15);
        assert_eq!(run(&seg, FilterOp::Lt(1, FilterValue::Int(150))).cardinality(), 14);
        for (op, n) in [(FilterOp::Gte(1, FilterValue::Int(50)), 16), (FilterOp::Lt(1, FilterValue::Int(150)), 14)] {
            let per_doc = (0..25).filter(|&d| evaluate_filter_single_doc(&op, d, &seg, &schema())).count();
            assert_eq!(per_doc, n);
        }
    }

    #[test]
    fn numeric_range() {
        let mut seg = segment(4);
        seg.values.insert(1, numbers(&[(50, &[0]), (100, &[1]), (200, &[2]), (300, &[3])], 4));
        let result = run(&seg, FilterOp::Range(1, FilterValue::Int(100), FilterValue::Int(200)));
        assert_eq!(result.to_vec(), [1, 2]);
    }

    #[test]
    fn float_field_accepts_integer_filter_values() {
        let mut c = ValueCollector::<i64>::new(3, Ords::Single);
        c.add(0, f64_key(1000.0));
        c.add(1, f64_key(1000.5));
        c.add(2, f64_key(-3.25));
        let mut seg = segment(3);
        seg.values.insert(1, ValueIndex::build_numbers(c, true).unwrap());
        assert_eq!(run(&seg, FilterOp::Eq(1, FilterValue::Int(1000))).to_vec(), [0]);
        assert_eq!(run(&seg, FilterOp::Gt(1, FilterValue::Int(1000))).to_vec(), [1]);
        assert_eq!(run(&seg, FilterOp::Lt(1, FilterValue::Int(0))).to_vec(), [2]);
    }

    #[test]
    fn bool_filter() {
        let mut seg = segment(4);
        seg.bools.insert(2, BoolIndex::build(&[0, 2], &[1, 3]).unwrap());
        let result = run(&seg, FilterOp::Eq(2, FilterValue::Bool(true)));
        assert_eq!(result.to_vec(), [0, 2]);
    }

    #[test]
    fn and_or_not_composition() {
        let mut seg = segment(4);
        seg.values.insert(0, strings(&[("red", &[0, 1, 2]), ("blue", &[3])], 4));
        seg.values.insert(1, numbers(&[(100, &[0]), (200, &[1, 2])], 4));
        let and = run(&seg, FilterOp::And(vec![
            FilterOp::Eq(0, FilterValue::String("red".into())),
            FilterOp::Gt(1, FilterValue::Int(100)),
        ]));
        assert_eq!(and.to_vec(), [1, 2]);
        let or = run(&seg, FilterOp::Or(vec![
            FilterOp::Eq(0, FilterValue::String("blue".into())),
            FilterOp::Eq(1, FilterValue::Int(100)),
        ]));
        assert_eq!(or.to_vec(), [0, 3]);
        let not = run(&seg, FilterOp::Not(Box::new(FilterOp::Eq(0, FilterValue::String("red".into())))));
        assert_eq!(not.to_vec(), [3]);
    }

    #[test]
    fn missing_value_returns_empty() {
        let mut seg = segment(2);
        seg.values.insert(0, strings(&[("red", &[0])], 2));
        assert!(run(&seg, FilterOp::Eq(0, FilterValue::String("nonexistent".into()))).is_empty());
    }

    #[test]
    fn single_doc_evaluation_matches_bitmaps() {
        let mut seg = segment(6);
        seg.values.insert(0, strings(&[("Steve Jobs", &[0, 3]), ("Bill", &[1]), ("steve", &[4])], 6));
        seg.values.insert(1, numbers(&[(5, &[0, 1]), (7, &[2, 3])], 6));
        let ops = [
            FilterOp::Eq(0, FilterValue::String("Bill".into())),
            FilterOp::Neq(1, FilterValue::Int(5)),
            FilterOp::Range(1, FilterValue::Int(6), FilterValue::Int(9)),
            FilterOp::Not(Box::new(FilterOp::In(1, vec![FilterValue::Int(7)]))),
        ];
        for op in ops {
            let bm = run(&seg, op.clone());
            for d in 0..6 {
                assert_eq!(evaluate_filter_single_doc(&op, d, &seg, &schema()), bm.contains(d), "{op:?} doc {d}");
            }
        }
    }

    #[test]
    fn nested_and_element_level() {
        use crate::schema::{FieldDef, FieldType};

        // Doc 0: colors=["Black","White"], sizes=["M","L"]    -> Black+M at idx 0 -> match
        // Doc 1: colors=["Black","White"], sizes=["L","M"]    -> Black+L at 0, White+M at 1 -> no match
        // Doc 2: colors=["Red","Black"],   sizes=["S","M"]    -> Black+M at idx 1 -> match
        let mut schema = schema();
        for name in ["color", "size"] {
            schema.fields.push(FieldDef { name: name.into(), field_type: FieldType::StringArray, facet: true, ..Default::default() });
        }
        schema.assign_field_ids();
        let docs = [
            serde_json::json!({"id": "0", "color": ["Black", "White"], "size": ["M", "L"]}),
            serde_json::json!({"id": "1", "color": ["Black", "White"], "size": ["L", "M"]}),
            serde_json::json!({"id": "2", "color": ["Red", "Black"], "size": ["S", "M"]}),
        ]
        .iter()
        .enumerate()
        .map(|(i, j)| crate::ingest::parse_json_document(j, &schema, i as DocId).unwrap())
        .collect();
        let seg = crate::ingest::build_index(&schema, docs);
        let (color, size) = (schema.name_to_id["color"], schema.name_to_id["size"]);

        // NestedAnd: color=Black AND size=M on same element
        let nested = FilterOp::NestedAnd(vec![
            (color, FilterValue::String("Black".into())),
            (size, FilterValue::String("M".into())),
        ]);
        assert_eq!(execute_filter(&nested, &seg, &schema).to_vec(), [0, 2]);

        // Compare with cross-element AND (would match all 3)
        let cross = FilterOp::And(vec![
            FilterOp::Eq(color, FilterValue::String("Black".into())),
            FilterOp::Eq(size, FilterValue::String("M".into())),
        ]);
        assert_eq!(execute_filter(&cross, &seg, &schema).cardinality(), 3, "Cross-element AND matches all 3 (false positive for doc 1)");
    }

    #[test]
    fn nested_and_by_element_sequences_is_nested_and_by_records() {
        use crate::schema::{FieldDef, FieldType};
        let field = |name: &str, field_type, facet, stem| FieldDef { name: name.into(), field_type, facet, stem, ..Default::default() };
        let mut schema = schema();
        schema.fields.extend([
            field("color", FieldType::StringArray, true, true),
            field("size", FieldType::StringArray, true, false),
            field("qty", FieldType::Int32Array, true, false),
            field("note", FieldType::StringArray, false, false),
            field("kind", FieldType::String, true, true),
        ]);
        schema.assign_field_ids();
        let colors = [Some("Black"), Some("Toys"), Some("Toy"), Some("red toys"), None];
        let sizes = [Some("M"), Some("S"), None];
        let mut rng = 7u64;
        let mut next = |n: usize| {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) as usize % n
        };
        let docs: Vec<Document> = (0..400)
            .map(|i| {
                let len = next(4);
                let mut json = serde_json::json!({
                    "id": i.to_string(),
                    "color": (0..len).map(|_| colors[next(colors.len())]).collect::<Vec<_>>(),
                    "size": (0..len + next(2)).map(|_| sizes[next(sizes.len())]).collect::<Vec<_>>(),
                    "qty": (0..len).map(|_| next(4) as i64).collect::<Vec<_>>(),
                    "note": (0..len).map(|_| if next(2) == 0 { "x" } else { "y" }).collect::<Vec<_>>(),
                    "kind": if next(2) == 0 { "Toys" } else { "Black" },
                });
                if let Some(l) = [Some("en"), Some("sv"), None][next(3)] {
                    json["language"] = l.into();
                }
                crate::ingest::parse_json_document(&json, &schema, i as DocId).unwrap()
            })
            .collect();
        let seg = crate::ingest::build_index(&schema, docs);
        let id = |name: &str| schema.name_to_id[name];
        let s = |v: &str| FilterValue::String(v.into());
        let cases = [
            (vec![(id("color"), s("Toys")), (id("size"), s("M"))], true),
            (vec![(id("color"), s("Black")), (id("qty"), FilterValue::Int(2))], true),
            (vec![(id("kind"), s("Toys")), (id("color"), s("red toys"))], true),
            // "Toy" is "Toys" in English, not in Swedish: those elements go to the records
            (vec![(id("color"), s("Toy")), (id("size"), s("S"))], true),
            // no element sequences: the records answer
            (vec![(id("note"), s("x")), (id("size"), s("M"))], false),
        ];
        for (conditions, by_ords) in cases {
            let strings: Vec<Option<TextFilter>> = conditions.iter().map(|(fid, val)| exact_string(*fid, val, &schema)).collect();
            assert_eq!(NestedOrds::new(&conditions, &strings, &seg, &schema).is_some(), by_ords, "{conditions:?}");
            let by_records: Vec<DocId> = (0..400)
                .filter(|&d| seg.get_doc(d, &schema).is_some_and(|doc| nested_and_matches(&doc, &conditions, &strings, &schema)))
                .collect();
            assert!(!by_records.is_empty(), "{conditions:?}");
            assert_eq!(execute_filter(&FilterOp::NestedAnd(conditions.clone()), &seg, &schema).to_vec(), by_records, "{conditions:?}");
        }
    }

    #[test]
    fn ids_select_documents_by_id() {
        use crate::schema::{FieldDef, FieldType};
        let mut schema = schema();
        schema.fields.push(FieldDef { name: "title".into(), field_type: FieldType::String, ..Default::default() });
        schema.assign_field_ids();
        let docs = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(i, id)| crate::ingest::parse_json_document(&serde_json::json!({"id": id, "title": id}), &schema, i as DocId).unwrap())
            .collect();
        let seg = crate::ingest::build_index(&schema, docs);
        let ids = |v: &[&str]| FilterOp::Ids(v.iter().map(|s| s.to_string()).collect());
        // unknown ids match nothing, repeated ones once
        assert_eq!(execute_filter(&ids(&["c", "a", "zz", "a"]), &seg, &schema).to_vec(), [0, 2]);
        assert_eq!(execute_filter(&FilterOp::Not(Box::new(ids(&["b"]))), &seg, &schema).to_vec(), [0, 2]);
        assert!(execute_filter(&ids(&[]), &seg, &schema).is_empty());
        assert!(evaluate_filter_single_doc(&ids(&["b"]), 1, &seg, &schema));
        assert!(!evaluate_filter_single_doc(&ids(&["b"]), 0, &seg, &schema));
    }
}
