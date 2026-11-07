//! Name matching. `prefix=false` matching the last word whole, the
//! `name_matching` field option (no hits for a query without words) and `explain_matches`. A
//! name-matching field's words found by passport spelling (cost 1) and sound (cost 2).

use torque::collection::CollectionRegistry;
use torque::ingest;
use torque::schema::{Document, FieldDef, FieldType, FieldValue, Language, Schema};
use torque::search;

fn setup(name_matching: bool, names: &[&[&str]]) -> CollectionRegistry {
    let mut schema = Schema {
        name: "people".into(),
        fields: vec![FieldDef { name: "names".into(), field_type: FieldType::StringArray, name_matching, ..Default::default() }],
        ..Default::default()
    };
    schema.assign_field_ids();
    let registry = CollectionRegistry::new(None);
    let collection = registry.create(schema).unwrap();
    let docs = names
        .iter()
        .enumerate()
        .map(|(i, n)| Document {
            id: 0,
            external_id: format!("{}", i + 1),
            language: Language::English,
            fields: vec![FieldValue::StringArray(n.iter().map(|s| Some(s.to_string())).collect())].into(),
            record: None,
        })
        .collect();
    let index = ingest::build_index(&collection.schema, docs);
    collection.swap_index(index);
    registry
}

fn params(q: &str) -> search::SearchParams {
    search::SearchParams { q: q.into(), query_by: vec!["names".into()], per_page: 10, page: 1, ..Default::default() }
}

fn run(registry: &CollectionRegistry, p: &search::SearchParams) -> search::SearchResult {
    let collection = registry.get("people").unwrap();
    let index = collection.snapshot();
    match search::execute(&index, &collection.schema, p).unwrap() {
        search::SearchResponse::Ungrouped(r) => r,
        _ => panic!("expected an ungrouped result"),
    }
}

