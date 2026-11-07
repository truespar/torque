//! A collection in several segments answers as one segment holding the same documents: the same
//! documents found, in the same order, with the same text match, facets, groups and highlights.

use std::sync::Arc;
use torque::bitmap::Bitmap;
use torque::index::IndexBundle;
use torque::ingest;
use torque::schema::{Document, FieldDef, FieldType, FieldValue, Language, Schema};
use torque::search::{self, SearchParams, SearchResponse};

fn schema() -> Schema {
    let field = |name: &str, field_type: FieldType, facet: bool, sort: bool| FieldDef {
        name: name.into(),
        field_type,
        facet,
        index: true,
        sort,
        ..Default::default()
    };
    let mut s = Schema {
        name: "companies".into(),
        fields: vec![
            field("name", FieldType::String, false, false),
            field("city", FieldType::String, true, true),
            field("employees", FieldType::Int32, true, true),
        ],
        ..Default::default()
    };
    s.assign_field_ids();
    s
}

const WORDS: [&str; 16] = [
    "bygg", "byggare", "byggservice", "bygger", "fastighet", "fastigheter", "fastighets", "konsult", "konsulting", "måleri",
    "maleri", "stockholm", "stokholm", "invest", "investering", "holding",
];
const CITIES: [&str; 5] = ["Stockholm", "Göteborg", "Malmö", "Uppsala", "Lund"];

fn docs() -> Vec<Document> {
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    (0..240)
        .map(|i| {
            let n = 1 + next() % 3;
            let name: Vec<&str> = (0..n).map(|_| WORDS[(next() % WORDS.len() as u64) as usize]).collect();
            Document {
                id: 0,
                external_id: i.to_string(),
                language: Language::English,
                fields: vec![
                    FieldValue::Text(format!("{} AB", name.join(" "))),
                    FieldValue::Text(CITIES[(next() % 5) as usize].into()),
                    FieldValue::Int32((next() % 50) as i32),
                ]
                .into(),
                record: None,
            }
        })
        .collect()
}

/// The documents as one segment, and split into segments of `sizes`.
fn bundles(sizes: &[usize]) -> (IndexBundle, IndexBundle) {
    bundles_of(schema(), sizes)
}

fn bundles_of(schema: Schema, sizes: &[usize]) -> (IndexBundle, IndexBundle) {
    bundles_with(schema, docs(), sizes)
}

fn bundles_with(schema: Schema, all: Vec<Document>, sizes: &[usize]) -> (IndexBundle, IndexBundle) {
    let schema = Arc::new(schema);
    let one = IndexBundle::single(ingest::build_index(&schema, all.clone()), Arc::clone(&schema));
    let mut segments = Vec::new();
    let mut at = 0usize;
    for &n in sizes {
        let mut seg = ingest::build_index(&schema, all[at..at + n].to_vec());
        seg.base_doc_id = at as u32;
        segments.push(Arc::new(seg));
        at += n;
    }
    assert_eq!(at, all.len());
    let deleted = vec![Arc::new(Bitmap::new()); segments.len()];
    let total_docs = IndexBundle::compute_live_total_docs(&segments, &deleted);
    let many = IndexBundle { schema, segments, deleted_per_segment: deleted, total_docs, lsn: 0, next_seq: 0, memory: 0 };
    (one, many)
}

fn params(q: &str) -> SearchParams {
    SearchParams { q: q.into(), query_by: vec!["name".into()], per_page: 20, page: 1, ..Default::default() }
}

/// What a response says, without timings.
fn summary(r: SearchResponse) -> serde_json::Value {
    let mut v = serde_json::to_value(&r).unwrap();
    v.as_object_mut().unwrap().remove("search_time_ms");
    v
}

fn check(sizes: &[usize], p: &SearchParams) {
    check_with(schema(), sizes, p);
}

fn check_with(schema: Schema, sizes: &[usize], p: &SearchParams) {
    let (one, many) = bundles_of(schema.clone(), sizes);
    check_bundles(&schema, &one, &many, sizes, p);
}

