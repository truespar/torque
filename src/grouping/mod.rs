use crate::index::values::{key_f64, BoolIndex, ValueIndex};
use crate::index::Segment;
use crate::schema::{DocId, FieldId, FieldType, Schema};
use crate::search::Hit;
use crate::bitmap::Bitmap;
use rustc_hash::FxHashMap;
use serde::Serialize;
use std::hash::{Hash, Hasher};

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
/// A grouped search response, with Typesense's keys.
#[derive(Serialize)]
pub struct GroupedSearchResult {
    /// Conversational search payload (answer + history), set by the handler.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<serde_json::Value>,
    /// One of several segments: its results, for the facets counted over every segment's at once.
    #[serde(skip)]
    pub docs: Option<Bitmap>,
    pub facet_counts: Vec<crate::index::facet::FacetResult>,
    pub found: usize,
    pub found_docs: usize,
    /// One of several segments: every group it found (`GroupKeys::id`) and its documents, for the
    /// merge to count groups across segments.
    #[serde(skip)]
    pub group_counts: Vec<(u64, usize)>,
    pub grouped_hits: Vec<GroupHit>,
    /// Curation rule metadata echoed when a matched curation item carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
    pub out_of: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    /// One of several segments: the pinned hits it holds, for the merge to place.
    #[serde(skip)]
    pub pinned: Vec<crate::search::PinnedHit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_params: Option<serde_json::Value>,
    pub search_cutoff: bool,
    pub search_time_ms: u64,
}

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
#[derive(Serialize)]
pub struct GroupHit {
    pub found: usize,
    /// The group's values, a missing one left out (Typesense lists the fields its documents have).
    #[serde(serialize_with = "present_values")]
    pub group_key: Vec<serde_json::Value>,
    pub hits: Vec<Hit>,
    /// The group across segments (`GroupKeys::id`).
    #[serde(skip)]
    pub id: u64,
}

fn present_values<S: serde::Serializer>(key: &[serde_json::Value], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(key.iter().filter(|v| !v.is_null()))
}

