pub mod collector;
pub mod facet_query;
pub mod highlight;
pub mod name_score;
pub mod projection;
pub mod query;
pub mod scoring;
pub mod text;

use crate::decay::DecaySort;
use crate::filter::ops::{execute_filter, execute_filter_with_joins, JoinContext, Referenced};
use crate::filter::parser::parse_filter;
use crate::grouping::{self, GroupedSearchResult};
use crate::index::facet::{self, FacetResult};
use crate::index::inverted::InvertedIndex;
use crate::index::sort::{Missing, SortColumn};
use crate::index::{document_to_json, Index, IndexBundle, Segment};
use crate::schema::{DocId, FieldId, FieldType, Schema};
use collector::TopKCollector;
use highlight::{self as hl, Highlight};
use crate::bitmap::Bitmap;
use rustc_hash::FxHashMap;
use serde::Serialize;
use std::cmp::Ordering;
use std::time::Instant;
use scoring::{MatchType, TextMatchInfo};
use text::{Infix, NameMatching, Scores};

/// An f32 as a u64 that orders like it (sign flipped, negatives complemented).
#[inline]
pub(crate) fn float_key(f: f32) -> u64 {
    let b = f.to_bits();
    u64::from(if b >> 31 == 1 { !b } else { b | 0x8000_0000 })
}

/// A candidate's score; 0 when it has none.
#[inline]
fn score_of(scores: &Scores, doc: DocId) -> u64 {
    scores.get(doc)
}

/// The text matches in rank order (score, then the latest document), for fusion with vectors.
fn lexical_ranking(scores: &Scores) -> Vec<(DocId, f32)> {
    let mut v: Vec<(DocId, u64)> = scores.iter().collect();
    v.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    v.into_iter().map(|(d, s)| (d, s as f32)).collect()
}

/// What a text search leaves for the hits.
#[derive(Clone)]
struct TextOutcome {
    /// The words of the query (`text_match_info.num_tokens_dropped` counts against them).
    words: usize,
    marks: std::sync::Arc<text::Marks>,
}

/// A searched field.
struct SearchField {
    fid: FieldId,
    weight: u32,
    num_typos: u32,
    prefix: bool,
    infix: Infix,
    names: NameMatching,
}

/// The search fields: query_by entries expanded (`expand_query_by`), in that order. Given weights
/// (one per query_by entry, or the inline `field:N`) are used as they are, up to the largest; without
/// any, earlier fields weigh more.
fn resolve_search_fields(params: &SearchParams, schema: &Schema) -> Result<Vec<SearchField>, String> {
    let raw_weights: Vec<u32> = match params.query_by_weights.as_deref().map(str::trim) {
        None | Some("") => Vec::new(),
        Some(w) => w
            .split(',')
            .map(|x| x.trim().parse::<f32>().map(|v| v.max(0.0) as u32).map_err(|_| format!("Parameter `query_by_weights` must be an unsigned integer list: `{w}`.")))
            .collect::<Result<_, _>>()?,
    };
    if !raw_weights.is_empty() && raw_weights.len() != params.query_by.len() {
        return Err("Number of weights in `query_by_weights` does not match number of `query_by` fields.".into());
    }
    let lenient = params.validate_field_names == Some(false);
    // `num_typos` and `prefix`: one value per query_by entry, or one for all
    let num_typos = num_typos_list(params)?;
    if !params.query_by.is_empty() && num_typos.len() != params.query_by.len() && num_typos.len() != 1 {
        return Err("Number of values in `num_typos` does not match number of `query_by` fields.".into());
    }
    let prefixes = prefix_list(params);
    if !params.query_by.is_empty() && prefixes.len() != params.query_by.len() && prefixes.len() != 1 {
        return Err("Number of prefix values in `prefix` does not match number of `query_by` fields.".into());
    }
    // `infix`: one value per query_by entry, or one for all; unknown values are skipped
    let infixes: Vec<Infix> = match params.infix.as_deref() {
        Some(v) => v.split(',').filter_map(Infix::parse).collect(),
        None => vec![Infix::Off],
    };
    if !params.query_by.is_empty() && infixes.len() != params.query_by.len() && infixes.len() != 1 {
        return Err("Number of infix values in `infix` does not match number of `query_by` fields.".into());
    }
    // `name_matching`: one value per query_by entry, or one for all (default: fallback in a
    // name-matching field)
    let name_matching: Vec<NameMatching> = match params.name_matching.as_deref() {
        Some(v) => v
            .split(',')
            .map(|m| NameMatching::parse(m.trim()).ok_or_else(|| format!("Invalid `name_matching` value `{m}`: off, fallback or always.")))
            .collect::<Result<_, _>>()?,
        None => Vec::new(),
    };
    if !params.query_by.is_empty() && name_matching.len() > 1 && name_matching.len() != params.query_by.len() {
        return Err("Number of values in `name_matching` does not match number of `query_by` fields.".into());
    }
    let mut weights_given = !raw_weights.is_empty();
    let mut expanded: Vec<(FieldId, u32, PerField)> = Vec::new();
    for (i, entry) in params.query_by.iter().enumerate() {
        let per_field = PerField {
            infix: infixes.get(i).copied().unwrap_or(infixes[0]),
            num_typos: num_typos.get(i).copied().unwrap_or(num_typos[0]),
            prefix: prefixes.get(i).copied().unwrap_or(prefixes[0]),
            names: name_matching.get(i).or(name_matching.first()).copied(),
        };
        let (name, inline) = parse_query_by_entry(entry);
        // an embedding field is searched through its vector (the query's embedding)
        if schema.name_to_id.get(name).is_some_and(|&f| matches!(schema.fields[f as usize].field_type, FieldType::FloatArray { num_dim } if num_dim > 0)) {
            continue;
        }
        let weight = match raw_weights.get(i) {
            Some(&w) => w,
            None if inline != 1.0 => {
                weights_given = true;
                inline.max(0.0) as u32
            }
            None => 1,
        };
        match expand_query_by(name, schema) {
            Ok(ids) => {
                // asked of a field without the schema option: refused, so no one believes they screen by name
                if let Some(f) = ids.iter().find(|&&f| !schema.fields[f as usize].name_matching)
                    && per_field.names.is_some_and(|m| m != NameMatching::Off)
                {
                    return Err(format!("`name_matching` needs a field with `name_matching: true` in the schema; `{}` has none.", schema.fields[*f as usize].name));
                }
                expanded.extend(ids.into_iter().map(|f| (f, weight, per_field)))
            }
            Err(ExpandError::NotFound(_)) if lenient => continue,
            Err(ExpandError::NotFound(e) | ExpandError::Invalid(e)) => return Err(e),
        }
    }
    if lenient && expanded.is_empty() && !params.query_by.is_empty() {
        return Err("None of the `query_by` fields exist in the schema.".into());
    }
    let n = expanded.len() as u32;
    Ok(expanded
        .into_iter()
        .enumerate()
        .map(|(i, (fid, weight, f))| SearchField {
            fid,
            weight: if weights_given { weight } else { n - i as u32 }.min(text::FIELD_MAX_WEIGHT),
            num_typos: f.num_typos,
            prefix: f.prefix,
            infix: f.infix,
            names: match f.names {
                Some(m) => m,
                None if schema.fields[fid as usize].name_matching => NameMatching::Fallback,
                None => NameMatching::Off,
            },
        })
        .collect())
}

/// What a query_by entry gives each of its fields besides the weight.
#[derive(Clone, Copy)]
struct PerField {
    infix: Infix,
    num_typos: u32,
    prefix: bool,
    /// None: the field's default.
    names: Option<NameMatching>,
}

/// `num_typos`: comma-separated typo limits (default 2; above 2 counts as 2).
fn num_typos_list(params: &SearchParams) -> Result<Vec<u32>, String> {
    match params.num_typos.as_deref() {
        None => Ok(vec![2]),
        Some(v) => v
            .split(',')
            .map(|x| x.trim().parse::<u32>().map(|n| n.min(2)).map_err(|_| "Parameter `num_typos` is malformed.".to_string()))
            .collect(),
    }
}

/// `prefix`: comma-separated booleans (default true); a value other than `true` is false.
fn prefix_list(params: &SearchParams) -> Vec<bool> {
    match params.prefix.as_deref() {
        None => vec![true],
        Some(v) => v.split(',').map(|x| x.trim() == "true").collect(),
    }
}

/// The text match parameters of a search.
fn text_params(params: &SearchParams, match_type: MatchType, deadline: text::Deadline) -> Result<text::Params, String> {
    let exhaustive = params.exhaustive_search.unwrap_or(false);
    let max_candidates = match params.max_candidates {
        Some(m) if m > 0 => m,
        _ if exhaustive => 10_000,
        _ => 4,
    };
    let split_join = match params.split_join_tokens.as_deref() {
        Some("off") | Some("false") => text::SplitJoin::Off,
        Some("always") => text::SplitJoin::Always,
        _ => text::SplitJoin::Fallback,
    };
    Ok(text::Params {
        typo_tokens_threshold: params.typo_tokens_threshold.unwrap_or(1) as u64,
        drop_tokens_threshold: params.drop_tokens_threshold.unwrap_or(1) as u64,
        drop_mode: text::DropMode::parse(params.drop_tokens_mode.as_deref().unwrap_or("right_to_left"))?,
        split_join,
        max_candidates,
        min_len_1typo: params.min_len_1typo.unwrap_or(4),
        min_len_2typo: params.min_len_2typo.unwrap_or(7),
        exhaustive,
        match_type,
        prioritize_exact_match: params.prioritize_exact_match.unwrap_or(true),
        prioritize_num_matching_fields: params.prioritize_num_matching_fields.unwrap_or(true),
        typos_for_numerical: params.enable_typos_for_numerical_tokens.unwrap_or(true),
        typos_for_alpha_numerical: params.enable_typos_for_alpha_numerical_tokens.unwrap_or(true),
        deadline,
        max_extra_prefix: params.max_extra_prefix.unwrap_or(usize::MAX),
        max_extra_suffix: params.max_extra_suffix.unwrap_or(usize::MAX),
    })
}

/// The answer when the search cutoff passed before anything was found (HTTP 408).
pub const REQUEST_TIMEOUT: &str = "Request Timeout";

/// `search_cutoff_ms` (default 30 seconds) from the search's start.
fn deadline(params: &SearchParams, start: Instant) -> text::Deadline {
    text::Deadline::new(start, params.search_cutoff_ms.map_or(30_000, |m| m as u64))
}

/// A search cut by its deadline with nothing found is an error, so the client can retry.
fn timed_out(r: Result<(SearchResponse, SearchTimings), String>) -> Result<(SearchResponse, SearchTimings), String> {
    let (cut, found) = match &r {
        Ok((SearchResponse::Ungrouped(u), _)) => (u.search_cutoff, u.found),
        Ok((SearchResponse::Grouped(g), _)) => (g.search_cutoff, g.found),
        Err(_) => return r,
    };
    if cut && found == 0 {
        return Err(REQUEST_TIMEOUT.into());
    }
    r
}

pub struct SearchTimings {
    pub filter_ms: f64,
    pub text_ms: f64,
    pub vector_ms: f64,
    pub facet_ms: f64,
    pub sort_ms: f64,
    pub deferred_filter_ms: f64,
}

#[derive(Clone, Default)]
pub struct SearchParams {
    pub q: String,
    pub query_by: Vec<String>,
    pub filter_by: Option<String>,
    pub sort_by: Option<String>,
    pub facet_by: Option<String>,
    pub per_page: usize,
    pub page: usize,
    /// The first hit to return, when the request gave `offset` and no `page` (Typesense then
    /// answers `offset` in place of `page`).
    pub offset: Option<usize>,
    pub highlight_fields: Option<String>,
    pub group_by: Option<String>,
    pub group_limit: Option<usize>,
    pub group_missing_values: Option<bool>,
    pub text_match_type: Option<String>,
    pub stopwords: Option<String>,
    pub synonym_sets: Option<String>,
    pub preset: Option<String>,
    /// Typo limits, one per query_by entry or one for all ("2,1").
    pub num_typos: Option<String>,
    pub typo_tokens_threshold: Option<usize>,
    /// Prefix search, one per query_by entry or one for all ("true,false").
    pub prefix: Option<String>,
    pub vector_query: Option<String>,
    pub max_facet_values: Option<usize>,
    pub facet_query: Option<String>,
    /// The typos a facet_query word may have (default 2).
    pub facet_query_num_typos: Option<usize>,
    pub highlight_start_tag: Option<String>,
    pub highlight_end_tag: Option<String>,
    pub highlight_affix_num_tokens: Option<usize>,
    pub query_by_weights: Option<String>,
    pub pinned_hits: Option<String>,
    /// Apply the search's filter conditions to curation-pinned documents
    /// (Typesense `filter_curated_hits`). Default false: pins bypass filters.
    pub filter_curated_hits: bool,
    pub hidden_hits: Option<String>,
    pub limit_hits: Option<usize>,
    pub min_len_1typo: Option<usize>,
    pub min_len_2typo: Option<usize>,
    pub drop_tokens_threshold: Option<usize>,
    pub drop_tokens_mode: Option<String>,
    pub split_join_tokens: Option<String>,
    pub enable_typos_for_numerical_tokens: Option<bool>,
    pub enable_typos_for_alpha_numerical_tokens: Option<bool>,
    pub synonym_num_typos: Option<usize>,
    pub synonym_prefix: Option<bool>,
    /// Torque extension: `text_match_info.matches`, how each query word matched.
    pub explain_matches: Option<bool>,
    /// Torque extension: off, fallback or always per query_by field (name-matching fields only).
    pub name_matching: Option<String>,
    /// Torque extension: the first `name_score_candidates` hits (default 100) scored by name and
    /// ordered by the score (`name_score`).
    pub name_score: Option<bool>,
    pub name_score_candidates: Option<usize>,
    /// What the query names, `person` or `organization`: one part of a person's name identifies no one.
    pub name_score_kind: Option<String>,
    pub highlight_full_fields: Option<String>,
    /// false: hits carry no `highlights` (Typesense's first format), only `highlight`.
    pub enable_highlight_v1: Option<bool>,
    pub snippet_threshold: Option<usize>,
    pub exhaustive_search: Option<bool>,
    pub search_cutoff_ms: Option<usize>,
    pub max_candidates: Option<usize>,
    pub prioritize_exact_match: Option<bool>,
    pub prioritize_token_position: Option<bool>,
    pub prioritize_num_matching_fields: Option<bool>,
    pub facet_sample_percent: Option<usize>,
    pub facet_sample_threshold: Option<usize>,
    pub facet_return_parent: Option<String>,
    pub validate_field_names: Option<bool>,
    pub enable_synonyms: Option<bool>,
    pub infix: Option<String>,
    pub max_extra_prefix: Option<usize>,
    pub max_extra_suffix: Option<usize>,
    /// include_fields and exclude_fields (with a scoped key's): what hits return, and without
    /// highlight_fields which query_by fields are highlighted (Typesense's implicit highlighting).
    pub projection: projection::Projection,
    /// One search of a multi_search's union (set by the union, not a request parameter).
    pub union: Option<UnionPart>,
}

impl SearchParams {
    /// The index of the first hit (or group) of the page.
    pub fn start(&self) -> usize {
        self.offset.unwrap_or_else(|| self.page.max(1).saturating_sub(1) * self.per_page)
    }

    /// The response's `page` and `offset`: one of them, as the request paged.
    fn page_or_offset(&self) -> (Option<usize>, Option<usize>) {
        match self.offset {
            Some(o) => (None, Some(o)),
            None => (Some(self.page.max(1)), None),
        }
    }
}

/// Parsed vector query parameters.
pub struct VectorQueryParams {
    pub field_name: String,
    pub field_id: FieldId,
    pub query_vector: Vec<f32>,
    pub k: usize,
    /// Maximum distance threshold — results beyond this are excluded.
    pub distance_threshold: Option<f32>,
    /// HNSW search beam width (higher = better recall, slower). Default: max(k, 100).
    pub ef: Option<usize>,
    /// When filtered result set is smaller than this, use brute-force instead of HNSW.
    pub flat_search_cutoff: Option<usize>,
    /// Document ID for "find similar" queries — vector is looked up from the index.
    pub doc_id: Option<String>,
    /// Hybrid search weight: 0.0 = pure text, 1.0 = pure vector. When set, uses
    /// score interpolation instead of RRF.
    pub alpha: Option<f32>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum SearchResponse {
    Ungrouped(SearchResult),
    Grouped(GroupedSearchResult),
}

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
/// A search response, with Typesense's keys.
#[derive(Serialize)]
pub struct SearchResult {
    /// Conversational search payload (answer + history), set by the handler.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<serde_json::Value>,
    /// One of several segments: its results, for the facets counted over every segment's at once.
    #[serde(skip)]
    pub docs: Option<Bitmap>,
    pub facet_counts: Vec<FacetResult>,
    pub found: usize,
    pub hits: Vec<Hit>,
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
    pub pinned: Vec<PinnedHit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_params: Option<serde_json::Value>,
    pub search_cutoff: bool,
    pub search_time_ms: u64,
    /// A search of a union removing duplicates (`UnionPart::Distinct`): its results per segment.
    #[serde(skip)]
    pub segment_docs: Vec<Bitmap>,
}

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
#[derive(Serialize)]
pub struct HybridSearchInfo {
    pub rank_fusion_score: f32,
}

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
#[derive(Serialize)]
pub struct Hit {
    /// A pinned (curated) hit; it has no text match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curated: Option<bool>,
    pub document: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo_distance_meters: Option<serde_json::Map<String, serde_json::Value>>,
    /// Highlighted fields by name (`{}` when none, as for `q=*`).
    pub highlight: serde_json::Value,
    /// Typesense's first highlight format (absent with `enable_highlight_v1=false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlights: Option<Vec<Highlight>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hybrid_search_info: Option<HybridSearchInfo>,
    /// With `name_score=true`, how well its names match the query's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_score: Option<name_score::NameScore>,
    /// The document's place in the collection (segment base + doc id), which locates it.
    #[serde(skip)]
    pub seq: u64,
    /// The document's `_seq_id` (`Segment::seq_id`): ties rank the later first.
    #[serde(skip)]
    pub seq_id: u64,
    /// The keys its segment sorted it by, comparable across segments (when a search spans several).
    #[serde(skip)]
    pub sort_key: Vec<SortValue>,
    /// The text match, for a text query (Typesense reports it only then).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_match: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_match_info: Option<TextMatchInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_distance: Option<f32>,
}

impl Hit {
    /// A hit of `document` with nothing else.
    pub fn new(document: serde_json::Value) -> Self {
        Hit {
            seq: 0,
            seq_id: 0,
            sort_key: Vec::new(),
            document,
            curated: None,
            text_match: None,
            highlights: Some(Vec::new()),
            highlight: serde_json::Value::Object(serde_json::Map::new()),
            text_match_info: None,
            hybrid_search_info: None,
            geo_distance_meters: None,
            vector_distance: None,
            name_score: None,
        }
    }
}

// -- Sort specification --

pub struct SortSpec {
    pub field: SortField,
    pub order: SortOrder,
    /// Where a field sort puts documents without a value.
    pub missing: Missing,
}

pub enum SortField {
    TextMatch,
    Field(FieldId),
    Decay(DecaySort),
    GroupFound,
    GeoDistance(FieldId, f64, f64), // field, center_lat, center_lng
    /// Sort by vector distance from the vector_query result.
    VectorDistance,
    /// `_seq_id`: the order documents were first written (`Segment::seq_id`), which writing one
    /// again keeps (Typesense's insertion order). A segment's documents are in this order.
    SeqId,
    /// JOIN sort: sort by a field in a referenced collection.
    /// Resolved at query time via forward lookup + target index.
    JoinSort {
        target_collection: String,
        target_field: String,
    },
}

pub enum SortOrder {
    Asc,
    Desc,
}

/// A hit's value of a sort spec, comparable across segments: a string as itself, a number as its
/// order key (a float's order-preserving bits), a score or a distance.
#[derive(Clone, Debug, PartialEq)]
pub enum SortValue {
    Missing,
    Int(i64),
    Score(u64),
    Float(f64),
    Str(Box<str>),
}

impl SortValue {
    /// Ascending order of two values of one sort spec.
    fn order(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a.cmp(b),
            (Self::Score(a), Self::Score(b)) => a.cmp(b),
            (Self::Float(a), Self::Float(b)) => a.total_cmp(b),
            (Self::Str(a), Self::Str(b)) => a.cmp(b),
            _ => Ordering::Equal,
        }
    }
}