fn check_bundles(schema: &Schema, one: &IndexBundle, many: &IndexBundle, sizes: &[usize], p: &SearchParams) {
    let a = summary(search::execute(one, schema, p).unwrap());
    let b = summary(search::execute(many, schema, p).unwrap());
    if a != b {
        panic!("sizes {sizes:?} q={} filter={:?} sort={:?} group={:?} page={}: {}", p.q, p.filter_by, p.sort_by, p.group_by, p.page, first_diff(&a, &b, ""));
    }
}

/// The first place two values differ.
fn first_diff(a: &serde_json::Value, b: &serde_json::Value, path: &str) -> String {
    use serde_json::Value;
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                if x.get(k) != y.get(k) {
                    return match (x.get(k), y.get(k)) {
                        (Some(u), Some(v)) => first_diff(u, v, &format!("{path}.{k}")),
                        (u, v) => format!("{path}.{k}: {u:?} vs {v:?}"),
                    };
                }
            }
            String::new()
        }
        (Value::Array(x), Value::Array(y)) => {
            for i in 0..x.len().max(y.len()) {
                if x.get(i) != y.get(i) {
                    return match (x.get(i), y.get(i)) {
                        (Some(u), Some(v)) => first_diff(u, v, &format!("{path}[{i}]")),
                        (u, v) => format!("{path}[{i}]: {} vs {}", u.map_or("-".into(), |u| u.to_string()), v.map_or("-".into(), |v| v.to_string())),
                    };
                }
            }
            String::new()
        }
        _ => format!("{path}: {a} vs {b}"),
    }
}

#[test]
fn text_queries_match_one_segment() {
    for sizes in [&[120, 120][..], &[100, 7, 133], &[1, 238, 1]] {
        for q in ["bygg", "byg", "b", "fastighet", "fastihet", "fastigheter stockholm", "konslt", "stockholm", "maleri", "invest holding", "xyz", "*"] {
            check(sizes, &params(q));
            let mut p = params(q);
            p.max_candidates = Some(2);
            check(sizes, &p);
        }
    }
}

#[test]
fn filters_sorts_facets_and_pages_match_one_segment() {
    for sizes in [&[120, 120][..], &[100, 7, 133]] {
        for q in ["bygg", "fastighet", "*"] {
            let mut p = params(q);
            p.filter_by = Some("employees:>10".into());
            p.facet_by = Some("city,employees".into());
            p.max_facet_values = Some(3);
            check(sizes, &p);
            p.sort_by = Some("employees:desc".into());
            check(sizes, &p);
            p.page = 2;
            p.per_page = 7;
            check(sizes, &p);
            p.offset = Some(5);
            check(sizes, &p);
        }
    }
}

#[test]
fn facets_of_every_kind_match_one_segment() {
    for sizes in [&[120, 120][..], &[100, 7, 133]] {
        for q in ["bygg", "*"] {
            for facet_by in [
                "employees",
                "employees(few:[0, 10], some:[10, 30], many:[30, ])",
                "city(sort_by:_alpha:desc)",
                "city(sort_by:employees:desc)",
                "city(sort_by:employees:asc), employees(sort_by:employees:desc)",
                "c*, employees",
            ] {
                let mut p = params(q);
                p.facet_by = Some(facet_by.into());
                p.max_facet_values = Some(4);
                check(sizes, &p);
                p.filter_by = Some("employees:<40".into());
                p.max_facet_values = Some(2);
                check(sizes, &p);
            }
            let mut p = params(q);
            p.facet_by = Some("city, employees".into());
            p.facet_query = Some("city:st".into());
            check(sizes, &p);
            p.facet_query = Some("employees:1".into());
            check(sizes, &p);
        }
    }
}