/// A group field's values, from its value index.
enum KeyField<'a> {
    /// string, int32, int64, float: one ordinal per document
    Single(&'a ValueIndex, FieldType),
    /// string[]: the elements in document order
    Multi(&'a ValueIndex),
    Bool(&'a BoolIndex),
    /// Other types, which form no group values (every document counts as missing).
    None,
}

/// Group keys of a segment's documents. Documents with equal values in every group field share a
/// hash (ordinals stand for the values within the segment); the key values are read once per group.
/// A document missing a value groups with the others missing it (`group_missing_values`, the
/// default), else alone (Typesense: its own group).
pub struct GroupKeys<'a> {
    fields: Vec<KeyField<'a>>,
    missing_values: bool,
    /// The segment's place in the collection, for the groups of one document.
    base: u64,
}

impl<'a> GroupKeys<'a> {
    pub fn new(seg: &'a Segment, schema: &Schema, group_fields: &[FieldId], missing_values: bool) -> Self {
        let fields = group_fields
            .iter()
            .map(|fid| {
                let ty = schema.fields.get(*fid as usize).map(|f| f.field_type.clone());
                let values = seg.values.get(fid).filter(|v| v.doc_ords().is_some());
                match (ty, values) {
                    (Some(FieldType::StringArray | FieldType::StringStar), Some(v)) if v.doc_ords().is_some_and(|o| o.is_multi()) => KeyField::Multi(v),
                    (Some(t @ (FieldType::String | FieldType::Text | FieldType::Int32 | FieldType::Int64 | FieldType::Float64)), Some(v))
                        if !v.doc_ords().is_some_and(|o| o.is_multi()) =>
                    {
                        KeyField::Single(v, t)
                    }
                    (Some(FieldType::Bool), _) => seg.bools.get(fid).map_or(KeyField::None, KeyField::Bool),
                    _ => KeyField::None,
                }
            })
            .collect();
        Self { fields, missing_values, base: u64::from(seg.base_doc_id) }
    }

    /// The group of a document alone, the same in every segment.
    fn alone(&self, doc: DocId) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
        (7u8, self.base + u64::from(doc)).hash(&mut hasher);
        hasher.finish()
    }

    /// Hash of the document's group values within the segment.
    #[inline]
    pub fn hash(&self, doc: DocId) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
        for f in &self.fields {
            let present = match f {
                KeyField::Single(v, t) => v.doc_ord(doc).map(|o| {
                    let tag: u8 = match t {
                        FieldType::Int32 => 1,
                        FieldType::Int64 => 2,
                        FieldType::Float64 => 3,
                        _ => 0,
                    };
                    (tag, o).hash(&mut hasher);
                }),
                KeyField::Bool(b) => b.value(doc).map(|x| (4u8, x).hash(&mut hasher)),
                KeyField::Multi(v) => {
                    let o = v.doc_ords().unwrap();
                    o.present(doc).then(|| {
                        5u8.hash(&mut hasher);
                        o.elems(doc).count().hash(&mut hasher);
                        o.elems(doc).for_each(|e| e.hash(&mut hasher));
                    })
                }
                KeyField::None => None,
            };
            if present.is_none() {
                if !self.missing_values {
                    return self.alone(doc);
                }
                6u8.hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// The document's group across segments: its values' hash, equal for equal values in any
    /// segment (`hash` hashes the segment's ordinals), or its own.
    pub fn id(&self, doc: DocId) -> u64 {
        self.key_id(&self.key(doc), doc)
    }

    /// `id` of a document whose key is read already.
    pub fn key_id(&self, key: &[serde_json::Value], doc: DocId) -> u64 {
        if !self.missing_values && key.iter().any(serde_json::Value::is_null) {
            return self.alone(doc);
        }
        key_hash(key)
    }

    /// Per document its group across segments (`id`), the values read once per group of the segment.
    pub fn ids(&self) -> impl FnMut(DocId) -> u64 + '_ {
        let mut seen: FxHashMap<u64, u64> = FxHashMap::default();
        move |doc| *seen.entry(self.hash(doc)).or_insert_with(|| self.id(doc))
    }

    /// The document's group key values (null for a missing value).
    pub fn key(&self, doc: DocId) -> Vec<serde_json::Value> {
        let mut key = Vec::with_capacity(self.fields.len());
        for f in &self.fields {
            let val = match f {
                KeyField::Single(v, t) => v.doc_ord(doc).and_then(|o| match t {
                    FieldType::Int32 => v.key_at(o).map(|k| serde_json::json!(k as i32)),
                    FieldType::Int64 => v.key_at(o).map(|k| serde_json::json!(k)),
                    FieldType::Float64 => v.key_at(o).map(|k| serde_json::json!(key_f64(k))),
                    _ => v.str_at(o).map(serde_json::Value::String),
                }),
                KeyField::Bool(b) => b.value(doc).map(serde_json::Value::Bool),
                KeyField::Multi(v) => {
                    let o = v.doc_ords().unwrap();
                    o.present(doc).then(|| {
                        let arr: Vec<Option<String>> = o.elems(doc).map(|e| e.and_then(|ord| v.str_at(ord))).collect();
                        serde_json::json!(arr)
                    })
                }
                KeyField::None => None,
            };
            key.push(val.unwrap_or(serde_json::Value::Null));
        }
        key
    }
}

/// Hash of a group key's values (equal keys hash equal in any segment).
pub fn key_hash(key: &[serde_json::Value]) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    for v in key {
        v.to_string().hash(&mut hasher);
    }
    hasher.finish()
}

/// Parse group_by field names and resolve to FieldIds.
/// Fields must have `facet: true`.
pub fn parse_group_by(group_by: &str, schema: &Schema) -> Result<Vec<FieldId>, String> {
    let mut fields = Vec::new();
    for name in group_by.split(',') {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let field_id = schema
            .name_to_id
            .get(name)
            .ok_or_else(|| format!("Group-by field `{name}` not found in schema."))?;
        let field_def = &schema.fields[*field_id as usize];
        if !field_def.facet {
            return Err(format!(
                "Field `{name}` must have faceting enabled for group_by."
            ));
        }
        fields.push(*field_id);
    }
    Ok(fields)
}

/// Lightweight grouping result: stores (DocId, score) pairs instead of full Hits.
/// Used for two-phase grouped search to avoid materializing all candidates.
pub struct GroupedDocIds {
    /// Each entry: (group_key, retained doc_ids+scores, total docs in group)
    pub groups: Vec<(Vec<serde_json::Value>, Vec<(DocId, u64)>, usize)>,
    pub found_docs: usize,
}

/// Bucket for accumulating docs per group. Stores the first doc_id seen
/// (used to extract the group key lazily) plus retained doc_ids and count.
struct GroupBucket {
    first_doc_id: DocId,
    retained: Vec<(DocId, u64)>,
    count: usize,
}