/// A search's sort: sort_by as given, then for a text query its text match to break ties (with fewer than three); without
/// sort_by and with a default_sorting_field, a text query's text match, a vector query's distance
/// and that field descending; else nothing (the score, then the latest document). At most three.
fn sort_specs(params: &SearchParams, schema: &Schema) -> Result<Vec<SortSpec>, String> {
    let text = !params.q.is_empty() && params.q != "*";
    let vector = params.vector_query.is_some();
    let spec = |field: SortField, order: SortOrder| SortSpec { field, order, missing: Missing::Normal };
    let mut specs = match params.sort_by.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(s) => parse_sort_by(s, schema)?,
        None => match schema.default_sorting_field.as_deref().filter(|f| !f.is_empty()) {
            Some(dsf) => {
                let mut v = Vec::new();
                if text {
                    v.push(spec(SortField::TextMatch, SortOrder::Desc));
                }
                if vector {
                    v.push(spec(SortField::VectorDistance, SortOrder::Asc));
                }
                v.push(spec(SortField::Field(sortable_field(dsf, schema)?), SortOrder::Desc));
                v
            }
            None => return Ok(Vec::new()),
        },
    };
    if specs.len() > 3 {
        return Err("Only upto 3 sort_by fields can be specified.".into());
    }
    if !vector && specs.iter().any(|s| matches!(s.field, SortField::VectorDistance)) {
        return Err("sort_by vector_distance is only supported for vector queries, semantic search and hybrid search.".into());
    }
    if params.group_by.is_none() && specs.iter().any(|s| matches!(s.field, SortField::GroupFound)) {
        return Err("group_by parameters should not be empty when using sort_by group_found".into());
    }
    if text && specs.len() < 3 && !specs.iter().any(|s| matches!(s.field, SortField::TextMatch)) {
        specs.push(spec(SortField::TextMatch, SortOrder::Desc));
    }
    if vector && !text && specs.len() < 3 && !specs.iter().any(|s| matches!(s.field, SortField::VectorDistance)) {
        specs.push(spec(SortField::VectorDistance, SortOrder::Asc));
    }
    Ok(specs)
}

fn parse_sort_by(sort_by: &str, schema: &Schema) -> Result<Vec<SortSpec>, String> {
    let mut specs = Vec::new();
    // Split carefully: commas inside parentheses should not split
    let parts = split_sort_parts(sort_by);
    for part in &parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        // A referenced collection's fields, as Typesense writes them: `$brands(revenue:desc)`.
        if let Some(rest) = part.strip_prefix('$') {
            let bad = || format!("Bad syntax for sorting field `{part}`");
            let open = rest.find('(').filter(|_| rest.ends_with(')')).ok_or_else(bad)?;
            let collection = rest[..open].trim();
            for inner in split_sort_parts(&rest[open + 1..rest.len() - 1]) {
                let (field, order) = inner.trim().rsplit_once(':').ok_or_else(bad)?;
                if collection.is_empty() || field.trim().is_empty() {
                    return Err(bad());
                }
                specs.push(SortSpec {
                    field: SortField::JoinSort { target_collection: collection.to_string(), target_field: field.trim().to_string() },
                    order: sort_order(field, order)?,
                    missing: Missing::Normal,
                });
            }
            continue;
        }

        // Find the last `:asc` or `:desc` suffix
        let (field_str, order_str) = part.rsplit_once(':').ok_or("Parameter `sort_by` is malformed.")?;
        let order = sort_order(field_str, order_str)?;

        let field_str = field_str.trim();
        let mut missing = Missing::Normal;
        let field = if field_str == "_text_match" {
            SortField::TextMatch
        } else if field_str == "_group_found" {
            SortField::GroupFound
        } else if field_str == "_vector_distance" {
            SortField::VectorDistance
        } else if field_str == "_seq_id" {
            SortField::SeqId
        } else if let Some((name, m)) = parse_missing_values(field_str)? {
            missing = m;
            SortField::Field(sortable_field(name, schema)?)
        } else if field_str.contains('(') {
            // Could be geo distance or decay sort — check field type
            let paren_pos = field_str.find('(').unwrap();
            let field_name = &field_str[..paren_pos];
            if let Some(&fid) = schema.name_to_id.get(field_name) {
                if matches!(
                    schema.fields[fid as usize].field_type,
                    crate::schema::FieldType::GeoPoint | crate::schema::FieldType::GeoPointArray
                ) {
                    let args = &field_str[paren_pos + 1..field_str.len() - 1]; // strip parens
                    let parts: Vec<&str> = args.split(',').collect();
                    if parts.len() != 2 {
                        return Err(format!(
                            "Geo sort `{field_str}` expects exactly 2 arguments: lat, lng"
                        ));
                    }
                    let lat: f64 = parts[0].trim().parse().map_err(|_| {
                        format!("Invalid latitude in geo sort: `{}`", parts[0].trim())
                    })?;
                    let lng: f64 = parts[1].trim().parse().map_err(|_| {
                        format!("Invalid longitude in geo sort: `{}`", parts[1].trim())
                    })?;
                    SortField::GeoDistance(fid, lat, lng)
                } else {
                    let decay = crate::decay::parse_decay_expr(field_str, schema)?;
                    SortField::Decay(decay)
                }
            } else {
                // Unknown field — try decay parse (will produce better error)
                let decay = crate::decay::parse_decay_expr(field_str, schema)?;
                SortField::Decay(decay)
            }
        } else {
            SortField::Field(sortable_field(field_str, schema)?)
        };

        specs.push(SortSpec { field, order, missing });
    }
    Ok(specs)
}

/// `asc` or `desc`, in any case (Typesense upper-cases it).
fn sort_order(field: &str, order: &str) -> Result<SortOrder, String> {
    match order.trim().to_ascii_lowercase().as_str() {
        "asc" => Ok(SortOrder::Asc),
        "desc" => Ok(SortOrder::Desc),
        _ => Err(format!("Order for field` {}` should be either ASC or DESC.", field.trim())),
    }
}

fn sortable_field(name: &str, schema: &Schema) -> Result<FieldId, String> {
    schema
        .name_to_id
        .get(name)
        .copied()
        .filter(|&f| schema.fields[f as usize].sort)
        .ok_or_else(|| format!("Could not find a field named `{name}` in the schema for sorting."))
}

/// `field(missing_values: first|last|normal)`: the field and the option; None for other
/// parenthesised sort expressions (geo, decay).
fn parse_missing_values(field_str: &str) -> Result<Option<(&str, Missing)>, String> {
    let Some((name, rest)) = field_str.split_once('(') else { return Ok(None) };
    let Some(inner) = rest.strip_suffix(')') else { return Ok(None) };
    let Some((key, value)) = inner.split_once(':') else { return Ok(None) };
    if key.trim() != "missing_values" {
        return Ok(None);
    }
    let missing = match value.trim() {
        "first" => Missing::First,
        "last" => Missing::Last,
        "normal" => Missing::Normal,
        other => return Err(format!("Invalid missing_values value `{other}` for sort field `{}`; expected first, last or normal.", name.trim())),
    };
    Ok(Some((name.trim(), missing)))
}

/// Split sort_by string by commas, but not inside parentheses.
fn split_sort_parts(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                parts.push(current.clone());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Parse a named parameter value from the params section of vector_query.
/// Looks for `name:value` and extracts value up to the next `,` or end of string.
fn extract_vq_param<'a>(params_str: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}:");
    let pos = params_str.find(&needle)?;
    let start = pos + needle.len();
    let rest = &params_str[start..];
    let end = rest.find(',').unwrap_or(rest.len());
    let val = rest[..end].trim().trim_end_matches(')');
    if val.is_empty() { None } else { Some(val) }
}

/// Parse vector_query parameter.
/// Format: `field_name:([0.1, 0.2, ...], k:100, distance_threshold:0.5, ef:200, flat_search_cutoff:100, alpha:0.8)`
/// Or find-similar: `field_name:(id:doc_123, k:10)`
fn parse_vector_query(vector_query: &str, schema: &Schema) -> Result<VectorQueryParams, String> {
    let colon_pos = vector_query.find(":(").ok_or_else(|| {
        "Invalid vector_query format. Expected `field_name:([v1, v2, ...], k:N)`".to_string()
    })?;

    let field_name = vector_query[..colon_pos].trim().to_string();
    let field_id = *schema
        .name_to_id
        .get(field_name.as_str())
        .ok_or_else(|| format!("Vector field `{field_name}` not found in schema."))?;

    let field_def = &schema.fields[field_id as usize];
    match field_def.field_type {
        FieldType::FloatArray { num_dim } if num_dim > 0 => {}
        FieldType::FloatArray { .. } => {
            return Err(format!(
                "Field `{field_name}` is a float[] without num_dim — not a vector field. Set num_dim to use vector search."
            ));
        }
        _ => {
            return Err(format!(
                "Field `{field_name}` is not a vector (float[]) field."
            ));
        }
    }

    let inner = &vector_query[colon_pos + 1..];
    let inner = inner.trim();
    let inner = if inner.starts_with('(') && inner.ends_with(')') {
        &inner[1..inner.len() - 1]
    } else {
        inner
    };

    // Check for id-based "find similar" query (no vector array)
    let (query_vector, doc_id, params_section) = if let Some(bracket_start) = inner.find('[') {
        let bracket_end = inner.find(']').ok_or("Expected closing `]` in vector_query.")?;
        let vec_str = &inner[bracket_start + 1..bracket_end];
        let qv: Vec<f32> = vec_str
            .split(',')
            .map(|s| {
                s.trim()
                    .parse::<f32>()
                    .map_err(|_| format!("Invalid float in vector_query: `{}`", s.trim()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Validate dimensions
        if let FieldType::FloatArray { num_dim } = field_def.field_type {
            if qv.len() != num_dim as usize {
                return Err(format!(
                    "Vector dimension mismatch: field `{field_name}` expects {num_dim} dimensions, got {}.",
                    qv.len()
                ));
            }
        }

        let after = &inner[bracket_end + 1..];
        (qv, None, after)
    } else if let Some(id_val) = extract_vq_param(inner, "id") {
        // Find-similar by document ID — vector will be looked up at search time
        (Vec::new(), Some(id_val.to_string()), inner)
    } else {
        return Err("Expected vector array `[...]` or `id:DOC_ID` in vector_query.".to_string());
    };

    // Parse optional parameters from the params section
    let k = extract_vq_param(params_section, "k")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(1, 1000);

    let distance_threshold = extract_vq_param(params_section, "distance_threshold")
        .and_then(|v| v.parse::<f32>().ok());

    let ef = extract_vq_param(params_section, "ef")
        .and_then(|v| v.parse::<usize>().ok());

    let flat_search_cutoff = extract_vq_param(params_section, "flat_search_cutoff")
        .and_then(|v| v.parse::<usize>().ok());

    let alpha = extract_vq_param(params_section, "alpha")
        .and_then(|v| v.parse::<f32>().ok())
        .map(|a| a.clamp(0.0, 1.0));

    Ok(VectorQueryParams {
        field_name,
        field_id,
        query_vector,
        k,
        distance_threshold,
        ef,
        flat_search_cutoff,
        doc_id,
        alpha,
    })
}

/// The fields a `query_by` entry searches: the field itself, an object's nested string fields, or
/// for `prefix*` every indexed string field whose name starts with `prefix`. Only indexed fields
/// take part.
fn expand_query_by(name: &str, schema: &Schema) -> Result<Vec<FieldId>, ExpandError> {
    if name == "id" {
        return Err(ExpandError::Invalid("Cannot use `id` as a query by field.".into()));
    }
    let is_wildcard = name.contains('*');
    let prefix = if is_wildcard { &name[..name.len() - 1] } else { name };
    let mut out = Vec::new();
    for (fid, f) in schema.fields.iter().enumerate() {
        if !f.index || !f.name.starts_with(prefix) {
            continue;
        }
        let exact = f.name.len() == name.len();
        let is_object = matches!(f.field_type, FieldType::Object | FieldType::ObjectArray);
        if !f.field_type.is_searchable() {
            if exact && !is_object && !is_wildcard {
                return Err(ExpandError::Invalid(format!("Field `{name}` should be a string or a string array.")));
            }
            continue;
        }
        let nested = is_object || (schema.enable_nested_fields && f.name.contains('.') && !f.name.contains(".*"));
        if !exact && !is_wildcard && !nested {
            continue;
        }
        let below_object = schema.enable_nested_fields && f.name.len() > name.len() && f.name.as_bytes()[name.len()] == b'.';
        if (exact && !is_object) || is_wildcard || below_object {
            out.push(fid as FieldId);
        }
    }
    if out.is_empty() {
        return Err(ExpandError::NotFound(if is_wildcard {
            format!("No string or string array field found matching the pattern `{name}` in the schema.")
        } else {
            format!("Could not find a field named `{name}` in the schema.")
        }));
    }
    Ok(out)
}

enum ExpandError {
    /// A 404: skipped when `validate_field_names=false`.
    NotFound(String),
    Invalid(String),
}

/// Parse a query_by entry like `"title:3"` into `("title", 3.0)`. Default weight 1.0.
fn parse_query_by_entry(entry: &str) -> (&str, f32) {
    if let Some(pos) = entry.rfind(':') {
        let (name, weight_str) = entry.split_at(pos);
        if let Ok(w) = weight_str[1..].parse::<f32>() {
            return (name, w);
        }
    }
    (entry, 1.0)
}

/// A search target — either a full `IndexBundle` (production path, may have
/// many segments and trigger fan-out) or a single `Segment` (test fixtures
/// and other callers that already have a single segment in hand). The
/// public `execute*` functions take `impl Into<SearchTarget<'_>>` so callers
/// can pass either form transparently.
#[derive(Clone, Copy)]
pub enum SearchTarget<'a> {
    Bundle(&'a IndexBundle),
    Segment(&'a Segment),
}

impl<'a> From<&'a IndexBundle> for SearchTarget<'a> {
    fn from(b: &'a IndexBundle) -> Self {
        SearchTarget::Bundle(b)
    }
}

impl<'a> From<&'a std::sync::Arc<IndexBundle>> for SearchTarget<'a> {
    fn from(b: &'a std::sync::Arc<IndexBundle>) -> Self {
        SearchTarget::Bundle(b.as_ref())
    }
}

impl<'a> From<&'a Segment> for SearchTarget<'a> {
    fn from(s: &'a Segment) -> Self {
        SearchTarget::Segment(s)
    }
}

impl<'a> From<&'a std::sync::Arc<Segment>> for SearchTarget<'a> {
    fn from(s: &'a std::sync::Arc<Segment>) -> Self {
        SearchTarget::Segment(s.as_ref())
    }
}

impl SearchTarget<'_> {
    fn total_docs(&self) -> u32 {
        match self {
            SearchTarget::Bundle(b) => b.total_docs(),
            SearchTarget::Segment(s) => s.total_docs(),
        }
    }
}

/// Execute a search query against the index, returning either ungrouped or grouped results.
/// Optionally accepts synonym and stopword stores for query expansion/stripping.
pub fn execute<'a, T>(
    target: T,
    schema: &Schema,
    params: &SearchParams,
) -> Result<SearchResponse, String>
where
    T: Into<SearchTarget<'a>>,
{
    execute_with_stores(target, schema, params, None, None)
}

/// Execute with optional synonym/stopword stores.
#[allow(unused_variables)]
pub fn execute_with_stores<'a, T>(
    target: T,
    schema: &Schema,
    params: &SearchParams,
    synonym_store: Option<&crate::synonyms::SynonymStore>,
    stopword_store: Option<&crate::stopwords::StopwordStore>,
) -> Result<SearchResponse, String>
where
    T: Into<SearchTarget<'a>>,
{
    execute_with_stores_gpu(
        target, schema, params, synonym_store, stopword_store,
        #[cfg(feature = "cuda")]
        None,
    )
    .map(|(response, _timings)| response)
}

/// Full search entry point with optional GPU context.
#[allow(unused_variables)]
pub fn execute_with_stores_gpu<'a, T>(
    target: T,
    schema: &Schema,
    params: &SearchParams,
    synonym_store: Option<&crate::synonyms::SynonymStore>,
    stopword_store: Option<&crate::stopwords::StopwordStore>,
    #[cfg(feature = "cuda")] gpu_context: Option<&std::sync::Arc<crate::gpu::GpuContext>>,
) -> Result<(SearchResponse, SearchTimings), String>
where
    T: Into<SearchTarget<'a>>,
{
    execute_with_stores_gpu_join(
        target, schema, params, synonym_store, stopword_store,
        #[cfg(feature = "cuda")]
        gpu_context,
        None,
    )
}