#[test]
fn sorts_and_pinned_hits_match_one_segment() {
    for sizes in [&[120, 120][..], &[100, 7, 133]] {
        for q in ["bygg", "fastighet stockholm", "*"] {
            for sort in ["city:asc", "city:desc", "city:DESC,employees:asc", "employees:asc,city:asc", "_text_match:asc", "employees(missing_values: first):desc"] {
                let mut p = params(q);
                p.sort_by = Some(sort.into());
                check(sizes, &p);
                p.page = 3;
                p.per_page = 9;
                check(sizes, &p);
            }
            let mut p = params(q);
            p.pinned_hits = Some("17:1,200:3,5:40,239:2".into());
            check(sizes, &p);
            p.filter_by = Some("employees:>25".into());
            p.filter_curated_hits = true;
            check(sizes, &p);
            p.group_by = Some("city".into());
            p.group_limit = Some(2);
            check(sizes, &p);
        }
    }
}

/// A field sort with the text match breaking its ties ranks only the documents that can reach the
/// page, after every round: each document's text_match is the one ranking it as it is found gives
/// (a text match sort's), through every typo round and dropped word and words matched in a second
/// field only by a later round's candidates; and each page holds what the same place of a page
/// taking every document found holds, through ties on the field and pinned hits that would
/// otherwise be among the first.
#[test]
fn a_field_sort_ranks_only_what_reaches_the_page() {
    let schema = schema();
    let (one, many) = bundles(&[100, 7, 133]);
    let hits = |bundle: &IndexBundle, p: &SearchParams| match search::execute(bundle, &schema, p).unwrap() {
        SearchResponse::Ungrouped(r) => serde_json::to_value(&r.hits).unwrap().as_array().unwrap().clone(),
        _ => panic!("expected an ungrouped result"),
    };
    let id = |h: &serde_json::Value| h["document"]["id"].as_str().unwrap().to_string();
    for bundle in [&one, &many] {
        for query_by in [&["name"][..], &["name", "city"]] {
            for q in ["bygg", "stokholm", "stokholm konsult", "fastighet stockholm xyzzy", "maleri invest holding"] {
                let mut base = params(q);
                base.query_by = query_by.iter().map(|f| f.to_string()).collect();
                base.typo_tokens_threshold = Some(500);
                base.drop_tokens_threshold = Some(500);
                base.per_page = 250;
                let ranked = hits(bundle, &SearchParams { sort_by: Some("_text_match:desc".into()), ..base.clone() });
                let text_match: std::collections::HashMap<String, serde_json::Value> = ranked.iter().map(|h| (id(h), h["text_match"].clone())).collect();
                for sort in ["employees:asc", "employees:desc", "city:asc", "city:desc,_text_match:desc", "employees(missing_values: first):desc"] {
                    let p = SearchParams { sort_by: Some(sort.into()), ..base.clone() };
                    let every = hits(bundle, &p);
                    assert_eq!(every.len(), ranked.len());
                    for h in &every {
                        assert_eq!(h["text_match"], text_match[&id(h)], "{} segments q={q} query_by={query_by:?} sort={sort}: text_match of {}", bundle.segments.len(), id(h));
                    }
                    // the first organic hits pinned further on, one more on the first page
                    let pins = (every.len() > 5).then(|| format!("{}:30,{}:31,{}:2", id(&every[0]), id(&every[1]), id(&every[5])));
                    for pinned in [None, pins] {
                        let p = SearchParams { pinned_hits: pinned.clone(), ..p.clone() };
                        let every = hits(bundle, &p);
                        for (page, per_page) in [(1, 1), (1, 5), (2, 5), (3, 9), (7, 4)] {
                            let part = SearchParams { page, per_page, ..p.clone() };
                            let want: Vec<_> = every.iter().skip((page - 1) * per_page).take(per_page).cloned().collect();
                            assert_eq!(hits(bundle, &part), want, "{} segments q={q} query_by={query_by:?} sort={sort} pinned={pinned:?} page {page} of {per_page}", bundle.segments.len());
                        }
                    }
                }
            }
        }
    }
}