/// Group sorted candidates by DocId without materializing Hits.
/// Phase 1 of two-phase grouped search: only retains first `group_limit` DocIds per group.
pub fn group_doc_ids(sorted_docs: Vec<(DocId, u64)>, keys: &GroupKeys<'_>, group_limit: usize) -> GroupedDocIds {
    let found_docs = sorted_docs.len();

    let mut group_order: Vec<u64> = Vec::new();
    let mut buckets: FxHashMap<u64, GroupBucket> = FxHashMap::default();

    for (doc_id, score) in sorted_docs {
        let h = keys.hash(doc_id);
        let bucket = buckets.entry(h).or_insert_with(|| {
            group_order.push(h);
            GroupBucket {
                first_doc_id: doc_id,
                retained: Vec::new(),
                count: 0,
            }
        });

        bucket.count += 1;
        if bucket.retained.len() < group_limit {
            bucket.retained.push((doc_id, score));
        }
    }

    // Extract group keys only once per unique group
    let groups: Vec<_> = group_order
        .into_iter()
        .filter_map(|h| {
            let bucket = buckets.remove(&h)?;
            Some((keys.key(bucket.first_doc_id), bucket.retained, bucket.count))
        })
        .collect();

    GroupedDocIds {
        groups,
        found_docs,
    }
}

/// Group candidates by score without pre-sorting: one pass keeps each group's `group_limit`
/// best documents (score, then the latest inserted), groups ordered by their best document.
pub fn group_doc_ids_scored(candidates: &Bitmap, scores: &crate::search::text::Scores, keys: &GroupKeys<'_>, group_limit: usize) -> GroupedDocIds {
    let found_docs = candidates.cardinality() as usize;
    let score = |d: DocId| scores.get(d);
    // (score, doc) orders documents best first when compared in reverse.
    let key = |d: DocId| (score(d), d);

    let mut group_order: Vec<u64> = Vec::new();
    let mut buckets: FxHashMap<u64, GroupBucket> = FxHashMap::default();
    for doc_id in candidates.iter() {
        let h = keys.hash(doc_id);
        let bucket = buckets.entry(h).or_insert_with(|| {
            group_order.push(h);
            GroupBucket { first_doc_id: doc_id, retained: Vec::with_capacity(group_limit), count: 0 }
        });
        bucket.count += 1;
        if bucket.retained.len() < group_limit {
            bucket.retained.push((doc_id, score(doc_id)));
        } else if let Some((min_idx, _)) = bucket.retained.iter().enumerate().min_by_key(|(_, e)| (e.1, e.0)) {
            if key(doc_id) > (bucket.retained[min_idx].1, bucket.retained[min_idx].0) {
                bucket.retained[min_idx] = (doc_id, score(doc_id));
            }
        }
    }

    let mut groups: Vec<((u64, DocId), Vec<serde_json::Value>, Vec<(DocId, u64)>, usize)> = group_order
        .into_iter()
        .filter_map(|h| {
            let mut bucket = buckets.remove(&h)?;
            bucket.retained.sort_unstable_by(|a, b| (b.1, b.0).cmp(&(a.1, a.0)));
            let best = bucket.retained.first().map(|&(d, s)| (s, d))?;
            Some((best, keys.key(best.1), bucket.retained, bucket.count))
        })
        .collect();
    groups.sort_unstable_by(|a, b| b.0.cmp(&a.0));

    GroupedDocIds {
        groups: groups.into_iter().map(|(_, k, r, c)| (k, r, c)).collect(),
        found_docs,
    }
}