/// Top-level search entry point with optional GPU context and JOIN support.
///
/// Dispatches based on the target type:
///
/// - **`SearchTarget::Segment`** (test fixtures, single-segment shortcuts):
///   call `search_one_segment` directly with no merge overhead. Behavior is
///   bit-identical to the pre-refactor code.
///
/// - **`SearchTarget::Bundle` with one segment** (the common case for
///   collections ≤ 500K docs and the path every existing test exercises):
///   unwrap the primary segment and dispatch to `search_one_segment`.
///
/// - **`SearchTarget::Bundle` with multiple segments** (large batch
///   collections, realtime writes): one text stage over all segments (`text_stage`), then each
///   segment's share through `search_one_segment`, merged into the response
///   one segment holding every document would give (`merge_segment_responses`).
#[allow(unused_variables)]
pub fn execute_with_stores_gpu_join<'a, T>(
    target: T,
    schema: &Schema,
    params: &SearchParams,
    synonym_store: Option<&crate::synonyms::SynonymStore>,
    stopword_store: Option<&crate::stopwords::StopwordStore>,
    #[cfg(feature = "cuda")] gpu_context: Option<&std::sync::Arc<crate::gpu::GpuContext>>,
    join_context: Option<&JoinContext<'_>>,
) -> Result<(SearchResponse, SearchTimings), String>
where
    T: Into<SearchTarget<'a>>,
{
    let target = target.into();
    if params.name_score == Some(true) {
        return name_score::rerank(target, schema, params, |p| {
            execute_with_stores_gpu_join(
                target,
                schema,
                p,
                synonym_store,
                stopword_store,
                #[cfg(feature = "cuda")]
                gpu_context,
                join_context,
            )
        });
    }
    let deadline = deadline(params, Instant::now());
    // The segments with their deletion masks (a bare segment has none).
    let segs = segments_of(&target);
    let collection_docs = target.total_docs();
    // facet_by and facet_query, checked before anything is searched
    let facet_by = match params.facet_by.as_deref() {
        Some(f) => facet::parse_facet_by(f, schema)?,
        None => facet::FacetBy::default(),
    };
    let fq = facet_query_of(params, &facet_by, schema)?;
    // a search of a union removing duplicates: its facets are the union's (`facets_over`)
    let distinct = params.union == Some(UnionPart::Distinct);
    let (facet_by, fq) = if distinct { (facet::FacetBy::default(), None) } else { (facet_by, fq) };

    // Filters per segment, then one text stage over all of them.
    let t_filter = Instant::now();
    let mut inputs = Vec::with_capacity(segs.len());
    for &(index, deleted) in &segs {
        let (filter, excluded) = segment_filter(index, schema, params, join_context, deleted)?;
        inputs.push((index, filter, excluded, deleted));
    }
    let filter_ms = t_filter.elapsed().as_secs_f64() * 1000.0;
    // the hits any segment pages to: the page's end, top_k facets' first hits, and room for the
    // pinned hits, which leave the organic ones
    let pins = params.pinned_hits.as_deref().map_or(0, |p| parse_pinned_entries(p).len());
    let top = facet_by.local.iter().any(|f| f.top_k);
    let page_end = if top { top_k_size(params) } else { params.start() + params.per_page } + pins;
    let mut texts = text_stage(inputs, schema, params, synonym_store, stopword_store, deadline, filter_ms, page_end)?;
    // pinned hits, each with the segment holding it
    let pins = resolve_pins(params, &segs, &texts);
    for (rank, pin) in pins.iter().enumerate() {
        texts[pin.seg].pinned.push((rank, *pin));
    }

    if segs.len() == 1 {
        let text = texts.pop().expect("one segment");
        return timed_out(search_one_segment(
            segs[0].0,
            schema,
            params,
            #[cfg(feature = "cuda")]
            gpu_context,
            join_context,
            segs[0].1,
            text,
            &facet_by,
            fq.as_ref(),
        ));
    }

    // Several segments: each finishes its share, asked for its first hits (a document ranks no
    // better overall than within its segment); the responses merge into the one ordering, the
    // pinned hits go in where they belong (`PinLayout`), and the facets over every segment's
    // results at once (`late_facets`) are counted.
    let (a, b) = (params.start(), params.start() + params.per_page);
    let top = facet_by.local.iter().any(|f| f.top_k).then(|| top_k_size(params));
    let end = top.unwrap_or(b);
    let grouped = params.group_by.is_some();
    let mut segment_params = params.clone();
    // grouped: also the groups pinned ones take in
    segment_params.per_page = end + if grouped { pins.len() } else { 0 };
    segment_params.page = 1;
    segment_params.offset = None;
    segment_params.pinned_hits = None;

    // grouped: each segment's share kept for a second round (below)
    let kept: Option<Vec<SegmentText>> = grouped.then(|| texts.clone());
    let mut per_segment: Vec<(SearchResponse, SearchTimings)> = Vec::with_capacity(segs.len());
    for (&(index, deleted), text) in segs.iter().zip(texts) {
        per_segment.push(search_one_segment(
            index,
            schema,
            &segment_params,
            #[cfg(feature = "cuda")]
            gpu_context,
            join_context,
            deleted,
            text,
            &facet_by,
            fq.as_ref(),
        )?);
    }
    // the pinned hits by rank, and each segment's results for the late facets
    let mut pinned: Vec<Option<PinnedHit>> = (0..pins.len()).map(|_| None).collect();
    let mut results: Vec<Bitmap> = Vec::with_capacity(segs.len());
    for (r, _) in &mut per_segment {
        let (hits, docs) = match r {
            SearchResponse::Ungrouped(u) => (&mut u.pinned, &mut u.docs),
            SearchResponse::Grouped(g) => (&mut g.pinned, &mut g.docs),
        };
        for p in hits.drain(..) {
            let rank = p.rank;
            pinned[rank] = Some(p);
        }
        results.push(docs.take().unwrap_or_default());
    }
    // per segment the groups it answered with, and every group it found with its documents
    let answered: Vec<(rustc_hash::FxHashSet<u64>, FxHashMap<u64, usize>)> = per_segment
        .iter()
        .map(|(r, _)| match r {
            SearchResponse::Grouped(g) => (g.grouped_hits.iter().map(|h| h.id).collect(), g.group_counts.iter().copied().collect()),
            SearchResponse::Ungrouped(_) => Default::default(),
        })
        .collect();

    let (mut merged, timings) = merge_segment_responses(per_segment, params, schema, collection_docs, 0, usize::MAX)?;
    // the first hits of the ordering per segment, for top_k facets
    let mut tops: Vec<Bitmap> = vec![Bitmap::new(); segs.len()];
    let mut add_top = |seq: u64| {
        if let Some(k) = segs.iter().position(|(s, _)| seq >= u64::from(s.base_doc_id) && seq < u64::from(s.base_doc_id) + u64::from(s.total_docs())) {
            tops[k].add((seq - u64::from(segs[k].0.base_doc_id)) as DocId);
        }
    };
    match &mut merged {
        SearchResponse::Ungrouped(r) => {
            let layout = PinLayout::new(pins.iter().map(|p| p.position), r.found.saturating_sub(pins.len()));
            let ranked = std::mem::take(&mut r.hits);
            if let Some(k) = top {
                ranked.iter().take(layout.organic(0, k).end).for_each(|h| add_top(h.seq));
                pinned.iter().zip(&layout.slots).filter(|&(_, &slot)| slot < k).filter_map(|(p, _)| p.as_ref()).for_each(|p| add_top(p.hit.seq));
            }
            let range = layout.organic(a, b);
            r.hits = layout.page(a, b, ranked.into_iter().skip(range.start).take(range.len()), |j| pinned[j].take().map(|p| p.hit));
        }
        SearchResponse::Grouped(g) => {
            let group_limit = params.group_limit.unwrap_or(3).max(1);
            let sort_specs = sort_specs(params, schema)?;
            let order = |x: &Hit, y: &Hit| hit_order(x, y, &sort_specs, schema);
            let counts: FxHashMap<u64, usize> = std::mem::take(&mut g.group_counts).into_iter().collect();
            // the pinned groups: the pinned hits by group, each where its first goes; an organic
            // group of their values joins them
            struct PinGroup {
                id: u64,
                key: Vec<serde_json::Value>,
                position: usize,
                hits: Vec<Hit>,
            }
            let mut pgroups: Vec<PinGroup> = Vec::new();
            for (p, pin) in pinned.iter_mut().zip(&pins) {
                let Some(PinnedHit { hit, group, .. }) = p.take() else { continue };
                let (id, key) = group.unwrap_or_default();
                match pgroups.iter_mut().find(|x| x.id == id) {
                    Some(x) => x.hits.push(hit),
                    None => pgroups.push(PinGroup { id, key, position: pin.position, hits: vec![hit] }),
                }
            }
            let mut joined: FxHashMap<u64, Vec<Hit>> = FxHashMap::default();
            let mut ranked: Vec<grouping::GroupHit> = Vec::new();
            for group in std::mem::take(&mut g.grouped_hits) {
                if pgroups.iter().any(|x| x.id == group.id) {
                    joined.insert(group.id, group.hits);
                } else {
                    ranked.push(group);
                }
            }
            let organic = counts.len() - pgroups.iter().filter(|x| counts.contains_key(&x.id)).count();
            let layout = PinLayout::new(pgroups.iter().map(|x| x.position), organic);
            ranked.truncate(layout.organic(0, end).end);

            // A group wanted has documents in a segment where it ranked below that segment's
            // answer: search that segment again for those groups only, so their best documents
            // count.
            let wanted: rustc_hash::FxHashSet<u64> = ranked
                .iter()
                .map(|h| h.id)
                .chain(pgroups.iter().zip(&layout.slots).filter(|&(_, &slot)| slot < end).map(|(x, _)| x.id))
                .collect();
            let group_fields = grouping::parse_group_by(params.group_by.as_deref().unwrap_or(""), schema)?;
            let group_missing = params.group_missing_values.unwrap_or(true);
            for (k, text) in kept.into_iter().flatten().enumerate() {
                let (on_page, counts) = &answered[k];
                let missing: rustc_hash::FxHashSet<u64> = wanted.iter().copied().filter(|h| counts.get(h).is_some_and(|&n| n > 0) && !on_page.contains(h)).collect();
                if missing.is_empty() {
                    continue;
                }
                let (index, deleted) = segs[k];
                let keys = grouping::GroupKeys::new(index, schema, &group_fields, group_missing);
                let mut id = keys.ids();
                let mut restricted = Bitmap::new();
                for d in text.candidates.iter() {
                    if missing.contains(&id(d)) && !text.pinned.iter().any(|(_, p)| p.doc == d) {
                        restricted.add(d);
                    }
                }
                let mut again = segment_params.clone();
                again.per_page = missing.len();
                again.facet_by = None;
                let text = SegmentText { candidates: restricted, pinned: Vec::new(), ..text };
                let (more, _) = search_one_segment(
                    index,
                    schema,
                    &again,
                    #[cfg(feature = "cuda")]
                    gpu_context,
                    join_context,
                    deleted,
                    text,
                    &facet::FacetBy::default(),
                    None,
                )?;
                let SearchResponse::Grouped(more) = more else { continue };
                for extra in more.grouped_hits {
                    match ranked.iter_mut().find(|h| h.id == extra.id) {
                        Some(target) => target.hits.extend(extra.hits),
                        None => joined.entry(extra.id).or_default().extend(extra.hits),
                    }
                }
            }
            for group in &mut ranked {
                group.hits.sort_by(order);
                group.hits.truncate(group_limit);
            }
            // a pinned group: its pinned hits, then its best organic ones
            let mut placed: Vec<Option<grouping::GroupHit>> = pgroups
                .into_iter()
                .map(|x| {
                    let mut organic = joined.remove(&x.id).unwrap_or_default();
                    organic.sort_by(order);
                    let found = x.hits.len() + counts.get(&x.id).copied().unwrap_or(0);
                    let mut hits = x.hits;
                    hits.extend(organic);
                    hits.truncate(group_limit);
                    Some(grouping::GroupHit { group_key: x.key, hits, found, id: x.id })
                })
                .collect();
            if let Some(k) = top {
                ranked.iter().flat_map(|h| &h.hits).for_each(|h| add_top(h.seq));
                placed.iter().zip(&layout.slots).filter(|&(_, &slot)| slot < k).filter_map(|(x, _)| x.as_ref()).flat_map(|x| &x.hits).for_each(|h| add_top(h.seq));
            }
            g.found = layout.total;
            let range = layout.organic(a, b);
            g.grouped_hits = layout.page(a, b, ranked.into_iter().skip(range.start).take(range.len()), |j| placed[j].take());
        }
    }
    // the facets over every segment's results at once
    let late: Vec<&facet::FacetSpec> = facet_by.local.iter().filter(|f| f.top_k || fq.as_ref().is_some_and(|q| q.fid == f.fid)).collect();
    if !late.is_empty() {
        let indexes: Vec<&Index> = segs.iter().map(|s| s.0).collect();
        let groups: Vec<Option<facet::DocGroups>> = match params.group_by.as_deref() {
            Some(gb) => {
                let group_fields = grouping::parse_group_by(gb, schema)?;
                let group_missing = params.group_missing_values.unwrap_or(true);
                indexes.iter().zip(&results).map(|(&index, r)| Some(facet::DocGroups::new(r, grouping::GroupKeys::new(index, schema, &group_fields, group_missing).ids()))).collect()
            }
            None => indexes.iter().map(|_| None).collect(),
        };
        let facets = late_facets(&late, &indexes, &results, top.map(|_| tops.as_slice()), fq.as_ref(), &groups, params, schema);
        let counts = match &mut merged {
            SearchResponse::Ungrouped(r) => &mut r.facet_counts,
            SearchResponse::Grouped(g) => &mut g.facet_counts,
        };
        counts.extend(facets);
        counts.sort_by_key(|f| f.position);
    }
    if distinct {
        if let SearchResponse::Ungrouped(r) = &mut merged {
            r.segment_docs = results;
        }
    }
    timed_out(Ok((merged, timings)))
}

/// `facet_query`, parsed for the search's facets.
fn facet_query_of<'s>(params: &'s SearchParams, facet_by: &facet::FacetBy, schema: &'s Schema) -> Result<Option<facet_query::FacetQuery<'s>>, String> {
    let Some(fq) = params.facet_query.as_deref() else { return Ok(None) };
    let num_typos = params.facet_query_num_typos.unwrap_or(2).min(2) as u8;
    let start = params.highlight_start_tag.as_deref().unwrap_or("<mark>");
    let end = params.highlight_end_tag.as_deref().unwrap_or("</mark>");
    facet_query::FacetQuery::parse(fq, facet_by, schema, num_typos, start, end)
}

/// Facets over every segment's results at once: the facet_query field's, with the values matching
/// by the fewest typos by which any result matches (`facet::query_level`), counted in the documents
/// holding the query's words, and top_k ones over `tops` (each segment's share of the search's
/// first hits).
#[allow(clippy::too_many_arguments)]
fn late_facets(
    specs: &[&facet::FacetSpec],
    indexes: &[&Index],
    results: &[Bitmap],
    tops: Option<&[Bitmap]>,
    fq: Option<&facet_query::FacetQuery<'_>>,
    groups: &[Option<facet::DocGroups>],
    params: &SearchParams,
    schema: &Schema,
) -> Vec<FacetResult> {
    // several segments, or a union's search: every value, for the merge to count and cut
    let multi = indexes.len() > 1 || params.union.is_some();
    let max = if multi { usize::MAX } else { params.max_facet_values.unwrap_or(10) };
    // per segment the results that can hold a value the query matches
    let narrowed: Vec<Option<Bitmap>> = indexes.iter().zip(results).map(|(index, r)| fq.and_then(|q| q.docs(index)).map(|d| &d & r)).collect();
    let level = fq.and_then(|q| {
        let spec = specs.iter().find(|s| s.fid == q.fid)?;
        indexes
            .iter()
            .enumerate()
            .filter_map(|(k, index)| facet::query_level(&facet_source(spec, index), narrowed[k].as_ref().unwrap_or(&results[k]), q))
            .min()
    });
    let mut acc: FxHashMap<usize, FacetResult> = FxHashMap::default();
    let mut out = Vec::new();
    for spec in specs {
        let query = fq.filter(|q| q.fid == spec.fid);
        for (k, index) in indexes.iter().enumerate() {
            let base = if spec.top_k { tops.map(|t| &t[k]) } else { Some(&results[k]) };
            let Some(base) = base else { continue };
            let narrowed_base = query.and(narrowed[k].as_ref()).map(|n| base & n);
            let docs = narrowed_base.as_ref().unwrap_or(base);
            let how = facet::Counting { max, multi, groups: groups[k].as_ref(), query: query.map(|q| (q as &dyn facet::ValueQuery, level.unwrap_or(0))) };
            let Some(mut f) = facet::compute_facet(spec, facet_source(spec, index), docs, how) else { continue };
            attach_parents(std::slice::from_mut(&mut f), params, schema, index, docs);
            if multi {
                merge_facet_into(&mut acc, f);
            } else {
                out.push(f);
            }
        }
    }
    if multi {
        out = finish_facets(acc, params);
    }
    out
}

/// A segment's live document `ext_id` for pinned_hits; with filter_curated_hits only one passing
/// the filter.
fn pinned_doc(index: &Index, deleted: Option<&Bitmap>, filter: Option<&Bitmap>, params: &SearchParams, ext_id: &str) -> Option<DocId> {
    let id = index.ext_id_lookup(ext_id)?;
    if deleted.is_some_and(|d| d.contains(id)) || (params.filter_curated_hits && filter.is_some_and(|f| !f.contains(id))) {
        return None;
    }
    Some(id)
}

/// A segment's filter (deleted documents taken out) and hidden hits, before the text match.
fn segment_filter(
    index: &Index,
    schema: &Schema,
    params: &SearchParams,
    join_context: Option<&JoinContext<'_>>,
    deleted_docs: Option<&Bitmap>,
) -> Result<(Option<Bitmap>, Option<Bitmap>), String> {
    let filter_op = match params.filter_by {
        Some(ref filter_str) => parse_filter(filter_str, schema).map_err(|e| e.message)?,
        None => None,
    };
    // Typesense's filter_by; JOIN predicates need the other collections.
    let mut filter_bitmap: Option<Bitmap> = filter_op.as_ref().map(|op| match join_context {
        Some(jctx) if op.contains_join() => execute_filter_with_joins(op, index, schema, jctx),
        _ => execute_filter(op, index, schema),
    });
    // Deleted documents (copies deleted or written again) never match.
    if let Some(d) = deleted_docs.filter(|d| !d.is_empty()) {
        match filter_bitmap.as_mut() {
            Some(fb) => fb.andnot_inplace(d),
            None => {
                let mut live = index.all_docs.clone();
                live.andnot_inplace(d);
                filter_bitmap = Some(live);
            }
        }
    }
    // hidden_hits (Typesense's excluded ids).
    let mut excluded: Option<Bitmap> = None;
    if let Some(ref hidden) = params.hidden_hits {
        let x = excluded.get_or_insert_with(Bitmap::new);
        for ext_id in hidden.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(doc_id) = index.ext_id_lookup(ext_id) {
                x.add(doc_id);
            }
        }
    }
    Ok((filter_bitmap, excluded))
}

/// A segment's share of a search's text stage.
#[derive(Clone)]
struct SegmentText {
    /// The filter, phrases applied.
    filter_bitmap: Option<Bitmap>,
    /// Hidden hits and the documents of the query's exclusions.
    excluded: Option<Bitmap>,
    /// The documents found (for `q=*` every visible document).
    candidates: Bitmap,
    scores: Scores,
    text: Option<TextOutcome>,
    search_cutoff: bool,
    filter_ms: f64,
    text_ms: f64,
    /// One of several segments: facets count every value and groups report all their counts, for
    /// the merge.
    multi: bool,
    /// The pinned documents of the segment, with their ranks among the search's pins.
    pinned: Vec<(usize, Pin)>,
}

