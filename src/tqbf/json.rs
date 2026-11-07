//! Response JSON exactly as Typesense writes it: Typesense serializes every response with
//! nlohmann::json, so keys are sorted at every level (std::map) and doubles use Grisu2
//! (`dtoa::push_f64`). Strings are escaped the same way by serde_json and nlohmann.

use super::dtoa;
use serde::Serialize;
use std::io;

pub struct NlohmannFormatter;

impl serde_json::ser::Formatter for NlohmannFormatter {
    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        let mut buf = Vec::with_capacity(32);
        dtoa::push_f64(&mut buf, value);
        writer.write_all(&buf)
    }

    fn write_f32<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        self.write_f64(writer, f64::from(value))
    }
}

/// Serialize with sorted keys at every level and nlohmann number formatting.
pub fn to_vec<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Vec<u8>> {
    let sorted = serde_json::to_value(value)?;
    let mut out = Vec::with_capacity(4096);
    let mut ser = serde_json::Serializer::with_formatter(&mut out, NlohmannFormatter);
    sorted.serialize(&mut ser)?;
    Ok(out)
}

/// Serialize a value whose keys come out sorted at every level as it is (structs declared in key
/// order, maps sorted): `to_vec`'s bytes without the copy it sorts through.
pub fn to_vec_in_order<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(4096);
    let mut ser = serde_json::Serializer::with_formatter(&mut out, NlohmannFormatter);
    value.serialize(&mut ser)?;
    Ok(out)
}

/// `Json`-like axum response that writes Typesense's byte format.
pub struct TsJson<T>(pub T);

/// `TsJson` for a value whose keys serialize in order (`to_vec_in_order`): a search response.
pub struct TsJsonInOrder<T>(pub T);

impl<T: Serialize> axum::response::IntoResponse for TsJsonInOrder<T> {
    fn into_response(self) -> axum::response::Response {
        match to_vec_in_order(&self.0) {
            Ok(body) => ([(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")], body).into_response(),
            Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
}

impl<T: Serialize> axum::response::IntoResponse for TsJson<T> {
    fn into_response(self) -> axum::response::Response {
        match to_vec(&self.0) {
            Ok(body) => (
                [(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")],
                body,
            )
                .into_response(),
            Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sorted_keys_and_nlohmann_floats() {
        #[derive(serde::Serialize)]
        struct R {
            found: u32,
            facet_counts: Vec<u8>,
            hits: Vec<serde_json::Value>,
        }
        let doc: serde_json::Value = serde_json::from_str(r#"{"z":[11.95282,40.0],"a":"ö"}"#).unwrap();
        let r = R { found: 1, facet_counts: vec![], hits: vec![doc] };
        let s = String::from_utf8(super::to_vec(&r).unwrap()).unwrap();
        assert_eq!(s, r#"{"facet_counts":[],"found":1,"hits":[{"a":"ö","z":[11.952819999999999,40.0]}]}"#);
    }

    /// Every key of a search response, written as declared, as `to_vec` sorts them.
    #[test]
    fn a_search_response_is_written_in_key_order() {
        use crate::grouping::{GroupHit, GroupedSearchResult};
        use crate::index::facet::{FacetCount, FacetResult, FacetStats};
        use crate::search::highlight::Highlight;
        use crate::search::name_score::{NamePair, NameScore};
        use crate::search::scoring::{TextMatchInfo, WordMatch};
        use crate::search::{Hit, HybridSearchInfo, SearchResponse, SearchResult};
        use serde_json::json;
        let hit = || {
            let mut info = TextMatchInfo::new(u64::MAX >> 1, 2);
            info.matches = Some(vec![WordMatch { encoder: Some("dm"), kind: "typo", term: "t".into(), token: Some("x".into()), typos: 1 }]);
            let highlight = Highlight {
                field: "f".into(),
                indices: Some(vec![0]),
                matched_tokens: json!(["m"]),
                snippet: Some("s".into()),
                snippets: Some(vec!["s".into()]),
                value: Some("v".into()),
                values: Some(vec!["v".into()]),
            };
            let pair = NamePair { encoder: Some("dm"), kind: "exact", name: "a".into(), query: "b".into(), similarity: 0.25 };
            Hit {
                curated: Some(true),
                geo_distance_meters: Some(serde_json::Map::from_iter([("b".to_string(), json!(2)), ("a".to_string(), json!(1))])),
                highlight: json!({"z": {"snippet": "s"}, "a": [1]}),
                highlights: Some(vec![highlight]),
                hybrid_search_info: Some(HybridSearchInfo { rank_fusion_score: 0.5 }),
                name_score: Some(NameScore { field: "n".into(), name: "x".into(), pairs: vec![pair], score: 0.75, unmatched_name: vec!["u".into()], unmatched_query: vec!["q".into()] }),
                text_match: Some(7),
                text_match_info: Some(info),
                vector_distance: Some(0.125),
                ..Hit::new(json!({"z": 1, "id": "1", "a": {"y": 2.5, "b": [3]}}))
            }
        };
        let count = FacetCount {
            count: 3,
            facet_filter: Some("f:=v".into()),
            groups: None,
            highlighted: "<mark>v</mark>".into(),
            key: None,
            parent: Some(json!({"b": 1, "a": 2})),
            sort_value: None,
            value: "v".into(),
        };
        let stats = FacetStats { avg: Some(1.5), count: 2, max: Some(2.0), min: Some(1.0), sum: Some(3.0), total_values: 2 };
        let facet = || FacetResult { counts: vec![count.clone()], field_name: "f".into(), order: Default::default(), position: 0, ranges: false, sampled: true, stats: stats.clone() };
        let ungrouped = SearchResponse::Ungrouped(SearchResult {
            conversation: Some(json!({"q": 1, "a": 2})),
            docs: None,
            facet_counts: vec![facet()],
            found: 1,
            hits: vec![hit()],
            metadata: Some(json!({"k": 1})),
            offset: Some(0),
            out_of: 9,
            page: Some(1),
            pinned: Vec::new(),
            request_params: Some(json!({"q": "x", "collection_name": "c"})),
            search_cutoff: false,
            search_time_ms: 1,
            segment_docs: Vec::new(),
        });
        let grouped = SearchResponse::Grouped(GroupedSearchResult {
            conversation: Some(json!({"q": 1})),
            docs: None,
            facet_counts: vec![facet()],
            found: 1,
            found_docs: 1,
            group_counts: Vec::new(),
            grouped_hits: vec![GroupHit { found: 1, group_key: vec![json!("k")], hits: vec![hit()], id: 0 }],
            metadata: Some(json!({"k": 1})),
            offset: Some(0),
            out_of: 9,
            page: Some(1),
            pinned: Vec::new(),
            request_params: Some(json!({"q": "x"})),
            search_cutoff: true,
            search_time_ms: 1,
        });
        for r in [ungrouped, grouped] {
            assert_eq!(String::from_utf8(super::to_vec_in_order(&r).unwrap()).unwrap(), String::from_utf8(super::to_vec(&r).unwrap()).unwrap());
        }
    }
}