/// Group candidates by iterating the field's values (~10-100) instead of all candidates (~1M).
/// Only valid for single-field group_by on scalar String/Text fields, which keep one ordinal per
/// document. StringArray uses different grouping semantics (whole array = one group key) vs the
/// values (individual values), so it must fall through to the hash path.
pub fn group_doc_ids_from_values(
    candidates: &Bitmap,
    idx: &ValueIndex,
    group_limit: usize,
    group_missing_values: bool,
) -> GroupedDocIds {
    let found_docs = candidates.cardinality() as usize;
    let mut groups: Vec<(Vec<serde_json::Value>, Vec<(DocId, u64)>, usize)> = Vec::new();
    let mut covered_count: u64 = 0;

    for ord in 0..idx.len() {
        // Count without materializing the intersection; the retained docs
        // (first group_limit members) come from a bounded probe instead of
        // a full AND per group value.
        let count = idx.count_in(ord, candidates) as usize;
        if count == 0 {
            continue;
        }
        // the group's latest documents first (Typesense's order without a sort)
        let mut retained: Vec<(DocId, u64)> = Vec::with_capacity(group_limit.min(count));
        idx.docs(ord).for_each_rev(|d| {
            if candidates.contains(d) {
                retained.push((d, 0));
            }
            retained.len() < group_limit
        });
        covered_count += count as u64;
        let group_key = vec![serde_json::Value::String(idx.str_at(ord).unwrap_or_default())];
        groups.push((group_key, retained, count));
    }

    let missing_count = found_docs.saturating_sub(covered_count as usize);
    if missing_count > 0 && group_missing_values {
        let mut retained: Vec<(DocId, u64)> = Vec::with_capacity(group_limit.min(missing_count));
        candidates.for_each_rev(|d| {
            if idx.doc_ord(d).is_none() {
                retained.push((d, 0));
            }
            retained.len() < group_limit
        });
        groups.push((vec![serde_json::Value::Null], retained, missing_count));
    } else if missing_count > 0 && group_limit > 0 {
        // each document without a value its own group
        candidates.for_each_rev(|d| {
            if idx.doc_ord(d).is_none() {
                groups.push((vec![serde_json::Value::Null], vec![(d, 0)], 1));
            }
            true
        });
    }
    // groups by their latest document
    groups.sort_by_key(|g| std::cmp::Reverse(g.1.first().map(|d| d.0)));

    GroupedDocIds {
        groups,
        found_docs,
    }
}

/// Group candidates directly from a bitmap without allocating an intermediate Vec.
/// Used when there's no sort_by and no text scores (wildcard query + group_by): the latest
/// documents first, groups by their latest document (Typesense's order without a sort).
pub fn group_doc_ids_from_bitmap(candidates: &Bitmap, keys: &GroupKeys<'_>, group_limit: usize) -> GroupedDocIds {
    let found_docs = candidates.cardinality() as usize;

    let mut group_order: Vec<u64> = Vec::new();
    let mut buckets: FxHashMap<u64, GroupBucket> = FxHashMap::default();

    candidates.for_each_rev(|doc_id| {
        let h = keys.hash(doc_id);
        let bucket = buckets.entry(h).or_insert_with(|| {
            group_order.push(h);
            GroupBucket {
                first_doc_id: doc_id,
                retained: Vec::new(),
                count: 0,
            }
        });

        bucket.count += 1;
        if bucket.retained.len() < group_limit {
            bucket.retained.push((doc_id, 0));
        }
        true
    });

    let groups: Vec<_> = group_order
        .into_iter()
        .filter_map(|h| {
            let bucket = buckets.remove(&h)?;
            Some((keys.key(bucket.first_doc_id), bucket.retained, bucket.count))
        })
        .collect();

    GroupedDocIds {
        groups,
        found_docs,
    }
}