/// The text stage of a search over `segments` (each with its filter, hidden hits and deleted
/// documents): one search over all of them (`text::search`), so the segments together find what
/// one segment holding all their documents would. `page_end`: the hits any segment pages to.
#[allow(clippy::too_many_arguments)]
fn text_stage(
    segments: Vec<(&Index, Option<Bitmap>, Option<Bitmap>, Option<&Bitmap>)>,
    schema: &Schema,
    params: &SearchParams,
    synonym_store: Option<&crate::synonyms::SynonymStore>,
    stopword_store: Option<&crate::stopwords::StopwordStore>,
    deadline: text::Deadline,
    filter_ms: f64,
    page_end: usize,
) -> Result<Vec<SegmentText>, String> {
    let t_text = Instant::now();
    let multi = segments.len() > 1;
    let match_type = match params.text_match_type.as_deref() {
        Some(t) => MatchType::parse(t)?,
        None => MatchType::MaxScore,
    };
    let search_fields = resolve_search_fields(params, schema)?;
    let is_text_query = !params.q.is_empty() && params.q != "*";

    // The named stopword set must exist (a 404).
    let stopword_set = match params.stopwords.as_deref() {
        Some(name) => Some(
            stopword_store
                .and_then(|store| store.get_stopwords(name))
                .ok_or_else(|| format!("Could not find the stopword set named `{name}`."))?,
        ),
        None => None,
    };
    if params.synonym_num_typos.is_some_and(|n| n > 2) {
        return Err("Value of `synonym_num_typos` must not be greater than 2.".into());
    }
    // The synonym sets of a text query: the search's, else the collection's; none with
    // enable_synonyms=false.
    let synonym_indexes: Vec<std::sync::Arc<crate::synonyms::SynonymIndex>> = if !is_text_query || params.enable_synonyms == Some(false) {
        Vec::new()
    } else {
        let names: Vec<String> = match params.synonym_sets.as_deref() {
            Some(s) => s.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            None => schema.synonym_sets.clone().unwrap_or_default(),
        };
        names
            .iter()
            .map(|name| {
                synonym_store
                    .and_then(|store| store.index(name))
                    .ok_or_else(|| format!("Could not find synonym index `{name}`"))
            })
            .collect::<Result<_, _>>()?
    };

    // The documents a result may hold, without the excluded ones.
    let visible = |index: &Index, filter: &Option<Bitmap>, excluded: &Option<Bitmap>| -> Bitmap {
        let mut bm = filter.clone().unwrap_or_else(|| index.all_docs.clone());
        if let Some(x) = excluded {
            bm.andnot_inplace(x);
        }
        bm
    };
    let done = |filter_bitmap, excluded, candidates, scores, text, search_cutoff| SegmentText {
        filter_bitmap,
        excluded,
        candidates,
        scores,
        text,
        search_cutoff,
        filter_ms,
        text_ms: t_text.elapsed().as_secs_f64() * 1000.0,
        multi,
        pinned: Vec::new(),
    };
    if !is_text_query {
        return Ok(segments
            .into_iter()
            .map(|(index, f, x, _)| {
                let c = visible(index, &f, &x);
                done(f, x, c, Scores::default(), None, false)
            })
            .collect());
    }
    if search_fields.is_empty() {
        return Err("Missing `query_by` parameter.".into());
    }
    let parsed = query::parse(&params.q, &schema.symbols_to_index, &schema.token_separators, stopword_set.as_ref());
    // fields with terms in some segment
    let fields: Vec<text::Field<'_>> = search_fields
        .iter()
        .filter_map(|f| {
            let invs: Vec<Option<&InvertedIndex>> = segments.iter().map(|s| s.0.inverted.get(&f.fid)).collect();
            invs.iter().any(Option::is_some).then(|| text::Field {
                fid: f.fid,
                def: &schema.fields[f.fid as usize],
                invs,
                weight: f.weight,
                num_typos: f.num_typos,
                prefix: f.prefix,
                infix: f.infix,
                names: f.names,
            })
        })
        .collect();

    // Per segment: phrases filter the documents, exclusions leave them out.
    let mut preps: Vec<(&Index, Option<Bitmap>, Option<Bitmap>, Option<&Bitmap>)> = Vec::with_capacity(segments.len());
    for (index, mut filter, mut excluded, deleted) in segments {
        for x in &parsed.exclude {
            let docs = text::phrase_docs(x, &fields, index, schema, deadline);
            match excluded.as_mut() {
                Some(e) => *e |= &docs,
                None => excluded = Some(docs),
            }
        }
        for phrase in &parsed.phrases {
            let docs = text::phrase_docs(phrase, &fields, index, schema, deadline);
            filter = Some(match filter.take() {
                Some(fb) => &fb & &docs,
                None => docs,
            });
        }
        preps.push((index, filter, excluded, deleted.filter(|d| !d.is_empty())));
    }

    let words = parsed.words.len() + parsed.phrases.iter().map(Vec::len).sum::<usize>();
    let nothing = |preps: Vec<(&Index, Option<Bitmap>, Option<Bitmap>, Option<&Bitmap>)>| -> Vec<SegmentText> {
        let outcome = TextOutcome { words, marks: Default::default() };
        preps.into_iter().map(|(_, f, x, _)| done(f, x, Bitmap::new(), Scores::default(), Some(outcome.clone()), false)).collect()
    };
    // Every word a stopword finds nothing; no word at all lists the documents, but searched only
    // in names, where listing everyone would be the worst answer.
    if parsed.only_stopwords {
        return Ok(nothing(preps));
    }
    if parsed.is_empty() {
        if search_fields.iter().all(|f| schema.fields[f.fid as usize].name_matching) {
            return Ok(nothing(preps));
        }
        return Ok(preps
            .into_iter()
            .map(|(index, f, x, _)| {
                let docs = visible(index, &f, &x);
                done(f, x, docs, Scores::default(), Some(TextOutcome { words, marks: Default::default() }), false)
            })
            .collect());
    }

    let mut rewrites: Vec<Vec<String>> = Vec::new();
    let normalized: Vec<String> = parsed.words.iter().map(|w| crate::text::normalize(w)).collect();
    let synonym_prefix = params.synonym_prefix.unwrap_or(false);
    let synonym_num_typos = params.synonym_num_typos.unwrap_or(0) as u32;
    for index in &synonym_indexes {
        index.reduce(&normalized, synonym_prefix, synonym_num_typos, &mut rewrites);
    }
    let tparams = text_params(params, match_type, deadline)?;

    // With group_by the typo threshold counts groups: by their values over several segments.
    let group_fields = match params.group_by {
        Some(ref gb) => grouping::parse_group_by(gb, schema)?,
        None => Vec::new(),
    };
    let group_missing = params.group_missing_values.unwrap_or(true);
    let group_keys: Vec<Option<grouping::GroupKeys<'_>>> =
        preps.iter().map(|p| (!group_fields.is_empty()).then(|| grouping::GroupKeys::new(p.0, schema, &group_fields, group_missing))).collect();
    let group_of: Vec<Option<Box<dyn Fn(DocId) -> Option<u64> + '_>>> = group_keys
        .iter()
        .map(|k| {
            k.as_ref().map(|k| -> Box<dyn Fn(DocId) -> Option<u64> + '_> {
                if multi {
                    // the values read once per group of the segment
                    let ids: std::cell::RefCell<FxHashMap<u64, u64>> = Default::default();
                    Box::new(move |d| Some(*ids.borrow_mut().entry(k.hash(d)).or_insert_with(|| k.id(d))))
                } else {
                    Box::new(move |d| Some(k.hash(d)))
                }
            })
        })
        .collect();
    let segs: Vec<text::Segment<'_>> = preps
        .iter()
        .zip(&group_of)
        .map(|(p, g)| text::Segment { index: p.0, filter: p.1.as_ref(), excluded: p.2.as_ref(), deleted: p.3, group_of: g.as_deref() })
        .collect();
    let q = text::Query { words: &parsed.words, phrases: &parsed.phrases, rewrites: &rewrites };
    // A field orders the results and the text match only breaks its ties: of each segment's
    // documents found only those up to the page's end by the field, with every one tied with the
    // last, are ranked; no other reaches the page (`paginate_candidates`).
    // (a sort_by in error ranks as it is found, and errs where it always did)
    let by_field = match (params.group_by.as_deref(), params.vector_query.as_deref(), sort_specs(params, schema)) {
        (None, None, Ok(specs)) => match specs.as_slice() {
            [SortSpec { field: SortField::Field(fid), order, missing }, SortSpec { field: SortField::TextMatch, order: SortOrder::Desc, .. }] => {
                Some((*fid, matches!(order, SortOrder::Desc), *missing))
            }
            _ => None,
        },
        _ => None,
    };
    let pick = |s: usize, found: &Bitmap| -> Bitmap {
        let Some((fid, descending, missing)) = by_field else { return found.clone() };
        let Some(col) = SortColumn::of(preps[s].0, fid) else { return found.clone() };
        if found.cardinality() <= page_end as u64 {
            return found.clone();
        }
        if page_end == 0 {
            return Bitmap::new();
        }
        let missing_first = col.missing_first(descending, missing);
        let mut keyed: Vec<(u64, DocId)> = found.iter().map(|d| (col.order(d, descending, missing_first), d)).collect();
        let last = keyed.select_nth_unstable_by_key(page_end - 1, |p| p.0).1.0;
        let mut docs: Vec<DocId> = keyed.into_iter().filter(|p| p.0 <= last).map(|p| p.1).collect();
        docs.sort_unstable();
        Bitmap::of(&docs)
    };
    let outcome = text::search(&fields, &segs, &q, &tparams, schema, by_field.is_some().then_some(&pick as &dyn Fn(usize, &Bitmap) -> Bitmap))?;
    drop(segs);
    let text::Outcome { found, scores, search_cutoff, marks } = outcome;
    Ok(preps
        .into_iter()
        .zip(found.into_iter().zip(scores))
        .map(|((_, f, x, _), (found, scores))| {
            let text = TextOutcome { words, marks: marks.clone() };
            done(f, x, found, scores, Some(text), search_cutoff)
        })
        .collect())
}