/// Without sort_by: the text match, then the default_sorting_field descending. A token's
/// candidates are the terms whose documents hold the field's largest values, then the most
/// frequent.
#[test]
fn default_sorting_field_sorts_and_matches_one_segment() {
    let mut s = schema();
    s.default_sorting_field = Some("employees".into());
    for sizes in [&[120, 120][..], &[100, 7, 133], &[1, 238, 1]] {
        for q in ["bygg", "byg", "b", "f", "fastihet", "konslt", "*"] {
            for max in [None, Some(1), Some(2)] {
                let mut p = params(q);
                p.max_candidates = max;
                check_with(s.clone(), sizes, &p);
            }
        }
    }
    // the candidate of "f" with one: the f-term whose documents hold the most employees
    let docs = docs();
    let mut best: Option<(i32, usize, String)> = None;
    let mut terms: std::collections::BTreeMap<String, (i32, usize)> = Default::default();
    for d in &docs {
        let (Some(FieldValue::Text(name)), Some(&FieldValue::Int32(n))) = (d.fields.get(0), d.fields.get(2)) else { panic!() };
        let mut seen = std::collections::HashSet::new();
        for t in name.to_lowercase().split(' ').filter(|t| t.starts_with('f')) {
            if seen.insert(t.to_string()) {
                let e = terms.entry(t.to_string()).or_insert((i32::MIN, 0));
                e.0 = e.0.max(n);
                e.1 += 1;
            }
        }
    }
    for (t, (v, df)) in terms {
        if best.as_ref().is_none_or(|b| (v, df) > (b.0, b.1)) {
            best = Some((v, df, t));
        }
    }
    let (_, df, _) = best.unwrap();
    let (one, many) = bundles_of(s.clone(), &[100, 7, 133]);
    let mut p = params("f");
    p.max_candidates = Some(1);
    p.drop_tokens_threshold = Some(0);
    for b in [&one, &many] {
        let SearchResponse::Ungrouped(r) = search::execute(b, &s, &p).unwrap() else { panic!() };
        assert_eq!(r.found, df);
    }
    let (one, _) = bundles_of(s.clone(), &[240]);
    let SearchResponse::Ungrouped(r) = search::execute(&one, &s, &params("*")).unwrap() else { panic!() };
    let employees: Vec<i64> = r.hits.iter().map(|h| h.document["employees"].as_i64().unwrap()).collect();
    assert!(employees.windows(2).all(|w| w[0] >= w[1]), "{employees:?}");
}

/// pinned_hits at their positions in the one ordering of the results (Typesense's curated hits): a
/// page holds the pins placed on it and the organic hits around them, pins past the end follow the
/// last hit, and pinned documents count in `found` and the facets.
#[test]
fn pinned_hits_sit_at_their_positions_across_pages() {
    let s = schema();
    let pins = "17:1,200:3,5:12,239:2,5:4,9999:5,31:500";
    for sizes in [&[120, 120][..], &[100, 7, 133]] {
        let (one, many) = bundles(sizes);
        for q in ["bygg", "*"] {
            for grouped in [false, true] {
                for page in 1..=4 {
                    let mut p = params(q);
                    p.pinned_hits = Some(pins.into());
                    p.per_page = 5;
                    p.page = page;
                    p.facet_by = Some("city, employees".into());
                    if grouped {
                        p.group_by = Some("city".into());
                        p.group_limit = Some(2);
                        p.per_page = 2;
                    }
                    check_bundles(&s, &one, &many, sizes, &p);
                }
            }
        }
    }
    // q=*: the latest documents first, the pins where they ask (5 once, 9999 is none), 31 last
    let (one, _) = bundles(&[240]);
    let page = |n: usize| {
        let mut p = params("*");
        p.pinned_hits = Some(pins.into());
        p.per_page = 5;
        p.page = n;
        let SearchResponse::Ungrouped(r) = search::execute(&one, &s, &p).unwrap() else { panic!() };
        let ids: Vec<String> = r.hits.iter().map(|h| h.document["id"].as_str().unwrap().to_string()).collect();
        (r.found, ids, r.hits.iter().map(|h| h.curated == Some(true)).collect::<Vec<_>>())
    };
    assert_eq!(page(1), (240, ["17", "239", "200", "238", "237"].map(String::from).to_vec(), vec![true, true, true, false, false]));
    assert_eq!(page(3).1, ["231", "5", "230", "229", "228"]);
    assert_eq!(page(48).1, ["3", "2", "1", "0", "31"]);
    assert!(page(49).1.is_empty());
}