/// Group a flat list of hits into GroupHits.
/// `sorted_doc_hits` contains (DocId, Hit) pairs in the desired sort order.
/// Returns groups preserving the order of first appearance.
pub fn group_hits(sorted_doc_hits: Vec<(DocId, Hit)>, keys: &GroupKeys<'_>, group_limit: usize) -> GroupedSearchResult {
    let found_docs = sorted_doc_hits.len();

    struct HitBucket {
        first_doc_id: DocId,
        hits: Vec<Hit>,
        count: usize,
    }

    let mut group_order: Vec<u64> = Vec::new();
    let mut buckets: FxHashMap<u64, HitBucket> = FxHashMap::default();

    for (doc_id, hit) in sorted_doc_hits {
        let h = keys.hash(doc_id);
        let bucket = buckets.entry(h).or_insert_with(|| {
            group_order.push(h);
            HitBucket {
                first_doc_id: doc_id,
                hits: Vec::new(),
                count: 0,
            }
        });

        bucket.count += 1;
        if bucket.hits.len() < group_limit {
            bucket.hits.push(hit);
        }
    }

    let grouped_hits: Vec<GroupHit> = group_order
        .into_iter()
        .filter_map(|h| {
            let bucket = buckets.remove(&h)?;
            Some(GroupHit {
                group_key: keys.key(bucket.first_doc_id),
                hits: bucket.hits,
                found: bucket.count,
                id: keys.id(bucket.first_doc_id),
            })
        })
        .collect();

    let found = grouped_hits.len();

    GroupedSearchResult {
        found,
        out_of: 0,
        page: None,
        offset: None,
        found_docs,
        grouped_hits,
        facet_counts: vec![],
        search_time_ms: 0,
        request_params: None,
        metadata: None,
        conversation: None,
        search_cutoff: false,
        group_counts: Vec::new(),
        pinned: Vec::new(),
        docs: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::values::{f64_key, Ords, ValueCollector};

    fn make_hit(_doc_id: DocId, doc_json: serde_json::Value) -> Hit {
        Hit::new(doc_json)
    }

    fn schema(types: &[FieldType]) -> Schema {
        let fields = types
            .iter()
            .enumerate()
            .map(|(i, t)| crate::schema::FieldDef {
                name: format!("f{i}"),
                field_type: t.clone(),
                facet: true,
                optional: true,
                id: i as FieldId,
                ..Default::default()
            })
            .collect();
        Schema {
            name: "test".into(),
            fields,
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: crate::schema::Language::default(),
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        }
    }

    /// A segment with string field 0 (`None` = missing) and, when given, int32 field 1.
    fn segment(strings: &[Option<&str>], ints: Option<&[i64]>) -> Segment {
        let n = strings.len() as u32;
        let mut c = ValueCollector::<String>::new(n, Ords::Single);
        for (d, v) in strings.iter().enumerate() {
            if let Some(v) = v {
                c.add(d as DocId, v.to_string());
            }
        }
        let mut seg = Segment::empty();
        seg.values.insert(0, ValueIndex::build_strings(c).unwrap());
        if let Some(ints) = ints {
            let mut c = ValueCollector::<i64>::new(n, Ords::Single);
            for (d, v) in ints.iter().enumerate() {
                c.add(d as DocId, *v);
            }
            seg.values.insert(1, ValueIndex::build_numbers(c, false).unwrap());
        }
        seg.all_docs.add_range(0..n);
        seg
    }

    fn hits(n: u32) -> Vec<(DocId, Hit)> {
        (0..n).map(|i| (i, make_hit(i, serde_json::json!({"id": i.to_string()})))).collect()
    }

    #[test]
    fn group_by_single_field() {
        // 4 docs: 2 in "red", 2 in "blue"
        let seg = segment(&[Some("red"), Some("blue"), Some("red"), Some("blue")], None);
        let s = schema(&[FieldType::String]);
        let result = group_hits(hits(4), &GroupKeys::new(&seg, &s, &[0], true), 10);
        assert_eq!(result.found, 2);
        assert_eq!(result.found_docs, 4);
        assert_eq!(result.grouped_hits[0].group_key, vec![serde_json::json!("red")]);
        assert_eq!(result.grouped_hits[0].found, 2);
        assert_eq!(result.grouped_hits[0].hits.len(), 2);
        assert_eq!(result.grouped_hits[1].group_key, vec![serde_json::json!("blue")]);
        assert_eq!(result.grouped_hits[1].found, 2);
    }

    #[test]
    fn group_limit_truncates() {
        let seg = segment(&[Some("a"), Some("a"), Some("a")], None);
        let s = schema(&[FieldType::String]);
        let result = group_hits(hits(3), &GroupKeys::new(&seg, &s, &[0], true), 2);
        assert_eq!(result.grouped_hits[0].found, 3);
        assert_eq!(result.grouped_hits[0].hits.len(), 2); // truncated to limit
    }

    #[test]
    fn group_missing_values() {
        let seg = segment(&[Some("a"), None, None], None);
        let s = schema(&[FieldType::String]);
        let result = group_hits(hits(3), &GroupKeys::new(&seg, &s, &[0], true), 10);
        assert_eq!(result.found, 2); // "a" group + null group
        let null_group = result.grouped_hits.iter().find(|g| g.group_key == vec![serde_json::Value::Null]).unwrap();
        assert_eq!(null_group.found, 2);
        let result = group_hits(hits(3), &GroupKeys::new(&seg, &s, &[0], false), 10);
        assert_eq!(result.found, 3); // "a", and each document without a value alone
        assert_eq!(result.found_docs, 3);
        assert_eq!(serde_json::to_value(&result.grouped_hits[1]).unwrap()["group_key"], serde_json::json!([]));
        assert_ne!(result.grouped_hits[1].id, result.grouped_hits[2].id);
        let alone = group_doc_ids_from_values(&seg.all_docs, &seg.values[&0], 10, false);
        assert_eq!(alone.groups.iter().map(|g| (g.1.clone(), g.2)).collect::<Vec<_>>(), [(vec![(2, 0)], 1), (vec![(1, 0)], 1), (vec![(0, 0)], 1)]);
        // The value walk forms the same groups, the one of the latest document first.
        let by_values = group_doc_ids_from_values(&seg.all_docs, &seg.values[&0], 10, true);
        let keys: Vec<_> = by_values.groups.iter().map(|g| (g.0.clone(), g.2)).collect();
        assert_eq!(keys, [(vec![serde_json::Value::Null], 2), (vec![serde_json::json!("a")], 1)]);
        assert_eq!(by_values.groups[0].1, [(2, 0), (1, 0)]);
    }

    #[test]
    fn group_doc_ids_scored_orders_by_score() {
        // 4 docs in 2 groups, "blue" has better top score
        let seg = segment(&[Some("red"), Some("blue"), Some("red"), Some("blue")], None);
        let s = schema(&[FieldType::String]);
        let scores: crate::search::text::Scores = [(0, 50), (1, 90), (2, 30), (3, 70)].into_iter().collect();
        let result = super::group_doc_ids_scored(&seg.all_docs, &scores, &GroupKeys::new(&seg, &s, &[0], true), 10);
        assert_eq!(result.groups.len(), 2);
        // Blue group (best=0.9) should come before Red group (best=0.5)
        assert_eq!(result.groups[0].0, vec![serde_json::json!("blue")]);
        assert_eq!(result.groups[1].0, vec![serde_json::json!("red")]);
        assert_eq!(result.found_docs, 4);
    }

    #[test]
    fn group_doc_ids_scored_respects_limit() {
        let seg = segment(&[Some("a"), Some("a"), Some("a")], None);
        let s = schema(&[FieldType::String]);
        let scores: crate::search::text::Scores = [(0, 30), (1, 90), (2, 60)].into_iter().collect();
        let result = super::group_doc_ids_scored(&seg.all_docs, &scores, &GroupKeys::new(&seg, &s, &[0], true), 2);
        assert_eq!(result.groups.len(), 1);
        // group_limit=2, so only top 2 by score retained
        assert_eq!(result.groups[0].1.len(), 2);
        assert_eq!(result.groups[0].2, 3); // total count
        // Top 2 by score: doc 1 (0.9), doc 2 (0.6)
        assert_eq!(result.groups[0].1[0].0, 1);
        assert_eq!(result.groups[0].1[1].0, 2);
    }

    #[test]
    fn multi_field_group_by() {
        let seg = segment(&[Some("a"), Some("a"), Some("a")], Some(&[1, 2, 1]));
        let s = schema(&[FieldType::String, FieldType::Int32]);
        let result = group_hits(hits(3), &GroupKeys::new(&seg, &s, &[0, 1], true), 10);
        assert_eq!(result.found, 2); // ("a",1) and ("a",2)
        assert_eq!(result.grouped_hits[0].group_key, vec![serde_json::json!("a"), serde_json::json!(1)]);
    }

    #[test]
    fn array_and_float_group_keys() {
        let mut c = ValueCollector::<String>::new(4, Ords::Multi);
        c.add_many(0, [Some("x".to_string()), None]);
        c.add_many(1, [Some("x".to_string()), None]);
        c.add_many(2, Vec::<Option<String>>::new());
        let mut f = ValueCollector::<i64>::new(4, Ords::Single);
        for d in 0..4 {
            f.add(d, f64_key(2.5));
        }
        let mut seg = Segment::empty();
        seg.values.insert(0, ValueIndex::build_strings(c).unwrap());
        seg.values.insert(1, ValueIndex::build_numbers(f, true).unwrap());
        seg.all_docs.add_range(0..4);
        let s = schema(&[FieldType::StringArray, FieldType::Float64]);
        let keys = GroupKeys::new(&seg, &s, &[0, 1], true);
        // groups by their latest document
        let result = group_doc_ids_from_bitmap(&seg.all_docs, &keys, 10);
        let got: Vec<_> = result.groups.iter().map(|g| (g.0.clone(), g.2)).collect();
        assert_eq!(
            got,
            [
                (vec![serde_json::Value::Null, serde_json::json!(2.5)], 1),
                (vec![serde_json::json!([]), serde_json::json!(2.5)], 1),
                (vec![serde_json::json!(["x", null]), serde_json::json!(2.5)], 2),
            ]
        );
    }
}