/// Per-segment search execution. Operates on one Segment at a time.
/// The public `execute_with_stores_gpu_join` either dispatches directly here
/// (single-segment collection) or fans out across all segments and merges
/// the responses (multi-segment collection — see `merge_segment_responses`).
///
/// `deleted_docs` is the segment's live-docs deletion mask (Lucene-style).
/// When `Some`, every candidate-producing path AND-NOTs the mask before
/// scoring so superseded and deleted copies are excluded from the result
/// set.
#[allow(unused_variables)]
fn search_one_segment(
    index: &Index,
    schema: &Schema,
    params: &SearchParams,
    #[cfg(feature = "cuda")] gpu_context: Option<&std::sync::Arc<crate::gpu::GpuContext>>,
    join_context: Option<&JoinContext<'_>>,
    deleted_docs: Option<&Bitmap>,
    seg: SegmentText,
    facet_by: &facet::FacetBy,
    facet_query: Option<&facet_query::FacetQuery<'_>>,
) -> Result<(SearchResponse, SearchTimings), String> {
    // Live (post-deletion) doc count for `out_of` reporting. The index's
    // physical total stays unchanged because segments are immutable; deletion
    // is expressed as a parallel bitmap on the bundle.
    let live_total_docs: u32 = match deleted_docs {
        Some(bm) => index.total_docs().saturating_sub(bm.cardinality() as u32),
        None => index.total_docs(),
    };
    let start = Instant::now();

    // Parse group_by
    let group_fields = if let Some(ref gb) = params.group_by {
        grouping::parse_group_by(gb, schema)?
    } else {
        vec![]
    };
    let is_grouped = !group_fields.is_empty();
    let group_limit = params.group_limit.unwrap_or(3);
    let group_missing = params.group_missing_values.unwrap_or(true);


    let SegmentText { filter_bitmap, excluded, candidates, scores, text, search_cutoff, filter_ms, text_ms, multi, pinned } = seg;
    let candidates_after_filter = filter_bitmap.as_ref().map(|b: &Bitmap| b.cardinality());

    // Parse sort_by
    let sort_specs = sort_specs(params, schema)?;

    // Check if _group_found sort is used
    let has_group_found_sort = sort_specs
        .iter()
        .any(|s| matches!(s.field, SortField::GroupFound));

    let is_text_query = !params.q.is_empty() && params.q != "*";
    let deferred_filter_ms = 0.0f64;
    let mut candidates = candidates;
    let mut scores = scores;
    let text_match_count = candidates.cardinality() as u64;
    let scored_count = Some(scores.len() as u64);

    // Vector search integration
    let t_vector = Instant::now();
    let vector_params = if let Some(ref vq_str) = params.vector_query {
        Some(parse_vector_query(vq_str, schema)?)
    } else {
        None
    };

    let mut vector_distances: FxHashMap<DocId, f32> =
        FxHashMap::with_capacity_and_hasher(256, Default::default());
    let mut rrf_scores: FxHashMap<DocId, f32> =
        FxHashMap::with_capacity_and_hasher(0, Default::default());

    if let Some(ref vq) = vector_params {
        let vec_idx = index.vector_indexes.get(&vq.field_id).ok_or_else(|| {
            format!("No vector index found for field `{}`.", vq.field_name)
        })?;

        // Resolve query vector: either provided directly or looked up by doc ID
        let query_vector: &[f32] = if let Some(ref doc_ext_id) = vq.doc_id {
            // Find-similar: look up the document's vector by external ID
            let dim = vec_idx.num_dim as usize;
            let internal_id = index.ext_id_lookup(doc_ext_id)
                .ok_or_else(|| format!("Document `{doc_ext_id}` not found for vector lookup."))? as usize;
            let dim = vec_idx.num_dim as usize;
            let start = internal_id * dim;
            let end = start + dim;
            if dim == 0 || end > vec_idx.vectors.len() {
                return Err(format!("Document `{doc_ext_id}` has no vector in field `{}`. The document may not have a vector, or the vector index is empty.", vq.field_name));
            }
            &vec_idx.vectors[start..end]
        } else {
            &vq.query_vector
        };

        let ef_search = vq.ef.unwrap_or_else(|| vq.k.max(100));

        // Decide: brute-force or HNSW based on flat_search_cutoff
        let use_flat = vq.flat_search_cutoff.map_or(false, |cutoff| {
            filter_bitmap.as_ref().map_or(false, |bm| (bm.cardinality() as usize) < cutoff)
        });

        // Try GPU vector search first, fall back to CPU HNSW
        #[allow(unused_mut)]
        let mut vector_results: Option<Vec<(DocId, f32)>> = None;

        if !use_flat {
            #[cfg(feature = "cuda")]
            if let Some(gpu) = gpu_context {
                if let (Some(gpu_vecs), Some(vk)) = (&vec_idx.gpu_vectors, &gpu.vector_kernels) {
                    let num_docs = (vec_idx.vectors.len() / vec_idx.num_dim as usize) as u32;
                    let mut bufs = gpu.search_buffers.lock().unwrap();
                    let result = if filter_bitmap.is_some() {
                        let bm = filter_bitmap.as_ref().unwrap();
                        roaring_to_u64_bitmap_into(bm, num_docs, &mut bufs.filter_bitmap_host);
                        let filter_words = (num_docs as usize + 63) / 64;
                        let filter_slice = bufs.filter_bitmap_host[..filter_words].to_vec();
                        crate::vector::gpu::dispatch_vec_search_filtered(
                            &gpu.stream, vk, gpu_vecs, query_vector,
                            &filter_slice,
                            num_docs, vec_idx.num_dim as u32,
                            &vec_idx.metric, vq.k, &mut bufs,
                        )
                    } else if let (Some(gpu_codes), Some(gpu_norms), Some(quantizer)) =
                        (&vec_idx.gpu_codes, &vec_idx.gpu_norms, &vec_idx.quantizer)
                    {
                        crate::vector::gpu::dispatch_vec_search_rabitq(
                            &gpu.stream, vk, gpu_codes, gpu_norms,
                            query_vector, quantizer, num_docs, vq.k, 4,
                            &vec_idx.vectors, &vec_idx.metric, &mut bufs,
                        )
                    } else {
                        crate::vector::gpu::dispatch_vec_search_f32(
                            &gpu.stream, vk, gpu_vecs, query_vector,
                            num_docs, vec_idx.num_dim as u32, &vec_idx.metric, vq.k,
                            &mut bufs,
                        )
                    };
                    drop(bufs);
                    match result {
                        Ok(r) => {
                            tracing::debug!(num_results = r.len(), "vector search used GPU path");
                            vector_results = Some(r);
                        }
                        Err(e) => tracing::warn!("GPU vector search failed, falling back to CPU: {e}"),
                    }
                }
            }
        }

        // CPU path: HNSW or brute-force flat scan
        let vector_results = vector_results.unwrap_or_else(|| {
            if use_flat {
                tracing::debug!("vector search using brute-force flat scan (filtered set below cutoff)");
                vec_idx.flat_search(query_vector, vq.k, filter_bitmap.as_ref())
            } else {
                tracing::debug!("vector search using CPU HNSW (ef_search={})", ef_search);
                vec_idx.search(query_vector, vq.k, ef_search, filter_bitmap.as_ref())
            }
        });

        // Apply distance_threshold filter
        let vector_results: Vec<(DocId, f32)> = if let Some(threshold) = vq.distance_threshold {
            let higher_is_better = vec_idx.metric.higher_is_better();
            vector_results.into_iter().filter(|&(_, dist)| {
                if higher_is_better { dist >= threshold } else { dist <= threshold }
            }).collect()
        } else {
            vector_results
        };

        // Store distances for response
        for &(doc_id, dist) in &vector_results {
            vector_distances.insert(doc_id, dist);
        }

        if is_text_query {
            if let Some(alpha) = vq.alpha {
                // Alpha-weighted hybrid: interpolate normalized scores
                let lexical_results = lexical_ranking(&scores);

                // Min-max normalize both score sets to [0, 1]
                let normalize = |results: &[(DocId, f32)]| -> FxHashMap<DocId, f32> {
                    if results.is_empty() { return FxHashMap::default(); }
                    let min = results.iter().map(|r| r.1).fold(f32::INFINITY, f32::min);
                    let max = results.iter().map(|r| r.1).fold(f32::NEG_INFINITY, f32::max);
                    let range = (max - min).max(1e-9);
                    results.iter().map(|&(id, s)| (id, (s - min) / range)).collect()
                };

                let text_norm = normalize(&lexical_results);
                // For vector distances: lower = better for cosine/l2, higher = better for ip
                let vec_norm = {
                    let higher = vec_idx.metric.higher_is_better();
                    let pairs: Vec<_> = vector_results.iter()
                        .map(|&(id, d)| (id, if higher { d } else { -d }))
                        .collect();
                    if !pairs.is_empty() {
                        let min = pairs.iter().map(|r| r.1).fold(f32::INFINITY, f32::min);
                        let max = pairs.iter().map(|r| r.1).fold(f32::NEG_INFINITY, f32::max);
                        let range = (max - min).max(1e-9);
                        pairs.iter().map(|&(id, s)| (id, (s - min) / range)).collect()
                    } else {
                        FxHashMap::default()
                    }
                };

                // Merge: alpha * vector + (1 - alpha) * text
                let mut all_ids: FxHashMap<DocId, f32> = FxHashMap::default();
                for (&id, &ts) in &text_norm {
                    *all_ids.entry(id).or_default() += (1.0 - alpha) * ts;
                }
                for (&id, &vs) in &vec_norm {
                    *all_ids.entry(id).or_default() += alpha * vs;
                }

                let mut fused: Vec<(DocId, f32)> = all_ids.into_iter().collect();
                fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

                candidates = Bitmap::new();
                scores.clear();
                for (doc_id, score) in &fused {
                    candidates.add(*doc_id);
                    scores.insert(*doc_id, float_key(*score));
                    rrf_scores.insert(*doc_id, *score);
                }
            } else {
                // Default hybrid: RRF fusion of lexical + vector
                let lexical_results = lexical_ranking(&scores);

                let fused = crate::vector::hybrid::reciprocal_rank_fusion(
                    &lexical_results,
                    &vector_results,
                    60.0,
                );

                candidates = Bitmap::new();
                scores.clear();
                for (doc_id, rrf_score) in &fused {
                    candidates.add(*doc_id);
                    scores.insert(*doc_id, float_key(*rrf_score));
                    rrf_scores.insert(*doc_id, *rrf_score);
                }
            }
        } else {
            // Vector-only search (no text query)
            candidates = Bitmap::new();
            scores.clear();
            let higher_is_better = vec_idx.metric.higher_is_better();
            for &(doc_id, dist) in &vector_results {
                candidates.add(doc_id);
                let sort_score = if higher_is_better { dist } else { -dist };
                scores.insert(doc_id, float_key(sort_score));
            }
        }
    }

    let vector_ms = t_vector.elapsed().as_secs_f64() * 1000.0;

    // If vector search produced results, treat as scored even without text query
    let has_scores = is_text_query || vector_params.is_some();

    // The pinned documents leave the organic results; the results are both.
    let pinned_docs: Bitmap = pinned.iter().map(|(_, p)| p.doc).collect();
    if !pinned.is_empty() {
        candidates.andnot_inplace(&pinned_docs);
    }
    let results_union = (!pinned.is_empty()).then(|| &candidates | &pinned_docs);
    let results: &Bitmap = results_union.as_ref().unwrap_or(&candidates);

    let found = match params.limit_hits {
        Some(limit) => (results.cardinality() as usize).min(limit),
        None => results.cardinality() as usize,
    };

    // Facets over the results. Those over every segment's at once (`late_facets`: top_k ones and
    // the facet_query field's) wait for the ranking; over several segments the merge counts them.
    let t_facet = Instant::now();
    // one of several segments: every value, for the merge to count and cut
    let max_facet_values = if multi || params.union.is_some() { usize::MAX } else { params.max_facet_values.unwrap_or(10) };
    // a search of a union removing duplicates: its results, for the union's facets and found
    let distinct = params.union == Some(UnionPart::Distinct);
    let group_keys = is_grouped.then(|| grouping::GroupKeys::new(index, schema, &group_fields, group_missing));
    let has_facets = !facet_by.local.is_empty() || !facet_by.joins.is_empty();
    let doc_groups: Option<facet::DocGroups> = group_keys.as_ref().filter(|_| has_facets).map(|k| {
        if multi {
            facet::DocGroups::new(results, k.ids())
        } else {
            facet::DocGroups::new(results, |d| k.hash(d))
        }
    });
    let is_late = |f: &facet::FacetSpec| f.top_k || facet_query.is_some_and(|q| q.fid == f.fid);
    let regular: Vec<&facet::FacetSpec> = facet_by.local.iter().filter(|f| !is_late(f)).collect();
    let late: Vec<&facet::FacetSpec> = facet_by.local.iter().filter(|f| is_late(f)).collect();
    let how = facet::Counting { max: max_facet_values, multi, groups: doc_groups.as_ref(), query: None };
    let mut facet_counts = compute_facets(&regular, results, index, how);
    if !facet_by.joins.is_empty() {
        facet_counts.extend(compute_join_facets(&facet_by.joins, results, index, join_context, how)?);
    }
    attach_parents(&mut facet_counts, params, schema, index, results);
    // the first hits of the ordering, for top_k facets
    let top = late.iter().any(|f| f.top_k).then(|| top_k_size(params));
    let mut tops = Bitmap::new();

    // What a hit highlights: highlight_fields, else the query_by fields (objects and patterns
    // expanded) the hits return (include_fields, exclude_fields), string fields only; for a text
    // query.
    let field_names = |list: &str| -> Vec<FieldId> {
        list.split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .filter_map(|n| expand_query_by(parse_query_by_entry(n).0, schema).ok())
            .flatten()
            .collect()
    };
    let highlighter: Option<hl::Highlighter<'_>> = text.as_ref().filter(|t| is_text_query && t.words > 0).map(|t| {
        let ids: Vec<FieldId> = match params.highlight_fields.as_deref() {
            Some(list) => field_names(list),
            None => params
                .query_by
                .iter()
                .filter_map(|e| expand_query_by(parse_query_by_entry(e).0, schema).ok())
                .flatten()
                .filter(|&fid| params.projection.allows(&schema.fields[fid as usize].name))
                .collect(),
        };
        let full: Vec<FieldId> = params.highlight_full_fields.as_deref().map(field_names).unwrap_or_default();
        let mut seen = rustc_hash::FxHashSet::default();
        let fields = ids
            .into_iter()
            .filter(|f| seen.insert(*f))
            .map(|fid| hl::Field { def: &schema.fields[fid as usize], full: full.contains(&fid) })
            .filter(|f| f.def.field_type.is_searchable())
            .collect();
        hl::Highlighter {
            fields,
            terms: &t.marks.terms,
            phrases: &t.marks.phrases,
            symbols_to_index: &schema.symbols_to_index,
            token_separators: &schema.token_separators,
            language: schema.default_language,
            start_tag: params.highlight_start_tag.as_deref().unwrap_or("<mark>"),
            end_tag: params.highlight_end_tag.as_deref().unwrap_or("</mark>"),
            affix_num_tokens: params.highlight_affix_num_tokens.unwrap_or(4),
            snippet_threshold: params.snippet_threshold.unwrap_or(30),
        }
    });

    let facet_ms = t_facet.elapsed().as_secs_f64() * 1000.0;

    let t_sort = Instant::now();
    // A hit, with its text match, text_match_info and highlights.
    let registry = schema.registry.load();
    let make_hit = |doc_id: DocId| -> Option<Hit> {
        let document = index.doc_json(doc_id, schema, &registry)?;
        let (highlight, highlights) = match &highlighter {
            Some(h) => h.highlight(&document),
            None => (serde_json::Value::Object(serde_json::Map::new()), Vec::new()),
        };
        let (text_match, text_match_info) = match &text {
            Some(t) => {
                let score = score_of(&scores, doc_id);
                let mut info = TextMatchInfo::new(score, t.words);
                if params.explain_matches == Some(true) {
                    info.matches = Some(t.marks.explain(index, doc_id));
                }
                (Some(score), Some(info))
            }
            None => (None, None),
        };
        Some(Hit {
            text_match,
            highlights: (params.enable_highlight_v1 != Some(false)).then_some(highlights),
            highlight,
            text_match_info,
            hybrid_search_info: rrf_scores.get(&doc_id).map(|&s| HybridSearchInfo { rank_fusion_score: s }),
            geo_distance_meters: compute_geo_distances(doc_id, &sort_specs, index, schema),
            vector_distance: vector_distances.get(&doc_id).copied(),
            seq: u64::from(index.base_doc_id) + u64::from(doc_id),
            seq_id: index.seq_id(doc_id),
            sort_key: if multi || params.union.is_some() {
                hit_sort_key(doc_id, &sort_specs, &scores, index, has_scores, join_context, &vector_distances)
            } else {
                Vec::new()
            },
            ..Hit::new(document)
        })
    };
    // A pinned hit: highlighted, curated, without a match score.
    let pinned_hit = |doc_id: DocId| -> Option<Hit> {
        make_hit(doc_id).map(|h| Hit { curated: Some(true), text_match: None, text_match_info: None, hybrid_search_info: None, vector_distance: None, ..h })
    };
    let (a, b) = (params.start(), params.start() + params.per_page);
    if is_grouped {
        let group_keys = group_keys.as_ref().expect("grouped");
        // Two-phase grouped search:
        // Phase 1: Group by DocId (lightweight — no Hit materialization)
        let mut grouped_ids = if sort_specs.is_empty() && !has_scores {
            // Fast path: single scalar String/Text field → iterate the field's values
            let scalar_string_values = match group_fields.as_slice() {
                [fid] if matches!(schema.fields[*fid as usize].field_type, FieldType::Text | FieldType::String) => {
                    index.values.get(fid).filter(|v| v.doc_ords().is_some_and(|o| !o.is_multi()))
                }
                _ => None,
            };
            match scalar_string_values {
                Some(idx) => grouping::group_doc_ids_from_values(&candidates, idx, group_limit, group_missing),
                None => grouping::group_doc_ids_from_bitmap(&candidates, group_keys, group_limit),
            }
        } else if sort_specs.is_empty() {
            // Text query with no explicit sort_by: groups by their best text match
            grouping::group_doc_ids_scored(&candidates, &scores, group_keys, group_limit)
        } else {
            let sorted_all = sort_all_candidates(
                &candidates,
                &sort_specs,
                &scores,
                index,
                has_scores,
                join_context,
                &vector_distances,
            );
            grouping::group_doc_ids(sorted_all, group_keys, group_limit)
        };

        // Sort groups by _group_found if requested
        if has_group_found_sort {
            let gf_spec = sort_specs
                .iter()
                .find(|s| matches!(s.field, SortField::GroupFound))
                .unwrap();
            match gf_spec.order {
                SortOrder::Desc => grouped_ids.groups.sort_by(|a, b| b.2.cmp(&a.2)),
                SortOrder::Asc => grouped_ids.groups.sort_by(|a, b| a.2.cmp(&b.2)),
            }
        }
        let group_id = |key: &[serde_json::Value], docs: &[(DocId, u64)]| docs.first().map_or_else(|| grouping::key_hash(key), |d| group_keys.key_id(key, d.0));
        let materialize = |(group_key, doc_ids_scores, group_found): (Vec<serde_json::Value>, Vec<(DocId, u64)>, usize)| {
            let id = group_id(&group_key, &doc_ids_scores);
            let hits: Vec<Hit> = doc_ids_scores.into_iter().filter_map(|(doc_id, _)| make_hit(doc_id)).collect();
            grouping::GroupHit { group_key, hits, found: group_found, id }
        };
        let found_docs = grouped_ids.found_docs + pinned.len();
        let mut result_pinned = Vec::new();
        let (total_groups, group_counts, grouped_hits) = if multi {
            // one of several segments: its organic groups and pinned hits, every group's count,
            // for the merge to place them
            let group_counts: Vec<(u64, usize)> = grouped_ids.groups.iter().map(|(key, docs, found)| (group_id(key, docs), *found)).collect();
            result_pinned = pinned
                .iter()
                .filter_map(|&(rank, p)| Some(PinnedHit { rank, hit: pinned_hit(p.doc)?, group: Some((group_keys.id(p.doc), group_keys.key(p.doc))) }))
                .collect();
            let total = grouped_ids.groups.len();
            let page: Vec<grouping::GroupHit> = grouped_ids.groups.into_iter().skip(a).take(params.per_page).map(materialize).collect();
            (total, group_counts, page)
        } else {
            // the pinned groups: the pinned documents by group, each where its first goes; an
            // organic group of their values joins them
            let mut pgroups: Vec<(u64, Vec<DocId>, usize)> = Vec::new();
            for &(_, p) in &pinned {
                let h = group_keys.hash(p.doc);
                match pgroups.iter_mut().find(|g| g.0 == h) {
                    Some(g) => g.1.push(p.doc),
                    None => pgroups.push((h, vec![p.doc], p.position)),
                }
            }
            let mut joined: FxHashMap<u64, (Vec<(DocId, u64)>, usize)> = FxHashMap::default();
            if !pgroups.is_empty() {
                grouped_ids.groups.retain_mut(|(_, docs, found)| match docs.first().map(|d| group_keys.hash(d.0)).filter(|h| pgroups.iter().any(|g| g.0 == *h)) {
                    Some(h) => {
                        joined.insert(h, (std::mem::take(docs), *found));
                        false
                    }
                    None => true,
                });
            }
            let layout = PinLayout::new(pgroups.iter().map(|g| g.2), grouped_ids.groups.len());
            if let Some(k) = top {
                for (_, docs, _) in grouped_ids.groups.iter().take(layout.organic(0, k).end) {
                    docs.iter().for_each(|d| tops.add(d.0));
                }
                for (g, _) in pgroups.iter().zip(&layout.slots).filter(|&(_, &slot)| slot < k) {
                    g.1.iter().for_each(|&d| tops.add(d));
                    if let Some((docs, _)) = joined.get(&g.0) {
                        docs.iter().take(group_limit.saturating_sub(g.1.len())).for_each(|d| tops.add(d.0));
                    }
                }
            }
            let mut placed: Vec<Option<grouping::GroupHit>> = pgroups
                .into_iter()
                .map(|(h, docs, _)| {
                    let (organic, organic_found) = joined.remove(&h).unwrap_or_default();
                    let mut hits: Vec<Hit> = docs.iter().filter_map(|&d| pinned_hit(d)).collect();
                    hits.extend(organic.iter().take(group_limit.saturating_sub(hits.len())).filter_map(|&(d, _)| make_hit(d)));
                    hits.truncate(group_limit.max(1));
                    Some(grouping::GroupHit { group_key: group_keys.key(docs[0]), id: group_keys.id(docs[0]), found: docs.len() + organic_found, hits })
                })
                .collect();
            let range = layout.organic(a, b);
            let organic_page = grouped_ids.groups.into_iter().skip(range.start).take(range.len()).map(materialize);
            (layout.total, Vec::new(), layout.page(a, b, organic_page, |j| placed[j].take()))
        };
        if !late.is_empty() && !multi {
            facet_counts.extend(late_facets(&late, &[index], std::slice::from_ref(results), top.map(|_| std::slice::from_ref(&tops)), facet_query, std::slice::from_ref(&doc_groups), params, schema));
            facet_counts.sort_by_key(|f| f.position);
        }

        let sort_ms = t_sort.elapsed().as_secs_f64() * 1000.0;
        let total_ms = start.elapsed().as_secs_f64() * 1000.0;
        let query_type = classify_query_type(is_text_query, vector_params.is_some());
        tracing::debug!(
            filter_ms = format!("{filter_ms:.2}").as_str(),
            text_ms = format!("{text_ms:.2}").as_str(),
            vector_ms = format!("{vector_ms:.2}").as_str(),
            facet_ms = format!("{facet_ms:.2}").as_str(),
            sort_ms = format!("{sort_ms:.2}").as_str(),
            total_ms = format!("{total_ms:.2}").as_str(),
            query_type,
            candidates_after_filter,
            text_matches = text_match_count,
            scored_count,
            found,
            returned_groups = grouped_hits.len(),
            grouped = true,
            "search timing"
        );

        let (page, offset) = params.page_or_offset();
        Ok((SearchResponse::Grouped(GroupedSearchResult {
            found: total_groups,
            out_of: live_total_docs as usize,
            page,
            offset,
            found_docs,
            grouped_hits,
            facet_counts,
            search_time_ms: start.elapsed().as_millis() as u64,
            request_params: None,
            metadata: None,
            conversation: None,
            search_cutoff,
            group_counts,
            pinned: result_pinned,
            docs: (multi && !late.is_empty()).then(|| results.clone()),
        }), SearchTimings { filter_ms, text_ms, vector_ms, facet_ms, sort_ms, deferred_filter_ms }))
    } else {
        let mut result_pinned = Vec::new();
        let hits: Vec<Hit> = if multi {
            // one of several segments: its first organic hits and its pinned hits, for the merge
            // to place
            result_pinned = pinned.iter().filter_map(|&(rank, p)| Some(PinnedHit { rank, hit: pinned_hit(p.doc)?, group: None })).collect();
            paginate_candidates(&candidates, &sort_specs, &scores, index, has_scores, a, params.per_page, join_context, &vector_distances)
                .into_iter()
                .filter_map(|(doc_id, _)| make_hit(doc_id))
                .collect()
        } else {
            let layout = PinLayout::new(pinned.iter().map(|(_, p)| p.position), candidates.cardinality() as usize);
            let range = layout.organic(a, b);
            // the page's organic hits, or with top_k facets all the first ones (the page among them)
            let (ranked, from) = match top {
                Some(k) => (paginate_candidates(&candidates, &sort_specs, &scores, index, has_scores, 0, layout.organic(0, k).end, join_context, &vector_distances), 0),
                None => (paginate_candidates(&candidates, &sort_specs, &scores, index, has_scores, range.start, range.len(), join_context, &vector_distances), range.start),
            };
            if let Some(k) = top {
                ranked.iter().for_each(|&(d, _)| tops.add(d));
                pinned.iter().enumerate().filter(|&(j, _)| layout.slots[j] < k).for_each(|(_, (_, p))| tops.add(p.doc));
            }
            let organic_page = ranked.iter().skip(range.start - from).take(range.len()).filter_map(|&(doc_id, _)| make_hit(doc_id));
            layout.page(a, b, organic_page, |j| pinned_hit(pinned[j].1.doc))
        };
        if !late.is_empty() && !multi {
            facet_counts.extend(late_facets(&late, &[index], std::slice::from_ref(results), top.map(|_| std::slice::from_ref(&tops)), facet_query, std::slice::from_ref(&doc_groups), params, schema));
            facet_counts.sort_by_key(|f| f.position);
        }

        let sort_ms = t_sort.elapsed().as_secs_f64() * 1000.0;
        let total_ms = start.elapsed().as_secs_f64() * 1000.0;
        let query_type = classify_query_type(is_text_query, vector_params.is_some());
        tracing::debug!(
            filter_ms = format!("{filter_ms:.2}").as_str(),
            text_ms = format!("{text_ms:.2}").as_str(),
            vector_ms = format!("{vector_ms:.2}").as_str(),
            facet_ms = format!("{facet_ms:.2}").as_str(),
            sort_ms = format!("{sort_ms:.2}").as_str(),
            total_ms = format!("{total_ms:.2}").as_str(),
            query_type,
            candidates_after_filter,
            text_matches = text_match_count,
            scored_count,
            found,
            returned = hits.len(),
            "search timing"
        );

        let (page, offset) = params.page_or_offset();
        Ok((SearchResponse::Ungrouped(SearchResult {
            found,
            out_of: live_total_docs as usize,
            page,
            offset,
            hits,
            facet_counts,
            search_time_ms: start.elapsed().as_millis() as u64,
            request_params: None,
            metadata: None,
            conversation: None,
            search_cutoff,
            pinned: result_pinned,
            docs: (multi && (!late.is_empty() || distinct)).then(|| results.clone()),
            segment_docs: if distinct && !multi { vec![results.clone()] } else { Vec::new() },
        }), SearchTimings { filter_ms, text_ms, vector_ms, facet_ms, sort_ms, deferred_filter_ms }))
    }
}

/// The first hits top_k facets count: 250, or the hits up to the page's end when more.
fn top_k_size(params: &SearchParams) -> usize {
    (params.start() + params.per_page).max(TOP_K_HITS)
}

/// Hits a top_k facet counts at least.
const TOP_K_HITS: usize = 250;

/// A pinned document of a search: its segment, id and position (1-based).
#[derive(Clone, Copy, Debug)]
struct Pin {
    seg: usize,
    doc: DocId,
    position: usize,
}

/// A pinned hit one of several segments found, for the merge to place: its rank among the
/// search's pins and, grouped, its group (`GroupKeys::id`) and key.
pub struct PinnedHit {
    pub rank: usize,
    pub hit: Hit,
    pub group: Option<(u64, Vec<serde_json::Value>)>,
}

/// pinned_hits: each document found in the segment holding it live (with
/// filter_curated_hits only one passing the filter), by position (the given order among equal
/// ones); a document named twice counts once, a hidden one (hidden_hits) not at all.
fn resolve_pins(params: &SearchParams, segs: &[(&Index, Option<&Bitmap>)], texts: &[SegmentText]) -> Vec<Pin> {
    let Some(pinned) = params.pinned_hits.as_deref() else { return Vec::new() };
    let hidden: rustc_hash::FxHashSet<&str> = params.hidden_hits.as_deref().unwrap_or("").split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    let mut seen = rustc_hash::FxHashSet::default();
    let mut pins = Vec::new();
    for (ext_id, position) in parse_pinned_entries(pinned) {
        if position == 0 || hidden.contains(ext_id) || !seen.insert(ext_id) {
            continue;
        }
        let found = segs.iter().zip(texts).enumerate().find_map(|(seg, (&(index, deleted), text))| {
            pinned_doc(index, deleted, text.filter_bitmap.as_ref(), params, ext_id).map(|doc| Pin { seg, doc, position })
        });
        pins.extend(found);
    }
    pins.sort_by_key(|p| p.position);
    pins
}

/// Where pinned hits go in the one ordering of a search's results: each at its position (1-based), the organic hits in their order around them. A pin asking for
/// the place of one before it, or an earlier one, goes right after it, and pins past the end of the
/// results after the last hit.
struct PinLayout {
    /// Each pin's place (0-based), ascending, the pins in the order of their positions.
    slots: Vec<usize>,
    /// The results, organic and pinned.
    total: usize,
}

impl PinLayout {
    fn new(positions: impl IntoIterator<Item = usize>, organic: usize) -> Self {
        let mut slots: Vec<usize> = Vec::new();
        for p in positions {
            let j = slots.len();
            let at = slots.last().map_or(p.saturating_sub(1), |&prev| p.saturating_sub(1).max(prev + 1));
            slots.push(at.min(organic + j));
        }
        let total = organic + slots.len();
        PinLayout { slots, total }
    }

    /// Pins placed before place `i`.
    fn before(&self, i: usize) -> usize {
        self.slots.partition_point(|&s| s < i)
    }

    /// The organic hits places `a..b` hold, as a range of the organic order.
    fn organic(&self, a: usize, b: usize) -> std::ops::Range<usize> {
        let (a, b) = (a.min(self.total), b.min(self.total));
        a - self.before(a)..(b - self.before(b)).max(a - self.before(a))
    }