/// top_k facets count the search's first hits: Typesense's 250, or the hits up to the page's end;
/// grouped, the first groups' documents (their groups counted).
#[test]
fn top_k_facets_count_the_first_hits() {
    use torque::search::SearchResponse::{Grouped, Ungrouped};
    let s = schema();
    // 600 documents, the latest 300 in "New": q=* ranks the latest first
    let all: Vec<Document> = (0..600)
        .map(|i| Document {
            id: 0,
            external_id: i.to_string(),
            language: Language::English,
            fields: vec![FieldValue::Text(format!("bygg {i}")), FieldValue::Text(if i >= 300 { "New" } else { "Old" }.into()), FieldValue::Int32((i % 50) as i32)].into(),
            record: None,
        })
        .collect();
    let counts = |r: &SearchResponse, at: usize| -> Vec<(String, usize)> {
        let facets = match r {
            Ungrouped(u) => &u.facet_counts,
            Grouped(g) => &g.facet_counts,
        };
        facets[at].counts.iter().map(|c| (c.value.clone(), c.count)).collect()
    };
    for sizes in [&[600][..], &[300, 300], &[250, 99, 251]] {
        let (one, many) = bundles_with(s.clone(), all.clone(), sizes);
        let mut p = params("*");
        p.facet_by = Some("city(top_k:true), city, employees(top_k:true)".into());
        p.max_facet_values = Some(50);
        check_bundles(&s, &one, &many, sizes, &p);
        let r = search::execute(&many, &s, &p).unwrap();
        assert_eq!(counts(&r, 0), [("New".to_string(), 250)]);
        assert_eq!(counts(&r, 1), [("New".to_string(), 300), ("Old".to_string(), 300)]);
        assert!(counts(&r, 2).iter().all(|c| c.1 == 5), "{:?}", counts(&r, 2));
        // the page ends past the 250th hit
        p.page = 30;
        p.per_page = 10;
        check_bundles(&s, &one, &many, sizes, &p);
        assert_eq!(counts(&search::execute(&many, &s, &p).unwrap(), 0), [("New".to_string(), 300)]);
        // the first groups' documents, 2 a group: the latest of every employee count
        p.page = 1;
        p.group_by = Some("employees".into());
        p.group_limit = Some(2);
        check_bundles(&s, &one, &many, sizes, &p);
        assert_eq!(counts(&search::execute(&many, &s, &p).unwrap(), 0), [("New".to_string(), 50)]);
        // pinned hits are among the first
        p.group_by = None;
        p.pinned_hits = Some("3:1".into());
        check_bundles(&s, &one, &many, sizes, &p);
        assert_eq!(counts(&search::execute(&many, &s, &p).unwrap(), 0), [("New".to_string(), 249), ("Old".to_string(), 1)]);
    }
}

