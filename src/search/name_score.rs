//! `name_score=true` (Torque extension): the search's first
//! `name_score_candidates` hits, scored by name (`crate::names::score`) and ordered by the score;
//! the page is taken from them. Without the parameter nothing here runs.

use super::{resolve_search_fields, SearchParams, SearchResponse, SearchTarget, SearchTimings};
use crate::names::score::{self, NameKind, Part, WEIGHTS};
use crate::schema::Schema;
use rustc_hash::FxHashMap;
use serde::Serialize;

/// The candidates scored when the search does not say.
pub const DEFAULT_CANDIDATES: usize = 100;
pub const MAX_CANDIDATES: usize = 1000;

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
/// A hit's `name_score`: the best of its names in the searched name-matching fields.
#[derive(Clone, Debug, Serialize)]
pub struct NameScore {
    pub field: String,
    pub name: String,
    pub pairs: Vec<NamePair>,
    pub score: f32,
    pub unmatched_name: Vec<String>,
    pub unmatched_query: Vec<String>,
}

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
#[derive(Clone, Debug, Serialize)]
pub struct NamePair {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<&'static str>,
    pub kind: &'static str,
    pub name: String,
    pub query: String,
    pub similarity: f32,
}

/// The search through `run` for its first candidates, scored and ordered by name.
pub(super) fn rerank(
    target: SearchTarget<'_>,
    schema: &Schema,
    params: &SearchParams,
    run: impl FnOnce(&SearchParams) -> Result<(SearchResponse, SearchTimings), String>,
) -> Result<(SearchResponse, SearchTimings), String> {
    if params.group_by.is_some() || params.union.is_some() {
        return Err("`name_score` does not work with `group_by` or a union.".into());
    }
    if params.q.trim() == "*" {
        return Err("`name_score` needs a name in `q`.".into());
    }
    let fields: Vec<(crate::schema::FieldId, &str)> = resolve_search_fields(params, schema)?
        .into_iter()
        .map(|f| f.fid)
        .filter(|&fid| schema.fields[fid as usize].name_matching)
        .map(|fid| (fid, schema.fields[fid as usize].name.as_str()))
        .collect();
    if fields.is_empty() {
        return Err("`name_score` needs a `query_by` field with `name_matching: true` in the schema.".into());
    }
    let kind = match params.name_score_kind.as_deref() {
        None => NameKind::Unknown,
        Some(k) => NameKind::parse(k).ok_or_else(|| format!("Invalid `name_score_kind` value `{k}`: person or organization."))?,
    };
    let window = params.name_score_candidates.unwrap_or(DEFAULT_CANDIDATES).clamp(1, MAX_CANDIDATES);
    let mut inner = params.clone();
    inner.name_score = None;
    inner.page = 1;
    inner.offset = None;
    inner.per_page = window;
    let (mut response, timings) = run(&inner)?;
    let SearchResponse::Ungrouped(r) = &mut response else {
        return Err("`name_score` does not work with `group_by` or a union.".into());
    };

    // A word's weight by how many documents hold it, in whichever searched field holds it most.
    let segments = super::segments_of(&target);
    let docs = target.total_docs();
    let mut rarities: FxHashMap<Vec<u8>, f32> = FxHashMap::default();
    let mut rarity = |word: &[u8]| -> f32 {
        if let Some(&r) = rarities.get(word) {
            return r;
        }
        let df = fields
            .iter()
            .map(|&(fid, _)| segments.iter().filter_map(|(s, _)| s.inverted.get(&fid)).map(|inv| inv.doc_freq(word)).sum::<u32>())
            .max()
            .unwrap_or(0);
        let r = score::rarity(df, docs, WEIGHTS.rarity_floor);
        rarities.insert(word.to_vec(), r);
        r
    };
    let mut parts_of = |text: &str| -> Vec<Part> {
        let mut out = Vec::new();
        for token in crate::text::tokenize(text, &schema.symbols_to_index, &schema.token_separators) {
            let (mut lowered, mut folded) = (String::new(), String::new());
            crate::text::normalize_into(token, &schema.symbols_to_index, &mut lowered, &mut folded);
            if !folded.is_empty() {
                let r = rarity(folded.as_bytes());
                out.push(Part::new(folded.into_bytes(), r, &WEIGHTS));
            }
        }
        out
    };
    let query = parts_of(&params.q);
    let word = |p: &Part| String::from_utf8_lossy(&p.word).into_owned();
    let words = |parts: &[Part], range: std::ops::Range<usize>| parts[range].iter().map(word).collect::<Vec<_>>().join(" ");

    let mut scored: Vec<(f32, super::Hit)> = Vec::with_capacity(r.hits.len());
    for mut hit in std::mem::take(&mut r.hits) {
        let mut best: Option<(score::Scored, &str, String, Vec<Part>)> = None;
        for &(_, field) in &fields {
            for value in values(&hit.document, field) {
                let name = parts_of(value);
                let s = score::score(&query, &name, kind, &WEIGHTS);
                if best.as_ref().is_none_or(|b| s.score > b.0.score) {
                    best = Some((s, field, value.to_string(), name));
                }
            }
        }
        let score = best.as_ref().map_or(0.0, |b| b.0.score);
        hit.name_score = best.map(|(s, field, value, name)| NameScore {
            score: (s.score * 1000.0).round() / 1000.0,
            field: field.to_string(),
            name: value,
            pairs: s
                .pairs
                .iter()
                .map(|p| NamePair {
                    query: words(&query, p.query.clone()),
                    name: words(&name, p.name.clone()),
                    kind: p.kind.name(),
                    encoder: p.kind.encoder(),
                    similarity: (p.similarity * 1000.0).round() / 1000.0,
                })
                .collect(),
            unmatched_query: s.unmatched_query.iter().map(|&i| word(&query[i])).collect(),
            unmatched_name: s.unmatched_name.iter().map(|&i| word(&name[i])).collect(),
        });
        scored.push((score, hit));
    }
    // by score; equal scores keep the search's order
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let start = params.start().min(scored.len());
    let end = (start + params.per_page).min(scored.len());
    r.hits = scored.drain(start..end).map(|(_, h)| h).collect();
    (r.page, r.offset) = params.page_or_offset();
    Ok((response, timings))
}

/// The strings of `field` (a dotted path for a nested field) in `doc`.
fn values<'d>(doc: &'d serde_json::Value, field: &str) -> Vec<&'d str> {
    let mut node = doc;
    for key in field.split('.') {
        match node.get(key) {
            Some(n) => node = n,
            None => return Vec::new(),
        }
    }
    match node {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str()).collect(),
        _ => Vec::new(),
    }
}