    /// Places `a..b`: pin `j` (from `pinned`) where it goes, the organic hits of `organic(a, b)` in
    /// order between.
    fn page<T>(&self, a: usize, b: usize, organic: impl IntoIterator<Item = T>, mut pinned: impl FnMut(usize) -> Option<T>) -> Vec<T> {
        let (a, b) = (a.min(self.total), b.min(self.total));
        let mut organic = organic.into_iter();
        let mut j = self.before(a);
        let mut out = Vec::with_capacity(b - a);
        for i in a..b {
            if self.slots.get(j) == Some(&i) {
                out.extend(pinned(j));
                j += 1;
            } else {
                out.extend(organic.next());
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Multi-segment merge
//
// `execute_with_stores_gpu_join` calls `search_one_segment` for each segment
// in an `IndexBundle` and passes the per-segment responses here. The merge
// must produce a SearchResponse that's globally correct: the right `found`
// count, the right top-K hits across all segments, summed facets, etc.
//
// Single-segment collections never reach this code path: the dispatcher in
// `execute_with_stores_gpu_join` short-circuits to a direct call into
// `search_one_segment`.
// ---------------------------------------------------------------------------

fn merge_segment_responses(
    per_segment: Vec<(SearchResponse, SearchTimings)>,
    params: &SearchParams,
    schema: &Schema,
    global_total_docs: u32,
    global_offset: usize,
    per_page: usize,
) -> Result<(SearchResponse, SearchTimings), String> {
    // Distinguish grouped vs ungrouped from the first response — all responses
    // from a single search request are the same shape.
    let is_grouped = matches!(
        per_segment.first().map(|(r, _)| r),
        Some(SearchResponse::Grouped(_))
    );

    if is_grouped {
        merge_grouped_responses(per_segment, params, schema, global_total_docs, global_offset, per_page)
    } else {
        merge_ungrouped_responses(per_segment, params, schema, global_total_docs, global_offset, per_page)
    }
}

fn merge_ungrouped_responses(
    per_segment: Vec<(SearchResponse, SearchTimings)>,
    params: &SearchParams,
    schema: &Schema,
    global_total_docs: u32,
    global_offset: usize,
    per_page: usize,
) -> Result<(SearchResponse, SearchTimings), String> {
    let mut found_total: usize = 0;
    let mut all_hits: Vec<Hit> = Vec::new();
    let mut facet_acc: FxHashMap<usize, FacetResult> = FxHashMap::default();
    let mut search_cutoff = false;
    let mut search_time_ms: u64 = 0;
    let mut request_params: Option<serde_json::Value> = None;
    let mut merged_timings = SearchTimings {
        filter_ms: 0.0,
        text_ms: 0.0,
        vector_ms: 0.0,
        facet_ms: 0.0,
        sort_ms: 0.0,
        deferred_filter_ms: 0.0,
    };

    // Later segments hold later inserted documents: on a tie they rank first, so they go in first
    // and the sorts below are stable.
    for (response, timings) in per_segment.into_iter().rev() {
        let SearchResponse::Ungrouped(result) = response else {
            return Err("multi-segment merge: expected ungrouped response, got grouped".into());
        };
        found_total += result.found;
        all_hits.extend(result.hits);
        for facet in result.facet_counts {
            merge_facet_into(&mut facet_acc, facet);
        }
        search_cutoff |= result.search_cutoff;
        // Each segment runs sequentially today so total search time is the sum.
        // (Once segments run in parallel, switch to max.)
        search_time_ms += result.search_time_ms;
        if request_params.is_none() {
            request_params = result.request_params;
        }
        merged_timings.filter_ms += timings.filter_ms;
        merged_timings.text_ms += timings.text_ms;
        merged_timings.vector_ms += timings.vector_ms;
        merged_timings.facet_ms += timings.facet_ms;
        merged_timings.sort_ms += timings.sort_ms;
        merged_timings.deferred_filter_ms += timings.deferred_filter_ms;
    }

    // Sort the unioned hits using a precise comparator that honors any
    // `sort_by` clause from the query. The comparator extracts sort keys
    // from each Hit (text_match, geo_distance_meters, vector_distance for
    // intrinsic keys; document JSON for arbitrary schema fields).
    let sort_specs = sort_specs(params, schema).unwrap_or_default();
    all_hits.sort_by(|a, b| hit_order(a, b, &sort_specs, schema));

    // Take the requested page from the merged top-K.
    let page_hits: Vec<Hit> = all_hits
        .into_iter()
        .skip(global_offset)
        .take(per_page)
        .collect();

    let facet_counts = finish_facets(facet_acc, params);

    Ok((
        SearchResponse::Ungrouped(SearchResult {
            found: found_total,
            out_of: global_total_docs as usize,
            page: params.page_or_offset().0,
            offset: params.page_or_offset().1,
            hits: page_hits,
            facet_counts,
            search_time_ms,
            request_params,
            metadata: None,
            conversation: None,
            search_cutoff,
            pinned: Vec::new(),
            docs: None,
            segment_docs: Vec::new(),
        }),
        merged_timings,
    ))
}

fn merge_grouped_responses(
    per_segment: Vec<(SearchResponse, SearchTimings)>,
    params: &SearchParams,
    schema: &Schema,
    global_total_docs: u32,
    global_offset: usize,
    per_page: usize,
) -> Result<(SearchResponse, SearchTimings), String> {
    // Precise grouped merge: groups with the same key across segments are
    // merged into one group whose `found` is the sum and whose `hits` are
    // the top `group_limit` after re-ranking with the user's sort_specs.
    // Then groups are sorted by their best hit's score and paginated.
    let group_limit = params.group_limit.unwrap_or(3).max(1);
    let sort_specs = sort_specs(params, schema).unwrap_or_default();

    // the groups by their id
    let mut group_acc: FxHashMap<u64, grouping::GroupHit> = FxHashMap::default();
    let mut facet_acc: FxHashMap<usize, FacetResult> = FxHashMap::default();
    let mut found_docs_total: usize = 0;
    let mut search_cutoff = false;
    let mut search_time_ms: u64 = 0;
    let mut request_params: Option<serde_json::Value> = None;
    let mut merged_timings = SearchTimings {
        filter_ms: 0.0,
        text_ms: 0.0,
        vector_ms: 0.0,
        facet_ms: 0.0,
        sort_ms: 0.0,
        deferred_filter_ms: 0.0,
    };

    // every group's documents over all segments
    let mut counts: FxHashMap<u64, usize> = FxHashMap::default();
    for (response, timings) in per_segment {
        let SearchResponse::Grouped(group) = response else {
            return Err("multi-segment merge: expected grouped response, got ungrouped".into());
        };
        for &(h, n) in &group.group_counts {
            *counts.entry(h).or_default() += n;
        }
        found_docs_total += group.found_docs;
        search_cutoff |= group.search_cutoff;
        search_time_ms += group.search_time_ms;
        if request_params.is_none() {
            request_params = group.request_params;
        }
        for facet in group.facet_counts {
            merge_facet_into(&mut facet_acc, facet);
        }
        for incoming in group.grouped_hits {
            match group_acc.entry(incoming.id) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(incoming);
                }
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    let existing = slot.get_mut();
                    existing.hits.extend(incoming.hits);
                    // Re-sort and trim to group_limit.
                    existing.hits.sort_by(|a, b| hit_order(a, b, &sort_specs, schema));
                    existing.hits.truncate(group_limit);
                }
            }
        }
        merged_timings.filter_ms += timings.filter_ms;
        merged_timings.text_ms += timings.text_ms;
        merged_timings.vector_ms += timings.vector_ms;
        merged_timings.facet_ms += timings.facet_ms;
        merged_timings.sort_ms += timings.sort_ms;
        merged_timings.deferred_filter_ms += timings.deferred_filter_ms;
    }

    let found_total = counts.len();
    // Order groups: each group's "best hit" determines its position. The
    // best hit is the one ranked first under the same sort criteria used
    // within the group.
    let mut grouped_hits: Vec<grouping::GroupHit> = group_acc.into_values().collect();
    for g in &mut grouped_hits {
        g.found = counts.get(&g.id).copied().unwrap_or(g.found);
    }
    grouped_hits.sort_by(|ga, gb| match (ga.hits.first(), gb.hits.first()) {
        (Some(a), Some(b)) => hit_order(a, b, &sort_specs, schema),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });
    // _group_found: by the groups' documents over all segments, as one segment re-sorts them
    if let Some(gf) = sort_specs.iter().find(|s| matches!(s.field, SortField::GroupFound)) {
        match gf.order {
            SortOrder::Desc => grouped_hits.sort_by(|a, b| b.found.cmp(&a.found)),
            SortOrder::Asc => grouped_hits.sort_by(|a, b| a.found.cmp(&b.found)),
        }
    }

    // Apply pagination at the group level (consistent with single-segment
    // behavior — `page`/`per_page` paginate groups, not raw hits).
    let page_groups: Vec<grouping::GroupHit> = grouped_hits
        .into_iter()
        .skip(global_offset)
        .take(per_page)
        .collect();

    Ok((
        SearchResponse::Grouped(grouping::GroupedSearchResult {
            found: found_total,
            out_of: global_total_docs as usize,
            page: params.page_or_offset().0,
            offset: params.page_or_offset().1,
            found_docs: found_docs_total,
            grouped_hits: page_groups,
            facet_counts: finish_facets(facet_acc, params),
            search_time_ms,
            request_params,
            metadata: None,
            conversation: None,
            search_cutoff,
            // every group's documents, for placing pinned groups
            group_counts: counts.into_iter().collect(),
            pinned: Vec::new(),
            docs: None,
        }),
        merged_timings,
    ))
}

/// The order of hits from different segments, as one segment orders them: by the sort keys their
/// segments attached (`hit_sort_key`), then the later document (`_seq_id`) first.
fn hit_order(a: &Hit, b: &Hit, sort_specs: &[SortSpec], schema: &Schema) -> Ordering {
    compare_sort_keys(a, b, sort_specs, schema).then(b.seq_id.cmp(&a.seq_id))
}

fn compare_sort_keys(a: &Hit, b: &Hit, sort_specs: &[SortSpec], schema: &Schema) -> Ordering {
    if sort_specs.is_empty() {
        // the segments' score, highest first
        return match (a.sort_key.first(), b.sort_key.first()) {
            (Some(x), Some(y)) => y.order(x),
            _ => Ordering::Equal,
        };
    }
    for (i, spec) in sort_specs.iter().enumerate() {
        let (x, y) = (a.sort_key.get(i).unwrap_or(&SortValue::Missing), b.sort_key.get(i).unwrap_or(&SortValue::Missing));
        let descending = matches!(spec.order, SortOrder::Desc);
        // where a field sort puts documents without a value (`SortColumn::missing_first`)
        let missing_first = match (&spec.field, spec.missing) {
            (SortField::Field(_), Missing::First) => true,
            (SortField::Field(fid), Missing::Normal) => {
                descending && schema.fields.get(*fid as usize).is_some_and(|f| matches!(f.field_type, FieldType::String | FieldType::Text))
            }
            _ => false,
        };
        let cmp = match (x, y) {
            (SortValue::Missing, SortValue::Missing) => Ordering::Equal,
            (SortValue::Missing, _) => if missing_first { Ordering::Less } else { Ordering::Greater },
            (_, SortValue::Missing) => if missing_first { Ordering::Greater } else { Ordering::Less },
            _ if descending => y.order(x),
            _ => x.order(y),
        };
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    Ordering::Equal
}

/// A hit's sort keys for merging several segments' hits: per sort spec its document's value (a
/// string as itself, a number as its order key, a score or a distance); without a sort its score.
fn hit_sort_key(
    doc: DocId,
    specs: &[SortSpec],
    scores: &Scores,
    index: &Index,
    has_scores: bool,
    join_context: Option<&JoinContext<'_>>,
    vector_distances: &FxHashMap<DocId, f32>,
) -> Vec<SortValue> {
    if specs.is_empty() {
        return if has_scores { vec![SortValue::Score(score_of(scores, doc))] } else { Vec::new() };
    }
    specs
        .iter()
        .map(|spec| match &spec.field {
            SortField::TextMatch => SortValue::Score(score_of(scores, doc)),
            SortField::Field(fid) => match SortColumn::of(index, *fid) {
                Some(SortColumn::Values(v)) => {
                    v.doc_ord(doc).and_then(|o| if v.is_str() { v.str_at(o).map(|s| SortValue::Str(s.into())) } else { v.key_at(o).map(SortValue::Int) })
                }
                Some(SortColumn::Bool(b)) => b.value(doc).map(|x| SortValue::Int(i64::from(x))),
                None => None,
            }
            .unwrap_or(SortValue::Missing),
            SortField::Decay(d) => SortValue::Float(index.number_i64(d.field_id, doc).map_or(0.0, |v| d.sort_score(v))),
            SortField::GeoDistance(fid, lat, lng) => {
                let centre = crate::index::geo::sort_centre(*lat, *lng);
                SortValue::Int(i64::from(index.geo_indexes.get(fid).map_or(crate::index::geo::MISSING_M, |g| g.distance_m(doc, centre))))
            }
            SortField::VectorDistance => SortValue::Float(f64::from(vector_distances.get(&doc).copied().unwrap_or(f32::MAX))),
            SortField::SeqId => SortValue::Int(index.seq_id(doc) as i64),
            SortField::GroupFound => SortValue::Missing,
            SortField::JoinSort { target_collection, target_field } => {
                join_sort_value(doc, index, join_context, target_collection, target_field).map_or(SortValue::Missing, SortValue::Float)
            }
        })
        .collect()
}

/// A document's value of a referenced collection's field (of the first live document it references).
fn join_sort_value(doc: DocId, index: &Index, join_context: Option<&JoinContext<'_>>, target_collection: &str, target_field: &str) -> Option<f64> {
    let target = join_context?.target(target_collection)?;
    let fid = *target.schema.name_to_id.get(target_field)?;
    let refs = JoinContext::references(index, &target, target_collection)?;
    let (s, d) = Referenced::new(refs, &target.view).first(doc)?;
    target.view.segment(s).0.number_f64(fid, d)
}

/// Add a segment's facet to the merge: counts summed per value (a grouped search's groups united),
/// stats combined, a value's `sort_by:<field>` value the extreme of the segments'. Segments of a
/// multi-segment search count every value (`SegmentText::multi`), so the sums are exact.
fn merge_facet_into(acc: &mut FxHashMap<usize, FacetResult>, facet: FacetResult) {
    use std::collections::hash_map::Entry;
    match acc.entry(facet.position) {
        Entry::Vacant(slot) => {
            slot.insert(facet);
        }
        Entry::Occupied(mut slot) => {
            let existing = slot.get_mut();
            existing.sampled |= facet.sampled;
            let desc = matches!(existing.order, facet::FacetOrder::Field { desc: true });
            let mut at: FxHashMap<String, usize> = existing.counts.iter().enumerate().map(|(i, c)| (c.value.clone(), i)).collect();
            for incoming in facet.counts {
                match at.get(&incoming.value) {
                    Some(&i) => {
                        let c = &mut existing.counts[i];
                        match (&mut c.groups, incoming.groups) {
                            (Some(mine), Some(theirs)) => {
                                mine.extend(theirs);
                                mine.sort_unstable();
                                mine.dedup();
                                c.count = mine.len();
                            }
                            _ => c.count += incoming.count,
                        }
                        c.sort_value = match (c.sort_value, incoming.sort_value) {
                            (Some(a), Some(b)) => Some(if desc { a.max(b) } else { a.min(b) }),
                            (a, b) => a.or(b),
                        };
                        if c.parent.is_none() {
                            c.parent = incoming.parent;
                        }
                    }
                    None => {
                        at.insert(incoming.value.clone(), existing.counts.len());
                        existing.counts.push(incoming);
                    }
                }
            }
            existing.stats.merge(&facet.stats);
        }
    }
}

/// The merged facets as one segment gives them: per field its values in its order, at most
/// `max_facet_values`, the fields in facet_by order.
fn finish_facets(acc: FxHashMap<usize, FacetResult>, params: &SearchParams) -> Vec<FacetResult> {
    // a union's search: every value, for the union to cut
    let max = if params.union.is_some() { usize::MAX } else { params.max_facet_values.unwrap_or(10) };
    let mut facets: Vec<FacetResult> = acc.into_values().collect();
    for f in &mut facets {
        f.stats.total_values = f.counts.len();
        let by_value = |a: &facet::FacetCount, b: &facet::FacetCount| match (a.key, b.key) {
            (Some(x), Some(y)) => x.cmp(&y),
            _ => a.value.cmp(&b.value),
        };
        facet::rank(&mut f.counts, f.order, max, |c| c.count as u64, |c| c.sort_value, by_value);
    }
    facets.sort_by_key(|f| f.position);
    facets
}

/// One search of a multi_search's union (`SearchParams::union`): its hits keep their sort keys
/// for the union to order them by, and its facets count every value for the union to merge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnionPart {
    /// Its facets its own (the union adds them up).
    Each,
    /// Duplicates removed: the response carries its results per segment (`segment_docs`), and the
    /// union counts the facets over every search's (`facets_over`), each document once.
    Distinct,
}

/// One field of a search's sort as a union compares searches: its name, the type of what it sorts by and whether descending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnionSortField {
    pub name: String,
    pub kind: &'static str,
    pub desc: bool,
}

/// A search's sort as a union compares it: its sort_by with what it adds (the text match, a vector query's distance; without sort_by a default sorting
/// field), and with fewer than two fields the union's search index and insertion order after.
pub fn union_sort(params: &SearchParams, schema: &Schema, join_context: Option<&JoinContext<'_>>) -> Result<Vec<UnionSortField>, String> {
    fn kind(t: Option<&FieldType>) -> &'static str {
        match t {
            Some(FieldType::Int32) => "int32_field",
            Some(FieldType::Int64) => "int64_field",
            Some(FieldType::Float64) => "float_field",
            Some(FieldType::Bool) => "bool_field",
            Some(FieldType::GeoPoint | FieldType::GeoPointArray) => "geopoint_field",
            _ => "string_field",
        }
    }
    let field = |name: &str, kind: &'static str, desc: bool| UnionSortField { name: name.into(), kind, desc };
    let specs = sort_specs(params, schema)?;
    let mut fields: Vec<UnionSortField> = specs
        .iter()
        .map(|s| {
            let desc = matches!(s.order, SortOrder::Desc);
            let of = |fid: FieldId| schema.fields.get(fid as usize);
            match &s.field {
                SortField::TextMatch => field("_text_match", "text_match", desc),
                SortField::VectorDistance => field("_vector_distance", "vector_search", desc),
                SortField::SeqId => field("_seq_id", "insertion_order", desc),
                SortField::GroupFound => field("_group_found", "string_field", desc),
                SortField::Field(fid) => field(of(*fid).map_or("", |f| f.name.as_str()), kind(of(*fid).map(|f| &f.field_type)), desc),
                SortField::Decay(d) => field(of(d.field_id).map_or("", |f| f.name.as_str()), kind(of(d.field_id).map(|f| &f.field_type)), desc),
                SortField::GeoDistance(fid, ..) => field(of(*fid).map_or("", |f| f.name.as_str()), "geopoint_field", desc),
                SortField::JoinSort { target_collection, target_field } => {
                    let target = join_context.and_then(|j| j.target(target_collection));
                    let t = target.as_ref().and_then(|t| t.schema.name_to_id.get(target_field).and_then(|&f| t.schema.fields.get(f as usize))).map(|f| f.field_type.clone());
                    field(target_field, kind(t.as_ref()), desc)
                }
            }
        })
        .collect();
    if specs.is_empty() {
        if !params.q.is_empty() && params.q != "*" {
            fields.push(field("_text_match", "text_match", true));
        }
        if params.vector_query.is_some() {
            fields.push(field("_vector_distance", "vector_search", false));
        }
    }
    if fields.len() < 2 {
        fields.push(field("_union_search_index", "union_query_order", false));
        fields.push(field("_seq_id", "insertion_order", true));
    }
    Ok(fields)
}