/// A grouped search's facets count groups (Typesense): a value's count is the `found` a filter on
/// it gives.
#[test]
fn grouped_facets_count_groups() {
    use torque::search::SearchResponse::Grouped;
    let s = schema();
    for sizes in [&[240][..], &[120, 120], &[100, 7, 133]] {
        let (one, many) = bundles(sizes);
        for q in ["bygg", "*"] {
            for missing in [None, Some(false)] {
                let mut p = params(q);
                p.group_by = Some("city".into());
                p.group_missing_values = missing;
                p.facet_by = Some("city, employees, employees(few:[0, 10], many:[10, ])".into());
                p.max_facet_values = Some(50);
                check_bundles(&s, &one, &many, sizes, &p);
                p.facet_query = Some("city:stok".into());
                check_bundles(&s, &one, &many, sizes, &p);
            }
            let mut p = params(q);
            p.group_by = Some("employees".into());
            p.facet_by = Some("city".into());
            check_bundles(&s, &one, &many, sizes, &p);
            let Grouped(r) = search::execute(&many, &s, &p).unwrap() else { panic!() };
            for c in &r.facet_counts[0].counts {
                let mut f = p.clone();
                f.filter_by = Some(format!("city:={}", c.value));
                let Grouped(g) = search::execute(&many, &s, &f).unwrap() else { panic!() };
                assert_eq!(g.found, c.count, "{q} {}", c.value);
            }
        }
    }
}

/// facet_query keeps the values holding its words, the last a prefix, with typos only where
/// nothing matches without.
#[test]
fn facet_query_matches_words_with_typos() {
    use torque::search::SearchResponse::Ungrouped;
    let s = schema();
    for sizes in [&[240][..], &[120, 120], &[100, 7, 133]] {
        let (one, many) = bundles(sizes);
        for fq in ["city:stokholm", "city:sto", "city:göt", "city:xyz", "city: lund", "employees:1"] {
            for facet_by in ["city, employees", "city(top_k:true), employees(top_k:true)"] {
                let mut p = params("*");
                p.facet_by = Some(facet_by.into());
                p.facet_query = Some(fq.into());
                p.max_facet_values = Some(20);
                check_bundles(&s, &one, &many, sizes, &p);
            }
        }
        let found = |fq: &str| -> Vec<(String, String)> {
            let mut p = params("*");
            p.facet_by = Some("city".into());
            p.facet_query = Some(fq.into());
            let Ungrouped(r) = search::execute(&many, &s, &p).unwrap() else { panic!() };
            r.facet_counts[0].counts.iter().map(|c| (c.value.clone(), c.highlighted.clone())).collect()
        };
        let one_value = |v: &str, h: &str| vec![(v.to_string(), h.to_string())];
        assert_eq!(found("city:stokholm"), one_value("Stockholm", "<mark>Stockholm</mark>"));
        assert_eq!(found("city:sto"), one_value("Stockholm", "<mark>Sto</mark>ckholm"));
        assert_eq!(found("city:göt"), one_value("Göteborg", "<mark>Göt</mark>eborg"));
        assert!(found("city:xyz").is_empty());
    }
    // typos widen the match only where nothing matches without, in any segment
    let all: Vec<Document> = (0..90)
        .map(|i| Document {
            id: 0,
            external_id: i.to_string(),
            language: Language::English,
            fields: vec![FieldValue::Text("bygg".into()), FieldValue::Text(["Lund", "Linköping", "Lunda"][i % 3].into()), FieldValue::Int32(1)].into(),
            record: None,
        })
        .collect();
    for sizes in [&[90][..], &[1, 89], &[45, 45]] {
        let (one, many) = bundles_with(s.clone(), all.clone(), sizes);
        let values = |fq: &str| -> Vec<String> {
            let mut p = params("*");
            p.facet_by = Some("city".into());
            p.facet_query = Some(fq.into());
            check_bundles(&s, &one, &many, sizes, &p);
            let Ungrouped(r) = search::execute(&many, &s, &p).unwrap() else { panic!() };
            r.facet_counts[0].counts.iter().map(|c| c.value.clone()).collect()
        };
        assert_eq!(values("city:lun"), ["Lund", "Lunda"]);
        assert_eq!(values("city:linkop"), ["Linköping"]);
        assert_eq!(values("city:lumd"), ["Lund", "Lunda"]);
    }
}