fn ids(r: &search::SearchResult) -> Vec<String> {
    r.hits.iter().map(|h| h.document["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn prefix_false_matches_the_last_word_whole() {
    let registry = setup(false, &[&["YAS AIR"], &["YASIR SABAWI"], &["TARKHAN GAZIEV"]]);
    let mut p = params("yas air");
    p.prefix = Some("false".into());
    let r = run(&registry, &p);
    assert_eq!(ids(&r).first().map(String::as_str), Some("1"), "the exact name first, found {:?}", ids(&r));
    // and the last word is no longer prefix-searched: "yas" does not complete to "yasir"
    let mut p = params("yas");
    p.prefix = Some("false".into());
    assert_eq!(ids(&run(&registry, &p)), ["1"]);
}

#[test]
fn name_matching_folds_case_in_every_script() {
    let names: &[&[&str]] = &[&["Шипулин Антон"], &["Λας"]];
    let mut p = params("ШИПУЛИН");
    p.num_typos = Some("0".into());
    assert_eq!(ids(&run(&setup(true, names), &p)), ["1"]);
    p.q = "шипулин".into();
    assert_eq!(ids(&run(&setup(true, names), &p)), ["1"]);
    // Greek final sigma folds with sigma
    p.q = "ΛΑΣ".into();
    assert_eq!(ids(&run(&setup(true, names), &p)), ["2"]);

    // every field folds so, with the option or without
    p.q = "шипулин".into();
    assert_eq!(ids(&run(&setup(false, names), &p)), ["1"]);
}

#[test]
fn name_matching_splits_at_unicode_spaces() {
    let names: &[&[&str]] = &[&["YAS\u{00A0}AIR"], &["ANNA\u{2007}KARIN"]];
    let mut p = params("air");
    p.num_typos = Some("0".into());
    p.prefix = Some("false".into());
    assert_eq!(ids(&run(&setup(true, names), &p)), ["1"]);
    p.q = "karin".into();
    assert_eq!(ids(&run(&setup(true, names), &p)), ["2"]);
    // every field splits so, with the option or without
    p.q = "air".into();
    assert_eq!(ids(&run(&setup(false, names), &p)), ["1"]);
}

#[test]
fn a_query_without_words_finds_nothing_in_name_fields() {
    let names: &[&[&str]] = &[&["Anna Karlsson"], &["Per Nilsson"]];
    for q in ["---", "???", "\"\"", "-anna"] {
        assert_eq!(run(&setup(true, names), &params(q)).found, 0, "q={q}");
    }
    // `*` still lists everything
    assert_eq!(run(&setup(true, names), &params("*")).found, 2);
    // without the option a query without words lists the documents
    assert_eq!(run(&setup(false, names), &params("---")).found, 2);
}

#[test]
fn explain_matches_reports_how_each_word_matched() {
    let registry = setup(false, &[&["Usama bin Muhammad bin Awad BIN LADIN"], &["Hamza bin Laden"]]);
    let mut p = params("osama bin laden");
    p.explain_matches = Some(true);
    let r = run(&registry, &p);
    let hit = r.hits.iter().find(|h| h.document["id"] == "1").expect("the OFAC spelling is found");
    let matches = &hit.text_match_info.as_ref().unwrap().matches.as_ref().expect("explained");
    let kind = |token: &str| matches.iter().find(|m| m.token.as_deref() == Some(token)).map(|m| (m.term.clone(), m.kind, m.typos));
    assert_eq!(kind("bin"), Some(("bin".into(), "exact", 0)));
    assert_eq!(kind("osama"), Some(("usama".into(), "typo", 1)));
    assert_eq!(kind("laden"), Some(("ladin".into(), "typo", 1)));

    // not asked for: nothing added
    p.explain_matches = None;
    let r = run(&registry, &p);
    assert!(r.hits.iter().all(|h| h.text_match_info.as_ref().unwrap().matches.is_none()));
}

#[test]
fn name_matching_refuses_stemming() {
    let mut schema = Schema {
        name: "people".into(),
        fields: vec![FieldDef { name: "names".into(), field_type: FieldType::String, name_matching: true, stem: true, ..Default::default() }],
        ..Default::default()
    };
    schema.assign_field_ids();
    assert!(CollectionRegistry::new(None).create(schema).is_err());

    let mut schema = Schema {
        name: "people".into(),
        fields: vec![FieldDef { name: "age".into(), field_type: FieldType::Int32, name_matching: true, ..Default::default() }],
        ..Default::default()
    };
    schema.assign_field_ids();
    assert!(CollectionRegistry::new(None).create(schema).is_err());
}

fn explained(r: &search::SearchResult, id: &str, token: &str) -> Option<(String, &'static str, u32, Option<&'static str>)> {
    let hit = r.hits.iter().find(|h| h.document["id"] == id)?;
    let matches = hit.text_match_info.as_ref()?.matches.as_ref()?;
    matches.iter().find(|m| m.token.as_deref() == Some(token)).map(|m| (m.term.clone(), m.kind, m.typos, m.encoder))
}

/// `name_matching=always`: every spelling, whatever the exact one finds.
fn always(q: &str) -> search::SearchParams {
    search::SearchParams { name_matching: Some("always".into()), explain_matches: Some(true), ..params(q) }
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn passport_spellings_match_both_ways() {
    let names: &[&[&str]] = &[&["Ødegård"], &["Oedegaard"], &["Odd Nilsen"]];
    for q in ["oedegaard", "Ødegård", "odegard"] {
        assert_eq!(sorted(ids(&run(&setup(true, names), &always(q)))), ["1", "2"], "q={q}");
    }
    let r = run(&setup(true, names), &always("oedegaard"));
    assert_eq!(ids(&r)[0], "2", "the exact spelling first");
    assert_eq!(explained(&r, "2", "oedegaard"), Some(("oedegaard".into(), "exact", 0, None)));
    assert_eq!(explained(&r, "1", "oedegaard"), Some(("odegard".into(), "fold", 1, None)));
}

#[test]
fn names_match_by_sound() {
    // three edits apart: beyond typos, a match by sound
    let names: &[&[&str]] = &[&["Muhamad Ali"], &["Mohammed Ali"], &["Mats Ali"]];
    let r = run(&setup(true, names), &always("mohammed"));
    assert_eq!(ids(&r), ["2", "1"], "the exact spelling first");
    let (term, kind, typos, encoder) = explained(&r, "1", "mohammed").expect("explained");
    assert_eq!((term.as_str(), kind, typos), ("muhamad", "phonetic", 2));
    assert!(encoder.is_some());

    // fallback (the default): by sound only when the spelling and its typos find nothing
    assert_eq!(ids(&run(&setup(true, names), &params("mohammed"))), ["2"]);
    assert_eq!(ids(&run(&setup(true, &[&["Muhamad Ali"]]), &params("mohammed"))), ["1"]);
    // off, without the option, or with typos turned off: only the spelling
    let p = search::SearchParams { name_matching: Some("off".into()), ..params("mohammed") };
    assert_eq!(ids(&run(&setup(true, names), &p)), ["2"]);
    assert_eq!(ids(&run(&setup(false, names), &params("mohammed"))), ["2"]);
    let p = search::SearchParams { num_typos: Some("0".into()), ..always("mohammed") };
    assert_eq!(ids(&run(&setup(true, names), &p)), ["2"]);
}

#[test]
fn exact_before_fold_before_sound() {
    // the spelling, then a typo (cost 1, as a fold), then a sound (cost 2)
    let names: &[&[&str]] = &[&["Muhamad"], &["Mohammed"], &["Mohamed"]];
    let p = search::SearchParams { prefix: Some("false".into()), ..always("mohammed") };
    let r = run(&setup(true, names), &p);
    assert_eq!(ids(&r), ["2", "3", "1"]);
    assert_eq!(explained(&r, "3", "mohammed").map(|m| m.1), Some("typo"));
    assert_eq!(explained(&r, "1", "mohammed").map(|m| m.1), Some("phonetic"));

    // a short word has no typos but still its keys
    let names: &[&[&str]] = &[&["Aase Berg"], &["Ase Berg"]];
    assert_eq!(sorted(ids(&run(&setup(true, names), &always("ase")))), ["1", "2"]);
}

#[test]
fn name_matching_is_refused_on_other_fields() {
    let registry = setup(false, &[&["Anna"]]);
    let collection = registry.get("people").unwrap();
    let index = collection.snapshot();
    assert!(search::execute(&index, &collection.schema, &always("anna")).is_err());
    let p = search::SearchParams { name_matching: Some("sometimes".into()), ..params("anna") };
    assert!(search::execute(&index, &collection.schema, &p).is_err());
    // off is what such a field does anyway
    let p = search::SearchParams { name_matching: Some("off".into()), ..params("anna") };
    assert_eq!(ids(&run(&registry, &p)), ["1"]);
}

#[test]
fn letters_without_transliteration_are_indexed_and_searched() {
    // from a sanctions list: a small capital N, which the tokenizer keeps and the encoders must skip
    let names: &[&[&str]] = &[&["Aɴna Svensson"], &["ᴀʟɪ Hassan"]];
    assert_eq!(ids(&run(&setup(true, names), &always("aɴna"))), ["1"]);
    assert_eq!(ids(&run(&setup(true, names), &always("ᴀʟɪ"))), ["2"]);
    assert_eq!(ids(&run(&setup(true, names), &always("svenson"))), ["1"]);
}

#[test]
fn a_word_with_digits_is_not_sounded_out() {
    // a UK entry searched on the UN list: "3m" sounded as "M" found every short M word once
    // the other words were dropped, and never got to the entry
    let names: &[&[&str]] = &[&["3MG", "MIZAN MACHINERY MANUFACTURING"], &["M. BABAIE INDUSTRIES"], &["MAEI", "MINISTRY OF ATOMIC ENERGY INDUSTRY"]];
    let q = "3M MIZAN MACHINERY MANUFACTURING";
    assert_eq!(ids(&run(&setup(true, names), &params(q))), ["1"]);
    assert_eq!(ids(&run(&setup(true, names), &always(q))), ["1"]);
    assert_eq!(ids(&run(&setup(false, names), &params(q))), ["1"]);
}

/// `name_score=true`, with every spelling searched.
fn scored(q: &str) -> search::SearchParams {
    search::SearchParams { name_score: Some(true), ..always(q) }
}

fn name_scores(r: &search::SearchResult) -> Vec<(String, f32)> {
    r.hits.iter().map(|h| (h.document["id"].as_str().unwrap().to_string(), h.name_score.as_ref().expect("scored").score)).collect()
}

#[test]
fn name_score_orders_candidates_by_name() {
    let names: &[&[&str]] = &[
        &["Hamza bin Laden"],
        &["Laden Osama Trading Company"],
        &["Usama bin Muhammad bin Awad BIN LADIN", "Osama bin Laden"],
        &["Usama Muhammed Awad Bin Laden"],
    ];
    let r = run(&setup(true, names), &scored("Osama bin Laden"));
    let s = name_scores(&r);
    assert_eq!(s[0], ("3".to_string(), 1.0), "the exact alias first: {s:?}");
    assert!(s.windows(2).all(|w| w[0].1 >= w[1].1), "ordered by score: {s:?}");
    // another man of the family, when the search keeps him (words dropped)
    assert!(s.iter().filter(|(id, _)| id == "1").all(|h| h.1 < 0.7), "{s:?}");
    assert!(s.iter().find(|(id, _)| id == "4").is_some_and(|h| h.1 > 0.7), "the UK spelling: {s:?}");

    // the evidence: the best name, how each part paired, what is left over
    let best = r.hits[0].name_score.as_ref().unwrap();
    assert_eq!((best.field.as_str(), best.name.as_str()), ("names", "Osama bin Laden"));
    let hit = r.hits.iter().find(|h| h.document["id"] == "4").unwrap().name_score.as_ref().unwrap();
    let kinds: Vec<(&str, &str, &str)> = hit.pairs.iter().map(|p| (p.query.as_str(), p.name.as_str(), p.kind)).collect();
    assert!(kinds.contains(&("osama", "usama", "typo")), "{kinds:?}");
    assert!(hit.unmatched_name.contains(&"awad".to_string()), "{:?}", hit.unmatched_name);

    // without it, nothing is scored
    let r = run(&setup(true, names), &always("Osama bin Laden"));
    assert!(r.hits.iter().all(|h| h.name_score.is_none()));
}

#[test]
fn name_score_pages_within_its_candidates() {
    let names: &[&[&str]] = &[&["Anna Karlsson"], &["Anna Carlsson"], &["Anna Maria Karlsson"], &["Karl Annasson"]];
    let all = name_scores(&run(&setup(true, names), &scored("Anna Karlsson")));
    assert_eq!(all[0].0, "1");
    let p = search::SearchParams { per_page: 1, page: 2, ..scored("Anna Karlsson") };
    let r = run(&setup(true, names), &p);
    assert_eq!(name_scores(&r), all[1..2]);
    assert_eq!(r.found, all.len(), "found stays the search's");
    // a window of two: the third is not scored, so not returned
    let p = search::SearchParams { per_page: 1, page: 3, name_score_candidates: Some(2), ..scored("Anna Karlsson") };
    assert!(run(&setup(true, names), &p).hits.is_empty());
}

#[test]
fn name_score_is_refused_where_it_cannot_score() {
    let registry = setup(true, &[&["Anna"]]);
    let collection = registry.get("people").unwrap();
    let index = collection.snapshot();
    let schema = &collection.schema;
    assert!(search::execute(&index, schema, &scored("*")).is_err(), "no name to score");
    let p = search::SearchParams { group_by: Some("names".into()), ..scored("anna") };
    assert!(search::execute(&index, schema, &p).is_err(), "groups");
    let p = search::SearchParams { name_score_kind: Some("animal".into()), ..scored("anna") };
    assert!(search::execute(&index, schema, &p).is_err(), "a kind of name it does not know");
    let p = search::SearchParams { name_score_kind: Some("person".into()), ..scored("anna") };
    assert!(search::execute(&index, schema, &p).is_ok());
    let registry = setup(false, &[&["Anna"]]);
    let collection = registry.get("people").unwrap();
    let p = search::SearchParams { name_score: Some(true), ..params("anna") };
    assert!(search::execute(&collection.snapshot(), &collection.schema, &p).is_err(), "no name-matching field");
}