/// Whether a search sorts by its collection's default sorting field (no sort_by).
pub fn sorts_by_default(params: &SearchParams, schema: &Schema) -> bool {
    params.sort_by.as_deref().is_none_or(|s| s.trim().is_empty()) && schema.default_sorting_field.as_deref().is_some_and(|f| !f.is_empty())
}

/// The order of hits of searches sorting alike (a union's, `union_sort` the same): by the sort
/// keys their searches attached (`SearchParams::union`), as the first search sorts them.
pub struct HitOrder {
    specs: Vec<SortSpec>,
}

impl HitOrder {
    pub fn new(params: &SearchParams, schema: &Schema) -> Result<Self, String> {
        Ok(Self { specs: sort_specs(params, schema)? })
    }

    /// `schema` the first search's.
    pub fn cmp(&self, a: &Hit, b: &Hit, schema: &Schema) -> Ordering {
        compare_sort_keys(a, b, &self.specs, schema)
    }
}

/// The segments of a search target with their deleted documents.
fn segments_of<'t>(target: &SearchTarget<'t>) -> Vec<(&'t Segment, Option<&'t Bitmap>)> {
    match *target {
        SearchTarget::Segment(seg) => vec![(seg, None)],
        SearchTarget::Bundle(b) => b.segments.iter().enumerate().map(|(i, s)| (&**s, b.deleted_for(i).filter(|d| !d.is_empty()))).collect(),
    }
}

/// A union's facets over the results of several of a collection's searches at once (`docs` per
/// segment of `target`; `tops` the union's first hits among them, for top_k facets), by
/// `params`' facet_by and facet_query: every value counted, for the union to merge and cut.
pub fn facets_over<'a>(
    target: impl Into<SearchTarget<'a>>,
    schema: &Schema,
    params: &SearchParams,
    docs: &[Bitmap],
    tops: Option<&[Bitmap]>,
    join_context: Option<&JoinContext<'_>>,
) -> Result<Vec<FacetResult>, String> {
    let target = target.into();
    let indexes: Vec<&Index> = segments_of(&target).into_iter().map(|(s, _)| s).collect();
    let Some(f) = params.facet_by.as_deref() else { return Ok(Vec::new()) };
    let facet_by = facet::parse_facet_by(f, schema)?;
    let fq = facet_query_of(params, &facet_by, schema)?;
    let mut every = params.clone();
    every.union = Some(UnionPart::Each);
    let specs: Vec<&facet::FacetSpec> = facet_by.local.iter().collect();
    let groups: Vec<Option<facet::DocGroups>> = indexes.iter().map(|_| None).collect();
    let mut out = late_facets(&specs, &indexes, docs, tops, fq.as_ref(), &groups, &every, schema);
    if !facet_by.joins.is_empty() {
        let how = facet::Counting { max: usize::MAX, multi: true, groups: None, query: None };
        let mut acc: FxHashMap<usize, FacetResult> = FxHashMap::default();
        for (k, index) in indexes.iter().enumerate() {
            for f in compute_join_facets(&facet_by.joins, &docs[k], index, join_context, how)? {
                merge_facet_into(&mut acc, f);
            }
        }
        out.extend(finish_facets(acc, &every));
        out.sort_by_key(|f| f.position);
    }
    Ok(out)
}

/// A union's facets: those of its searches (or collections) added up by field, each field's at
/// most `max_facet_values` in its order.
pub fn merge_union_facets(parts: impl IntoIterator<Item = Vec<FacetResult>>, max_facet_values: Option<usize>) -> Vec<FacetResult> {
    let mut names: Vec<String> = Vec::new();
    let mut acc: FxHashMap<usize, FacetResult> = FxHashMap::default();
    for facets in parts {
        for mut f in facets {
            f.position = match names.iter().position(|n| *n == f.field_name) {
                Some(p) => p,
                None => {
                    names.push(f.field_name.clone());
                    names.len() - 1
                }
            };
            merge_facet_into(&mut acc, f);
        }
    }
    finish_facets(acc, &SearchParams { max_facet_values, ..Default::default() })
}

/// Parse `pinned_hits` syntax: `ext_id:position,ext_id:position,...`
/// (positions are 1-indexed; the id may itself contain `:`).
fn parse_pinned_entries(pinned: &str) -> Vec<(&str, usize)> {
    pinned
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            let colon = s.rfind(':')?;
            let ext_id = &s[..colon];
            let pos = s[colon + 1..].parse::<usize>().ok()?;
            Some((ext_id, pos))
        })
        .collect()
}

fn classify_query_type(is_text: bool, is_vector: bool) -> &'static str {
    match (is_text, is_vector) {
        (true, true) => "hybrid",
        (true, false) => "text",
        (false, true) => "vector",
        (false, false) => "filter_only",
    }
}

/// Sort all candidates and return (DocId, score) pairs in sorted order.
fn sort_all_candidates(
    candidates: &Bitmap,
    sort_specs: &[SortSpec],
    scores: &Scores,
    index: &Index,
    is_text_query: bool,
    join_context: Option<&JoinContext<'_>>,
    vector_distances: &FxHashMap<DocId, f32>,
) -> Vec<(DocId, u64)> {
    if !sort_specs.is_empty() {
        // `_seq_id` alone: the documents' order (a segment's documents are in it)
        if let [SortSpec { field: SortField::SeqId, order, .. }] = sort_specs {
            let mut doc_ids: Vec<DocId> = candidates.iter().collect();
            if matches!(order, SortOrder::Desc) {
                doc_ids.reverse();
            }
            return doc_ids.into_iter().map(|id| (id, score_of(scores, id))).collect();
        }
        // Fast path: single-field sort — use position lookup O(C log C) instead of O(M) linear scan
        if sort_specs.len() == 1 {
            if let SortField::Field(fid) = &sort_specs[0].field {
                if let Some(col) = SortColumn::of(index, *fid) {
                    let descending = matches!(sort_specs[0].order, SortOrder::Desc);
                    let missing_first = col.missing_first(descending, sort_specs[0].missing);
                    let mut pairs: Vec<(u64, DocId)> = candidates
                        .iter()
                        .map(|doc_id| (col.key(doc_id, descending, missing_first), doc_id))
                        .collect();
                    pairs.sort_unstable_by_key(|p| p.0);
                    return pairs
                        .into_iter()
                        .map(|(_, id)| (id, score_of(scores, id)))
                        .collect();
                }
            }
        }
        let precomputed = precompute_sort_scores(candidates, sort_specs, index, join_context);
        let mut doc_ids: Vec<DocId> = candidates.iter().collect();
        sort_doc_ids(&mut doc_ids, sort_specs, scores, index, Some(&precomputed), &vector_distances);
        doc_ids
            .into_iter()
            .map(|id| (id, score_of(scores, id)))
            .collect()
    } else if is_text_query {
        // Typesense's default order: the score, then the latest inserted document.
        let mut pairs: Vec<(DocId, u64)> = candidates.iter().map(|id| (id, score_of(scores, id))).collect();
        pairs.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
        pairs
    } else {
        // No sort, no text query (q=*): return highest doc IDs first (most recent),
        // matching Typesense default ordering.
        let mut doc_ids: Vec<DocId> = candidates.iter().collect();
        doc_ids.reverse();
        doc_ids
            .into_iter()
            .map(|id| (id, score_of(scores, id)))
            .collect()
    }
}

/// Sort + paginate candidates, returning only the page slice.
fn paginate_candidates(
    candidates: &Bitmap,
    sort_specs: &[SortSpec],
    scores: &Scores,
    index: &Index,
    is_text_query: bool,
    start: usize,
    per_page: usize,
    join_context: Option<&JoinContext<'_>>,
    vector_distances: &FxHashMap<DocId, f32>,
) -> Vec<(DocId, u64)> {
    if per_page == 0 {
        return Vec::new();
    }
    if !sort_specs.is_empty() {
        // `_seq_id` alone: one page of the documents' order
        if let [SortSpec { field: SortField::SeqId, order, .. }] = sort_specs {
            let page: Vec<DocId> = if matches!(order, SortOrder::Desc) {
                candidates.top_desc(start.saturating_add(per_page)).into_iter().skip(start).collect()
            } else {
                candidates.iter().skip(start).take(per_page).collect()
            };
            return page.into_iter().map(|id| (id, score_of(scores, id))).collect();
        }
        // Fast path: single-field sort — scan presorted index with early termination
        if sort_specs.len() == 1 {
            if let SortField::Field(fid) = &sort_specs[0].field {
                if let Some(col) = SortColumn::of(index, *fid) {
                    let descending = matches!(sort_specs[0].order, SortOrder::Desc);
                    let missing_first = col.missing_first(descending, sort_specs[0].missing);
                    return scan_presorted_page(
                        col, index.total_docs() as usize, candidates, scores,
                        descending, missing_first, start, per_page,
                    );
                }
            }

            // Fast path: single decay/geo sort → TopKCollector O(N) + O(K log K)
            // Computes each score once instead of O(N log N) recomputations in sort
            let descending = matches!(sort_specs[0].order, SortOrder::Desc);
            match &sort_specs[0].field {
                SortField::Decay(decay) => {
                    let k = start + per_page;
                    let mut collector = TopKCollector::new(k);
                    for doc_id in candidates.iter() {
                        let val = index.number_i64(decay.field_id, doc_id);
                        let score = val.map(|v| decay.sort_score(v)).unwrap_or(0.0) as f32;
                        collector.push(doc_id, if descending { score } else { -score });
                    }
                    let sorted = collector.into_sorted();
                    let start_idx = start;
                    return sorted
                        .iter()
                        .skip(start_idx)
                        .take(per_page)
                        .map(|s| (s.doc_id, score_of(scores, s.doc_id)))
                        .collect();
                }
                SortField::GeoDistance(fid, center_lat, center_lng) => {
                    if let Some(geo_idx) = index.geo_indexes.get(fid) {
                        // Typesense's whole-metre distances, ties by highest doc id.
                        let k = start + per_page;
                        let centre = crate::index::geo::sort_centre(*center_lat, *center_lng);
                        let n_candidates = candidates.cardinality();
                        let page_of = |ranked: Vec<(u32, DocId)>| -> Vec<(DocId, u64)> {
                            ranked
                                .into_iter()
                                .skip(start)
                                .take(per_page)
                                .map(|(_, d)| (d, score_of(scores, d)))
                                .collect()
                        };
                        if !descending && n_candidates * 16 >= geo_idx.n_points() {
                            // Nearest first: best-first walk of the R-tree from the centre.
                            return page_of(geo_idx.nearest(centre, candidates, k));
                        }
                        // Few candidates, or farthest first: rank every candidate.
                        let mut ranked: Vec<(u32, DocId)> =
                            candidates.iter().map(|d| (geo_idx.distance_m(d, centre), d)).collect();
                        let order = |a: &(u32, DocId), b: &(u32, DocId)| {
                            let by = if descending { b.0.cmp(&a.0) } else { a.0.cmp(&b.0) };
                            by.then(b.1.cmp(&a.1))
                        };
                        if ranked.len() > k && k > 0 {
                            ranked.select_nth_unstable_by(k - 1, order);
                            ranked.truncate(k);
                        }
                        ranked.sort_unstable_by(order);
                        return page_of(ranked);
                    }
                }
                _ => {}
            }
        }
        // A field, then the text match breaking its ties (a text query's sort_by): one key per
        // candidate.
        if let [first, SortSpec { field: SortField::TextMatch, order: SortOrder::Desc, .. }] = sort_specs {
            if let Some(col) = match first.field {
                SortField::Field(fid) => SortColumn::of(index, fid),
                _ => None,
            } {
                let descending = matches!(first.order, SortOrder::Desc);
                let missing_first = col.missing_first(descending, first.missing);
                let needed = start + per_page;
                let mut keyed: Vec<((u64, u64, u32), DocId)> = candidates
                    .iter()
                    .map(|d| ((col.order(d, descending, missing_first), u64::MAX - score_of(scores, d), u32::MAX - d), d))
                    .collect();
                if keyed.len() > needed {
                    keyed.select_nth_unstable_by_key(needed - 1, |p| p.0);
                    keyed.truncate(needed);
                }
                keyed.sort_unstable_by_key(|p| p.0);
                return keyed.into_iter().skip(start).take(per_page).map(|(_, d)| (d, score_of(scores, d))).collect();
            }
        }
        // Multi-field or no fast path: pre-compute expensive scores, then partial sort
        let precomputed = precompute_sort_scores(candidates, sort_specs, index, join_context);
        let needed = start + per_page;
        let mut doc_ids: Vec<DocId> = candidates.iter().collect();
        if doc_ids.len() > needed {
            doc_ids.select_nth_unstable_by(needed - 1, |&a, &b| {
                compare_by_specs(a, b, sort_specs, scores, index, Some(&precomputed), &vector_distances)
            });
            doc_ids.truncate(needed);
            doc_ids.sort_unstable_by(|&a, &b| {
                compare_by_specs(a, b, sort_specs, scores, index, Some(&precomputed), &vector_distances)
            });
        } else {
            sort_doc_ids(&mut doc_ids, sort_specs, scores, index, Some(&precomputed), &vector_distances);
        }
        let start_idx = start;
        doc_ids
            .iter()
            .skip(start_idx)
            .take(per_page)
            .map(|&id| (id, score_of(scores, id)))
            .collect()
    } else if is_text_query {
        // Typesense's default order: the score, then the latest inserted document. The keys are
        // unique, so the first `needed` after a selection are exact.
        let needed = start + per_page;
        let mut pairs: Vec<(DocId, u64)> = candidates.iter().map(|id| (id, score_of(scores, id))).collect();
        let order = |a: &(DocId, u64), b: &(DocId, u64)| b.1.cmp(&a.1).then(b.0.cmp(&a.0));
        if pairs.len() > needed && needed > 0 {
            pairs.select_nth_unstable_by(needed - 1, order);
            pairs.truncate(needed);
        }
        pairs.sort_unstable_by(order);
        pairs.into_iter().skip(start).take(per_page).collect()
    } else {
        // No sort, no text query (q=*): return highest doc IDs first (most recent),
        // matching Typesense default ordering.
        candidates.top_desc(start.saturating_add(per_page)).into_iter().skip(start).map(|d| (d, 0)).collect()
    }
}

/// One page of candidates in a field's sort order, returning one page of results.
/// Dual-strategy: when candidates are sparse (< total/4), iterate the bitmap and look up
/// sort keys for O(N_candidates) partial sort. Otherwise walk the field's values in order
/// with early termination.
#[allow(clippy::too_many_arguments)]
fn scan_presorted_page(
    col: SortColumn<'_>,
    n_total: usize,
    candidates: &Bitmap,
    scores: &Scores,
    descending: bool,
    missing_first: bool,
    skip: usize,
    take: usize,
) -> Vec<(DocId, u64)> {
    let needed = skip + take;
    let n_candidates = candidates.cardinality() as usize;

    // Sparse filter: look up the candidates' sort keys and sort partially (an allocation of
    // n_candidates pairs). Dense filter or small candidate set: walk the values in order, which
    // scans about n_total / n_candidates * needed documents, until the walk holds more documents
    // than half the candidates (a filter on the sort field leaves the first values without any).
    let expected_dense_scan = (n_total as u64 * needed as u64) / n_candidates.max(1) as u64;
    let sparse = n_candidates < n_total * 3 / 4 && n_candidates > needed;
    if !(sparse && expected_dense_scan > n_candidates as u64) {
        let budget = if sparse { n_candidates as u64 / 2 } else { u64::MAX };
        if let Some(page) = col.presorted_within(candidates, descending, missing_first, needed, budget) {
            return page.into_iter().skip(skip).map(|id| (id, score_of(scores, id))).collect();
        }
    }
    let mut pairs: Vec<(u64, DocId)> = candidates.iter().map(|doc_id| (col.key(doc_id, descending, missing_first), doc_id)).collect();
    // The keys are unique (they end in the doc id), so the first `needed` are exact.
    pairs.select_nth_unstable_by_key(needed - 1, |p| p.0);
    pairs.truncate(needed);
    pairs.sort_unstable_by_key(|p| p.0);
    pairs.into_iter().skip(skip).take(take).map(|(_, doc_id)| (doc_id, score_of(scores, doc_id))).collect()
}

/// Pre-computed expensive sort scores (Decay/GeoDistance) for O(1) lookup during sort.
/// Each entry in `caches` corresponds to a sort spec: `Some(vec)` for Decay/GeoDistance
/// (indexed by DocId), `None` for cheap fields (TextMatch, Field, GroupFound).
struct PrecomputedScores {
    caches: Vec<Option<Vec<f64>>>,
}

/// Pre-compute expensive sort scores (Decay, GeoDistance, JoinSort) for all candidates.
/// Returns a PrecomputedScores with dense Vec<f64> indexed by DocId for each expensive spec.
fn precompute_sort_scores(
    candidates: &Bitmap,
    sort_specs: &[SortSpec],
    index: &Index,
    join_context: Option<&JoinContext<'_>>,
) -> PrecomputedScores {
    let needs_precompute = sort_specs.iter().any(|s| {
        matches!(s.field, SortField::Decay(_) | SortField::GeoDistance(..) | SortField::JoinSort { .. })
    });
    if !needs_precompute {
        return PrecomputedScores {
            caches: sort_specs.iter().map(|_| None).collect(),
        };
    }

    let max_doc = candidates.maximum().map(|m| m as usize + 1).unwrap_or(0);
    let caches = sort_specs
        .iter()
        .map(|spec| match &spec.field {
            SortField::Decay(decay) => {
                let mut scores = vec![0.0f64; max_doc];
                for doc_id in candidates.iter() {
                    let val = index.number_i64(decay.field_id, doc_id);
                    scores[doc_id as usize] = val.map(|v| decay.sort_score(v)).unwrap_or(0.0);
                }
                Some(scores)
            }
            SortField::GeoDistance(fid, center_lat, center_lng) => {
                if let Some(geo_idx) = index.geo_indexes.get(fid) {
                    let centre = crate::index::geo::sort_centre(*center_lat, *center_lng);
                    let mut dists = vec![0.0f64; max_doc];
                    for doc_id in candidates.iter() {
                        dists[doc_id as usize] = f64::from(geo_idx.distance_m(doc_id, centre));
                    }
                    Some(dists)
                } else {
                    None
                }
            }
            SortField::JoinSort { target_collection, target_field } => {
                precompute_join_sort_values(
                    candidates, max_doc, index, join_context,
                    target_collection, target_field,
                )
            }
            _ => None,
        })
        .collect();
    PrecomputedScores { caches }
}

/// Precompute sort values from a referenced (joined) collection: per candidate the value of the
/// first live document it references, in whichever of the collection's segments holds it.
fn precompute_join_sort_values(
    candidates: &Bitmap,
    max_doc: usize,
    index: &Index,
    join_context: Option<&JoinContext<'_>>,
    target_collection: &str,
    target_field: &str,
) -> Option<Vec<f64>> {
    let target = join_context?.target(target_collection)?;
    let fid = *target.schema.name_to_id.get(target_field)?;
    let refs = JoinContext::references(index, &target, target_collection)?;
    let mut referenced = Referenced::new(refs, &target.view);
    let mut values = vec![f64::NAN; max_doc];
    for doc_id in candidates.iter() {
        if let Some(v) = referenced.first(doc_id).and_then(|(s, d)| target.view.segment(s).0.number_f64(fid, d)) {
            values[doc_id as usize] = v;
        }
    }
    Some(values)
}