#[test]
fn groups_match_one_segment() {
    for sizes in [&[120, 120][..], &[100, 7, 133], &[3, 230, 7]] {
        for q in ["bygg", "fastighet", "konslt", "*"] {
            let mut p = params(q);
            p.group_by = Some("city".into());
            p.group_limit = Some(50);
            check(sizes, &p);
            p.typo_tokens_threshold = Some(5);
            check(sizes, &p);
            // few hits per group and groups per page: a page group's best documents can be in a
            // segment where the group ranks below the page
            p.group_limit = Some(2);
            p.per_page = 2;
            check(sizes, &p);
            p.page = 2;
            check(sizes, &p);
            p.sort_by = Some("employees:asc".into());
            check(sizes, &p);
            p.sort_by = Some("_group_found:desc".into());
            check(sizes, &p);
            p.group_by = Some("employees".into());
            p.sort_by = None;
            check(sizes, &p);
        }
    }
}


/// A realtime collection's writes, each a segment of its own, answer as one segment holding the
/// documents as they are after the writes, in `_seq_id` order: the base's, a replaced one in its
/// place, then the new ones in the order written. So do they once merged in memory, and flushed.
#[test]
fn realtime_writes_match_one_segment() {
    use torque::collection::CollectionRegistry;
    use torque::search::SearchTarget;
    let mut s = schema();
    s.index_mode = torque::schema::IndexMode::Realtime;
    let registry = CollectionRegistry::new(None);
    let collection = registry.create(s.clone()).unwrap();
    let all = docs();
    let (base, later) = all.split_at(200);
    collection.swap_index(ingest::build_index(&collection.schema, base.to_vec()));
    // new documents, replaced ones (new text under an existing id) and deletions
    let mut written: Vec<Document> = Vec::new();
    for d in later {
        collection.apply_upsert(d.clone());
        written.push(d.clone());
    }
    for (i, d) in base.iter().enumerate().filter(|(i, _)| i % 9 == 0) {
        let mut d = d.clone();
        d.fields = vec![FieldValue::Text(format!("bygg fastighet {i} AB")), FieldValue::Text("Lund".into()), FieldValue::Int32(i as i32 % 50)].into();
        collection.apply_upsert(d.clone());
        written.push(d);
    }
    let deleted: Vec<String> = ["5", "17", "203", "77"].iter().map(|s| s.to_string()).collect();
    for id in &deleted {
        collection.apply_delete(id);
    }

    let expected = torque::collection::merge_writes(base.to_vec(), written, &deleted);
    let one = ingest::build_index(&collection.schema, expected);

    let mut cases = Vec::new();
    for q in ["bygg", "byg", "b", "fastighet", "fastihet", "konslt", "invest holding", "*"] {
        let mut p = params(q);
        // a replaced or deleted document counts in no term's frequency
        cases.push(p.clone());
        p.max_candidates = Some(2);
        cases.push(p.clone());
        p.max_candidates = None;
        p.filter_by = Some("employees:>10".into());
        p.facet_by = Some("city".into());
        p.sort_by = Some("employees:desc".into());
        cases.push(p.clone());
        p.sort_by = None;
        p.group_by = Some("city".into());
        p.group_limit = Some(2);
        p.per_page = 3;
        cases.push(p);
    }
    let compare = |stage: &str| {
        let bundle = collection.snapshot_bundle();
        for p in &cases {
            let a = summary(search::execute(&one, &collection.schema, p).unwrap());
            let b = summary(search::execute(SearchTarget::Bundle(&bundle), &collection.schema, p).unwrap());
            if a != b {
                panic!("{stage} ({} segments) q={} filter={:?} group={:?}: {}", bundle.segments.len(), p.q, p.filter_by, p.group_by, first_diff(&a, &b, ""));
            }
        }
        bundle.segments.len()
    };
    let written_apart = compare("a segment per write");
    assert!(collection.merge_memory() > 0);
    let merged = compare("merged in memory");
    assert!(merged < written_apart, "{merged} segments after merging {written_apart}");
    collection.compact();
    assert_eq!(collection.snapshot_bundle().memory, 0);
    compare("flushed");
}