/// Compare two doc IDs according to sort specs.
/// When `precomputed` is Some, Decay/GeoDistance lookups are O(1) array reads.
#[inline]
fn compare_by_specs(
    a: DocId,
    b: DocId,
    sort_specs: &[SortSpec],
    scores: &Scores,
    index: &Index,
    precomputed: Option<&PrecomputedScores>,
    vector_distances: &FxHashMap<DocId, f32>,
) -> Ordering {
    for (i, spec) in sort_specs.iter().enumerate() {
        if let SortField::JoinSort { .. } = &spec.field {
            // without a referenced value last in both directions, as a missing number
            if let Some(cache) = precomputed.and_then(|p| p.caches[i].as_ref()) {
                let (va, vb) = (cache[a as usize], cache[b as usize]);
                let cmp = match (va.is_nan(), vb.is_nan()) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (false, false) if matches!(spec.order, SortOrder::Desc) => vb.total_cmp(&va),
                    (false, false) => va.total_cmp(&vb),
                };
                if cmp != Ordering::Equal {
                    return cmp;
                }
            }
            continue;
        }
        if let SortField::Field(fid) = &spec.field {
            // The place in the sort order already has the direction and missing values applied.
            if let Some(col) = SortColumn::of(index, *fid) {
                let descending = matches!(spec.order, SortOrder::Desc);
                let missing_first = col.missing_first(descending, spec.missing);
                let cmp = col.order(a, descending, missing_first).cmp(&col.order(b, descending, missing_first));
                if cmp != Ordering::Equal {
                    return cmp;
                }
            }
            continue;
        }
        let cmp = match &spec.field {
            SortField::TextMatch => score_of(scores, a).cmp(&score_of(scores, b)),
            SortField::Field(_) => unreachable!(),
            SortField::Decay(decay) => {
                if let Some(cache) = precomputed.and_then(|p| p.caches[i].as_ref()) {
                    let sa = cache[a as usize];
                    let sb = cache[b as usize];
                    sa.partial_cmp(&sb).unwrap_or(Ordering::Equal)
                } else {
                    let score = |d: DocId| index.number_i64(decay.field_id, d).map_or(0.0, |v| decay.score(v));
                    score(a).partial_cmp(&score(b)).unwrap_or(Ordering::Equal)
                }
            }
            SortField::GeoDistance(fid, center_lat, center_lng) => {
                if let Some(cache) = precomputed.and_then(|p| p.caches[i].as_ref()) {
                    let da = cache[a as usize];
                    let db = cache[b as usize];
                    da.partial_cmp(&db).unwrap_or(Ordering::Equal)
                } else if let Some(geo_idx) = index.geo_indexes.get(fid) {
                    let centre = crate::index::geo::sort_centre(*center_lat, *center_lng);
                    geo_idx.distance_m(a, centre).cmp(&geo_idx.distance_m(b, centre))
                } else {
                    Ordering::Equal
                }
            }
            SortField::VectorDistance => {
                let da = vector_distances.get(&a).copied().unwrap_or(f32::MAX);
                let db = vector_distances.get(&b).copied().unwrap_or(f32::MAX);
                da.partial_cmp(&db).unwrap_or(Ordering::Equal)
            }
            // a segment's documents are in `_seq_id` order
            SortField::SeqId => a.cmp(&b),
            SortField::GroupFound => {
                // GroupFound sort is handled at the group level, not doc level
                Ordering::Equal
            }
            SortField::JoinSort { .. } => unreachable!(),
        };
        let cmp = match spec.order {
            SortOrder::Asc => cmp,
            SortOrder::Desc => cmp.reverse(),
        };
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    // Tiebreak: highest doc ID first (most recently inserted), matching Typesense.
    b.cmp(&a)
}

/// Sort doc_ids in place according to sort specs.
fn sort_doc_ids(
    doc_ids: &mut [DocId],
    sort_specs: &[SortSpec],
    scores: &Scores,
    index: &Index,
    precomputed: Option<&PrecomputedScores>,
    vector_distances: &FxHashMap<DocId, f32>,
) {
    doc_ids.sort_by(|&a, &b| compare_by_specs(a, b, sort_specs, scores, index, precomputed, vector_distances));
}

fn compute_geo_distances(
    doc_id: DocId,
    sort_specs: &[SortSpec],
    index: &Index,
    schema: &Schema,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut distances = serde_json::Map::new();
    for spec in sort_specs {
        if let SortField::GeoDistance(fid, center_lat, center_lng) = &spec.field {
            let centre = crate::index::geo::sort_centre(*center_lat, *center_lng);
            let metres = index
                .geo_indexes
                .get(fid)
                .map_or(crate::index::geo::MISSING_M, |g| g.distance_m(doc_id, centre));
            let field_name = &schema.fields[*fid as usize].name;
            distances.insert(field_name.clone(), serde_json::Value::Number(serde_json::Number::from(metres)));
        }
    }
    if distances.is_empty() { None } else { Some(distances) }
}

/// The facets of `specs` over `candidates`.
fn compute_facets(specs: &[&facet::FacetSpec], candidates: &Bitmap, index: &Index, how: facet::Counting<'_>) -> Vec<FacetResult> {
    let compute = |spec: &&facet::FacetSpec| facet::compute_facet(spec, facet_source(spec, index), candidates, how);
    if specs.len() <= 2 {
        specs.iter().filter_map(compute).collect()
    } else {
        use rayon::prelude::*;
        specs.par_iter().filter_map(compute).collect()
    }
}

/// Where a segment holds a facet's values.
fn facet_source<'i>(spec: &facet::FacetSpec, index: &'i Index) -> facet::FacetSource<'i> {
    match (index.values.get(&spec.fid), index.bools.get(&spec.fid)) {
        (Some(idx), _) => facet::FacetSource::Values { idx, sort_by: spec.sort_field.and_then(|f| index.values.get(&f)) },
        (None, Some(b)) => facet::FacetSource::Bools(b),
        (None, None) => facet::FacetSource::Absent,
    }
}

/// facet_return_parent: per value of a nested facet field the object holding it, from the first
/// document of `docs` with the value: the field's parent object, and in an array of objects the
/// element whose field has the value. The value is compared as the field's type, any element of
/// an array.
fn attach_parents(facets: &mut [FacetResult], params: &SearchParams, schema: &Schema, index: &Index, docs: &Bitmap) {
    let Some(ref ret_parent) = params.facet_return_parent else { return };
    let parent_fields: Vec<&str> = ret_parent.split(',').map(str::trim).collect();
    for fr in facets {
        // range labels have no document (Typesense gives ranges no parent either)
        if fr.ranges || !parent_fields.contains(&fr.field_name.as_str()) {
            continue;
        }
        let path: Vec<&str> = fr.field_name.split('.').collect();
        if path.len() < 2 {
            continue;
        }
        let Some(&fid) = schema.name_to_id.get(fr.field_name.as_str()) else {
            continue;
        };
        let Some(sf) = index.values.get(&fid) else {
            continue;
        };
        for count in &mut fr.counts {
            let with_value = if sf.is_str() { sf.eq_str(&count.value) } else { count.key.and_then(|k| sf.eq_key(k)) };
            let Some(doc) = with_value.map(|bm| bm & docs).and_then(|bm| bm.minimum()).and_then(|id| index.get_doc(id, schema)) else {
                continue;
            };
            let holds = |v: &serde_json::Value| -> bool {
                if sf.is_str() {
                    v.as_str() == Some(count.value.as_str())
                } else if sf.is_float() {
                    v.as_f64().map(crate::index::values::f64_key) == count.key
                } else {
                    v.as_i64() == count.key
                }
            };
            count.parent = parent_holding(&document_to_json(&doc, schema), &path, &holds).cloned();
        }
    }
}

/// The object in `node` (an object or an array of them) whose field at `path` holds a value `holds`
/// (any element of an array value); in an array the first such element.
fn parent_holding<'a>(node: &'a serde_json::Value, path: &[&str], holds: &dyn Fn(&serde_json::Value) -> bool) -> Option<&'a serde_json::Value> {
    match node {
        serde_json::Value::Array(items) => items.iter().find_map(|item| parent_holding(item, path, holds)),
        serde_json::Value::Object(fields) => {
            let child = fields.get(path[0])?;
            if path.len() > 1 {
                return parent_holding(child, &path[1..], holds);
            }
            let found = match child {
                serde_json::Value::Array(values) => values.iter().any(holds),
                value => holds(value),
            };
            found.then_some(node)
        }
        _ => None,
    }
}

/// Facets on fields of referenced collections (`$brands(country)`): per value of the referenced
/// documents (in any of the collection's segments) the candidates referencing one with it, with
/// the `facet_filter` for the value.
fn compute_join_facets(
    joins: &[facet::JoinFacet],
    candidates: &Bitmap,
    index: &Index,
    join_context: Option<&JoinContext<'_>>,
    how: facet::Counting<'_>,
) -> Result<Vec<FacetResult>, String> {
    let Some(jctx) = join_context else { return Ok(Vec::new()) };
    let mut results = Vec::new();
    for join in joins {
        let error = |e: String| format!("Error parsing reference facet: {e}");
        let target = jctx.target(&join.collection).ok_or_else(|| error(format!("Referenced collection `{}` not found.", join.collection)))?;
        let parsed = facet::parse_facet_by(&join.entries, &target.schema).map_err(error)?;
        let Some(refs) = JoinContext::references(index, &target, &join.collection) else {
            continue;
        };
        let segs = target.view.segments();
        // each candidate's referenced documents (`docs[at[i]..at[i + 1]]`), and grouped its group
        let mut referenced = Referenced::new(refs, &target.view);
        let mut at: Vec<usize> = Vec::with_capacity(candidates.cardinality() as usize + 1);
        let mut docs: Vec<(usize, DocId)> = Vec::new();
        at.push(0);
        for d in candidates.iter() {
            referenced.each(d, &mut docs);
            at.push(docs.len());
        }
        let of = |i: usize| &docs[at[i]..at[i + 1]];
        let groups: Option<Vec<u64>> = how.groups.map(|g| candidates.iter().map(|d| g.of(d)).collect());
        for (k, spec) in parsed.local.into_iter().enumerate() {
            let field = spec.name.clone();
            let spec = facet::FacetSpec { name: format!("${}({field})", join.collection), position: join.position + k, ..spec };
            let mut result = facet::empty(&spec);
            let idxs: Vec<Option<&crate::index::values::ValueIndex>> = segs.iter().map(|(s, _)| s.values.get(&spec.fid)).collect();
            let bools: Vec<Option<&crate::index::values::BoolIndex>> = segs.iter().map(|(s, _)| s.bools.get(&spec.fid)).collect();
            let mut string = false;
            if let Some(&first) = idxs.iter().flatten().next() {
                string = first.is_str();
                let mut values = JoinValues::new(&idxs, first);
                let range_ords: Vec<Vec<std::ops::Range<u64>>> = idxs
                    .iter()
                    .map(|idx| {
                        use std::ops::Bound;
                        let Some(idx) = idx else { return Vec::new() };
                        spec.ranges.iter().map(|r| idx.key_ords(r.lo.map_or(Bound::Unbounded, Bound::Included), r.hi.map_or(Bound::Unbounded, Bound::Excluded))).collect()
                    })
                    .collect();
                let by: Vec<Option<&crate::index::values::ValueIndex>> = segs.iter().map(|(s, _)| spec.sort_field.and_then(|f| s.values.get(&f))).collect();
                let desc = matches!(spec.order, facet::FacetOrder::Field { desc: true });
                let mut counts: FxHashMap<u64, u32> = FxHashMap::default();
                let mut value_groups: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
                let mut per_range = vec![0u64; spec.ranges.len()];
                let mut range_groups: Vec<Vec<u64>> = vec![Vec::new(); spec.ranges.len()];
                let mut in_range = vec![false; spec.ranges.len()];
                let mut sort_values: FxHashMap<u64, i64> = FxHashMap::default();
                let (mut buf, mut ords): (Vec<u64>, Vec<u64>) = (Vec::new(), Vec::new());
                for i in 0..at.len() - 1 {
                    buf.clear();
                    in_range.fill(false);
                    for &(s, td) in of(i) {
                        let Some(idx) = idxs[s] else { continue };
                        ords.clear();
                        match idx.doc_ords() {
                            Some(o) => ords.extend(o.elems(td).flatten()),
                            None => ords.extend((0..idx.len()).filter(|&o| idx.docs(o).contains(td))),
                        }
                        let key = by[s].and_then(|b| b.doc_key(td));
                        for &o in &ords {
                            for (r, range) in range_ords[s].iter().enumerate() {
                                in_range[r] |= range.contains(&o);
                            }
                            let id = values.id(s, o);
                            buf.push(id);
                            if let Some(key) = key {
                                sort_values.entry(id).and_modify(|v| *v = if desc { (*v).max(key) } else { (*v).min(key) }).or_insert(key);
                            }
                        }
                    }
                    buf.sort_unstable();
                    buf.dedup();
                    for &id in &buf {
                        *counts.entry(id).or_default() += 1;
                        if let Some(g) = &groups {
                            value_groups.entry(id).or_default().push(g[i]);
                        }
                    }
                    for (r, _) in in_range.iter().enumerate().filter(|(_, x)| **x) {
                        per_range[r] += 1;
                        if let Some(g) = &groups {
                            range_groups[r].push(g[i]);
                        }
                    }
                }
                // several segments: the values numbered in value order, as one segment's ordinals
                let merged = values.finish();
                if let Some((_, place)) = &merged {
                    let p = |id: u64| place[id as usize];
                    counts = counts.into_iter().map(|(id, n)| (p(id), n)).collect();
                    value_groups = value_groups.into_iter().map(|(id, g)| (p(id), g)).collect();
                    sort_values = sort_values.into_iter().map(|(id, v)| (p(id), v)).collect();
                }
                let mut counts: Vec<(u64, u32)> = counts.into_iter().collect();
                counts.sort_unstable();
                let distinct = |mut g: Vec<u64>| {
                    g.sort_unstable();
                    g.dedup();
                    g
                };
                let value_groups = groups.as_ref().map(|_| counts.iter().map(|(o, _)| distinct(value_groups.remove(o).unwrap_or_default())).collect());
                let ranges: Vec<(u64, Option<Vec<u64>>)> = match &groups {
                    Some(_) => range_groups
                        .into_iter()
                        .map(|g| {
                            let g = distinct(g);
                            (g.len() as u64, Some(g))
                        })
                        .collect(),
                    None => per_range.into_iter().map(|n| (n, None)).collect(),
                };
                let sort_values = matches!(spec.order, facet::FacetOrder::Field { .. }).then_some(sort_values);
                let names: &dyn facet::OrdValues = match &merged {
                    Some((m, _)) => m,
                    None => first,
                };
                facet::finish(&mut result, &spec, names, counts, value_groups, sort_values, ranges, facet::Counting { query: None, ..how });
            } else if bools.iter().any(Option::is_some) {
                // the candidates referencing a document with the value, or their groups
                let with = |v: bool| -> (usize, Option<Vec<u64>>) {
                    let holders = (0..at.len() - 1).filter(|&i| of(i).iter().any(|&(s, td)| bools[s].and_then(|b| b.value(td)) == Some(v)));
                    match &groups {
                        Some(g) => {
                            let mut ids: Vec<u64> = holders.map(|i| g[i]).collect();
                            ids.sort_unstable();
                            ids.dedup();
                            (ids.len(), Some(ids))
                        }
                        None => (holders.count(), None),
                    }
                };
                let mut items: Vec<(bool, usize, Option<Vec<u64>>)> = [false, true].into_iter().map(|v| (v, with(v))).map(|(v, (n, g))| (v, n, g)).filter(|x| x.1 > 0).collect();
                result.stats.total_values = items.len();
                facet::rank(&mut items, facet::FacetOrder::Count, how.max, |x| x.1 as u64, |_| None, |a, b| a.0.cmp(&b.0));
                result.counts = items
                    .into_iter()
                    .map(|(v, n, groups)| facet::FacetCount {
                        parent: None,
                        value: v.to_string(),
                        highlighted: v.to_string(),
                        count: n,
                        facet_filter: None,
                        key: Some(i64::from(v)),
                        sort_value: None,
                        groups: groups.filter(|_| how.multi),
                    })
                    .collect();
            } else {
                continue;
            }
            for c in &mut result.counts {
                let filter = match c.key.and_then(|i| spec.ranges.get(i as usize)) {
                    Some(r) => {
                        let float = matches!(target.schema.fields[spec.fid as usize].field_type, FieldType::Float64);
                        let text = |k: i64| facet::number_text(float, k);
                        match (r.lo, r.hi) {
                            (Some(lo), Some(hi)) => format!("{field}: >={} && {field}: <{}", text(lo), text(hi)),
                            (Some(lo), None) => format!("{field}: >={}", text(lo)),
                            (None, Some(hi)) => format!("{field}: <{}", text(hi)),
                            (None, None) => format!("{field}: *"),
                        }
                    }
                    None if string => format!("{field}: `{}`", c.value),
                    None => format!("{field}: {}", c.value),
                };
                c.facet_filter = Some(format!("${}({filter})", join.collection));
            }
            results.push(result);
        }
    }
    Ok(results)
}

/// A join facet's values over the referenced collection's segments, one id per value: with one
/// segment its ordinals; else each segment's ordinals mapped, when first seen, to their value's id
/// (`finish` then numbers the values in value order).
struct JoinValues<'t> {
    idxs: &'t [Option<&'t crate::index::values::ValueIndex>],
    single: bool,
    str: bool,
    float: bool,
    seen: Vec<FxHashMap<u64, u64>>,
    strs: FxHashMap<String, u64>,
    keys: FxHashMap<i64, u64>,
}

impl<'t> JoinValues<'t> {
    fn new(idxs: &'t [Option<&'t crate::index::values::ValueIndex>], first: &crate::index::values::ValueIndex) -> Self {
        Self {
            idxs,
            single: idxs.len() == 1,
            str: first.is_str(),
            float: first.is_float(),
            seen: vec![FxHashMap::default(); idxs.len()],
            strs: FxHashMap::default(),
            keys: FxHashMap::default(),
        }
    }

    fn id(&mut self, s: usize, ord: u64) -> u64 {
        if self.single {
            return ord;
        }
        if let Some(&id) = self.seen[s].get(&ord) {
            return id;
        }
        let idx = self.idxs[s].expect("a segment holding the value");
        let id = if self.str {
            let next = self.strs.len() as u64;
            *self.strs.entry(idx.str_at(ord).unwrap_or_default()).or_insert(next)
        } else {
            let next = self.keys.len() as u64;
            *self.keys.entry(idx.key_at(ord).unwrap_or_default()).or_insert(next)
        };
        self.seen[s].insert(ord, id);
        id
    }

    /// With several segments the values seen in value order, and each id's place among them.
    fn finish(self) -> Option<(facet::MergedValues, Vec<u64>)> {
        fn ordered<K: Ord>(map: FxHashMap<K, u64>) -> (Vec<K>, Vec<u64>) {
            let mut v: Vec<(K, u64)> = map.into_iter().collect();
            v.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            let mut place = vec![0u64; v.len()];
            for (p, (_, id)) in v.iter().enumerate() {
                place[*id as usize] = p as u64;
            }
            (v.into_iter().map(|(k, _)| k).collect(), place)
        }
        if self.single {
            return None;
        }
        Some(if self.str {
            let (strs, place) = ordered(self.strs);
            (facet::MergedValues::Strs(strs), place)
        } else {
            let (keys, place) = ordered(self.keys);
            (facet::MergedValues::Keys { keys, float: self.float }, place)
        })
    }
}

/// Convert a Bitmap into a pre-allocated u64 word buffer for GPU filter bitmap upload.
/// Reuses the buffer to avoid allocation per query.
#[cfg(feature = "cuda")]
fn roaring_to_u64_bitmap_into(bm: &Bitmap, num_docs: u32, buf: &mut Vec<u64>) {
    let num_words = ((num_docs as usize) + 63) / 64;
    buf.resize(num_words, 0);
    buf[..num_words].fill(0);
    for doc_id in bm.iter() {
        let word_idx = doc_id as usize / 64;
        let bit_idx = doc_id as usize % 64;
        if word_idx < num_words {
            buf[word_idx] |= 1u64 << bit_idx;
        }
    }
}


