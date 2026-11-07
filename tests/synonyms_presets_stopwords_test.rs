use std::collections::HashSet;
use std::sync::Arc;
use torque::collection::CollectionRegistry;
use torque::ingest::build_index;
use torque::presets::PresetStore;
use torque::schema::*;
use torque::search;
use torque::stopwords::{StopwordSet, StopwordStore};
use torque::synonyms::{SynonymItem, SynonymSet, SynonymStore};

// -- Test helpers --

fn make_field(name: &str, field_type: FieldType) -> FieldDef {
    FieldDef {
        name: name.into(),
        field_type,
        facet: false,
        index: true,
        optional: false,
        sort: false,
        stem: true,
        infix: false,
        locale: None,
        num_dim: None,
        vec_dist: None,
        id: 0,
        reference: None,
        drop: false,
        stem_dictionary: None,
        embed: None,
        name_matching: false,
    }
}

fn make_field_sort(name: &str, field_type: FieldType) -> FieldDef {
    FieldDef {
        name: name.into(),
        field_type,
        facet: false,
        index: true,
        optional: false,
        sort: true,
        stem: true,
        infix: false,
        locale: None,
        num_dim: None,
        vec_dist: None,
        id: 0,
        reference: None,
        drop: false,
        stem_dictionary: None,
        embed: None,
        name_matching: false,
    }
}

fn shoe_schema() -> Schema {
    Schema {
        index_mode: Default::default(),
        name: "shoes".into(),
        fields: vec![
            make_field("title", FieldType::String),
            make_field_sort("price", FieldType::Float64),
        ],
        default_sorting_field: None,
        enable_nested_fields: false,
        symbols_to_index: vec![],
        token_separators: vec![],
        default_language: Language::default(),
        persistence_enabled: true,
        name_to_id: Default::default(),
        geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
    }
}

fn setup_shoes() -> (Arc<CollectionRegistry>, Schema) {
    let mut schema = shoe_schema();
    schema.assign_field_ids();

    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema.clone()).unwrap();

    let collection = registry.get("shoes").unwrap();

    // Words chosen so stemming behavior is predictable:
    // "red" → "red", "blue" → "blue", "fast" → "fast", "quick" → "quick"
    // "big" → "big", "large" → "larg", "small" → "small", "tiny" → "tini"
    // Use simple words that stem to themselves for reliable synonym matching
    let docs = vec![
        Document {
            id: 0,
            external_id: "0".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("red fast car".into()),
                FieldValue::Float64(89.99),
            ].into(),
            record: None,
        },
        Document {
            id: 1,
            external_id: "1".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("blue quick van".into()),
                FieldValue::Float64(129.99),
            ].into(),
            record: None,
        },
        Document {
            id: 2,
            external_id: "2".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("red slow bus".into()),
                FieldValue::Float64(69.99),
            ].into(),
            record: None,
        },
        Document {
            id: 3,
            external_id: "3".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("green big truck".into()),
                FieldValue::Float64(159.99),
            ].into(),
            record: None,
        },
        Document {
            id: 4,
            external_id: "4".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("the red fast bike".into()),
                FieldValue::Float64(99.99),
            ].into(),
            record: None,
        },
    ];

    let index = build_index(&collection.schema, docs);
    collection.swap_index(index);

    (registry, (*collection.schema).clone())
}

fn make_params(q: &str) -> search::SearchParams {
    search::SearchParams {
        q: q.into(),
        query_by: vec!["title".into()],
        filter_by: None,
        sort_by: None,
        facet_by: None,
        per_page: 10,
        page: 1,
        ..Default::default()
    }
}

fn unwrap_ungrouped(resp: search::SearchResponse) -> search::SearchResult {
    match resp {
        search::SearchResponse::Ungrouped(r) => r,
        _ => panic!("Expected ungrouped result"),
    }
}

// ===== Synonym Tests =====

#[test]
fn synonym_multi_way_expansion() {
    let (registry, schema) = setup_shoes();
    let collection = registry.get("shoes").unwrap();
    let index = collection.snapshot();

    // Without synonyms: "quick" finds only doc 1
    let params = make_params("quick");
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.found, 1);

    // Create synonym store: fast ↔ quick (multi-way)
    let syn_store = SynonymStore::new();
    syn_store.upsert_set(SynonymSet {
        name: "speed".into(),
        items: vec![SynonymItem {
            id: "1".into(),
            synonyms: vec!["fast".into(), "quick".into()],
            ..Default::default()
        }],
    });

    // With synonyms: "quick" should also find docs containing "fast"
    let mut params = make_params("quick");
    params.synonym_sets = Some("speed".into());
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, Some(&syn_store), None).unwrap(),
    );
    // doc 0 has "fast", doc 1 has "quick", doc 4 has "fast"
    assert_eq!(result.found, 3);
}

#[test]
fn synonym_one_way_expansion() {
    let (registry, schema) = setup_shoes();
    let collection = registry.get("shoes").unwrap();
    let index = collection.snapshot();

    // One-way: "slow" → fast, quick
    // "slow" stems to "slow" (invariant), so lookup works
    let syn_store = SynonymStore::new();
    syn_store.upsert_set(SynonymSet {
        name: "speed".into(),
        items: vec![SynonymItem {
            id: "1".into(),
            synonyms: vec!["fast".into(), "quick".into()],
            root: Some("slow".into()),
            ..Default::default()
        }],
    });

    // "slow" expands to fast/quick
    let mut params = make_params("slow");
    params.synonym_sets = Some("speed".into());
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, Some(&syn_store), None).unwrap(),
    );
    // "slow" is in doc 2 + expansion adds "fast"(doc 0, 4) and "quick"(doc 1)
    assert_eq!(result.found, 4);

    // "fast" does NOT expand (one-way from "slow" only)
    let mut params = make_params("fast");
    params.synonym_sets = Some("speed".into());
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, Some(&syn_store), None).unwrap(),
    );
    // Only docs with "fast": doc 0, doc 4
    assert_eq!(result.found, 2);
}

#[test]
fn synonym_only_specified_sets_used() {
    let (registry, schema) = setup_shoes();
    let collection = registry.get("shoes").unwrap();
    let index = collection.snapshot();

    let syn_store = SynonymStore::new();
    syn_store.upsert_set(SynonymSet {
        name: "set_a".into(),
        items: vec![SynonymItem {
            id: "1".into(),
            synonyms: vec!["fast".into(), "quick".into()],
            ..Default::default()
        }],
    });
    syn_store.upsert_set(SynonymSet {
        name: "set_b".into(),
        items: vec![SynonymItem {
            id: "1".into(),
            synonyms: vec!["red".into(), "blue".into()],
            ..Default::default()
        }],
    });

    // Use only set_a: "quick" → also "fast"
    let mut params = make_params("quick");
    params.synonym_sets = Some("set_a".into());
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, Some(&syn_store), None).unwrap(),
    );
    assert_eq!(result.found, 3); // fast+quick docs

    // Use only set_b: "quick" has no expansion in set_b
    let mut params = make_params("quick");
    params.synonym_sets = Some("set_b".into());
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, Some(&syn_store), None).unwrap(),
    );
    assert_eq!(result.found, 1); // only "quick"
}

#[test]
fn synonym_delete_stops_expansion() {
    let (registry, schema) = setup_shoes();
    let collection = registry.get("shoes").unwrap();
    let index = collection.snapshot();

    let syn_store = SynonymStore::new();
    syn_store.upsert_set(SynonymSet {
        name: "test".into(),
        items: vec![SynonymItem {
            id: "1".into(),
            synonyms: vec!["fast".into(), "quick".into()],
            ..Default::default()
        }],
    });

    let mut params = make_params("quick");
    params.synonym_sets = Some("test".into());

    // Before delete: expansion works
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, Some(&syn_store), None).unwrap(),
    );
    assert_eq!(result.found, 3);

    // Delete the synonym item
    syn_store.delete_item("test", "1");

    // After delete: no expansion
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, Some(&syn_store), None).unwrap(),
    );
    assert_eq!(result.found, 1);
}

#[test]
fn synonym_no_sets_specified_no_expansion() {
    let (registry, schema) = setup_shoes();
    let collection = registry.get("shoes").unwrap();
    let index = collection.snapshot();

    let syn_store = SynonymStore::new();
    syn_store.upsert_set(SynonymSet {
        name: "test".into(),
        items: vec![SynonymItem {
            id: "1".into(),
            synonyms: vec!["fast".into(), "quick".into()],
            ..Default::default()
        }],
    });

    // No synonym_sets param → no expansion
    let params = make_params("quick");
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, Some(&syn_store), None).unwrap(),
    );
    assert_eq!(result.found, 1);
}

// ===== Stopword Tests =====

#[test]
fn stopwords_strip_from_query() {
    let (registry, schema) = setup_shoes();
    let collection = registry.get("shoes").unwrap();
    let index = collection.snapshot();

    let sw_store = StopwordStore::new();
    sw_store.upsert(StopwordSet::new(
        "english".into(),
        HashSet::from(["the".into()]),
        None,
    ));

    // "the red" → strips "the" (after stemming: "the"→"the"), searches "red"
    let mut params = make_params("the red");
    params.stopwords = Some("english".into());
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, None, Some(&sw_store)).unwrap(),
    );
    // doc 0 "red fast car", doc 2 "red slow bus", doc 4 "the red fast bike"
    assert_eq!(result.found, 3);
}

#[test]
fn stopwords_all_terms_stripped_matches_nothing() {
    let (registry, schema) = setup_shoes();
    let collection = registry.get("shoes").unwrap();
    let index = collection.snapshot();

    let sw_store = StopwordStore::new();
    sw_store.upsert(StopwordSet::new(
        "common".into(),
        HashSet::from(["the".into()]),
        None,
    ));

    // "the" — all terms are stopwords: nothing is left to find
    let mut params = make_params("the");
    params.stopwords = Some("common".into());
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, None, Some(&sw_store)).unwrap(),
    );
    assert_eq!(result.found, 0);
}

#[test]
fn stopwords_no_set_specified_no_stripping() {
    let (registry, schema) = setup_shoes();
    let collection = registry.get("shoes").unwrap();
    let index = collection.snapshot();

    let sw_store = StopwordStore::new();
    sw_store.upsert(StopwordSet::new(
        "english".into(),
        HashSet::from(["red".into()]),
        None,
    ));

    // Without specifying stopwords param, "red" is NOT stripped
    let params = make_params("red");
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, None, Some(&sw_store)).unwrap(),
    );
    // doc 0, doc 2, doc 4 have "red"
    assert_eq!(result.found, 3);
}

#[test]
fn stopwords_delete_set_stops_stripping() {
    let (registry, schema) = setup_shoes();
    let collection = registry.get("shoes").unwrap();
    let index = collection.snapshot();

    let sw_store = StopwordStore::new();
    sw_store.upsert(StopwordSet::new(
        "test".into(),
        HashSet::from(["red".into()]),
        None,
    ));

    // With stopwords: "red" stripped → nothing left to match
    let mut params = make_params("red");
    params.stopwords = Some("test".into());
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, None, Some(&sw_store)).unwrap(),
    );
    assert_eq!(result.found, 0);

    // Delete set: naming a missing set is an error
    sw_store.delete("test");
    let err = search::execute_with_stores(&index, &schema, &params, None, Some(&sw_store)).err();
    assert_eq!(err.as_deref(), Some("Could not find the stopword set named `test`."));

    // Without the parameter "red" is searched as it is
    params.stopwords = None;
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, None, Some(&sw_store)).unwrap(),
    );
    assert_eq!(result.found, 3); // only red docs
}

// ===== Preset Tests =====

#[test]
fn preset_crud() {
    let store = PresetStore::new();

    store.upsert("p1", serde_json::json!({}));
    store.upsert("p2", serde_json::json!({"searches": []}));

    assert_eq!(store.list().len(), 2);
    assert!(store.get("p1").is_some());

    assert!(store.delete("p1").is_some());
    assert_eq!(store.list().len(), 1);
    assert!(store.get("p1").is_none());
    assert!(store.delete("p1").is_none());
}

// ===== Combined Tests =====

#[test]
fn synonyms_and_stopwords_combined() {
    let (registry, schema) = setup_shoes();
    let collection = registry.get("shoes").unwrap();
    let index = collection.snapshot();

    let syn_store = SynonymStore::new();
    syn_store.upsert_set(SynonymSet {
        name: "speed".into(),
        items: vec![SynonymItem {
            id: "1".into(),
            synonyms: vec!["fast".into(), "quick".into()],
            ..Default::default()
        }],
    });

    let sw_store = StopwordStore::new();
    sw_store.upsert(StopwordSet::new(
        "english".into(),
        HashSet::from(["the".into()]),
        None,
    ));

    // "the quick" → strips "the" → expands "quick" to also match "fast"
    let mut params = make_params("the quick");
    params.synonym_sets = Some("speed".into());
    params.stopwords = Some("english".into());

    let result = unwrap_ungrouped(
        search::execute_with_stores(
            &index,
            &schema,
            &params,
            Some(&syn_store),
            Some(&sw_store),
        )
        .unwrap(),
    );
    // "the" stripped, "quick" expanded to also match "fast"
    // doc 0 "fast", doc 1 "quick", doc 4 "fast"
    assert_eq!(result.found, 3);
}

#[test]
fn synonym_store_persistence_roundtrip() {
    let dir = tempfile::tempdir().unwrap();

    let store = SynonymStore::new();
    store.upsert_set(SynonymSet {
        name: "test".into(),
        items: vec![SynonymItem {
            id: "1".into(),
            synonyms: vec!["fast".into(), "quick".into()],
            ..Default::default()
        }],
    });

    torque::persistence::save_synonyms(dir.path(), &store.list_sets()).unwrap();

    let loaded = torque::persistence::load_synonyms(dir.path()).unwrap();
    let store2 = SynonymStore::new();
    store2.load(loaded);

    let set = store2.get_set("test").unwrap();
    assert_eq!(set.items.len(), 1);
    assert_eq!(set.items[0].synonyms, vec!["fast", "quick"]);
}

#[test]
fn preset_persistence_roundtrip() {
    let dir = tempfile::tempdir().unwrap();

    let store = PresetStore::new();
    store.upsert("p1", serde_json::json!({"sort_by": "price:asc"}));

    torque::persistence::save_presets(dir.path(), &store.list()).unwrap();

    let loaded = torque::persistence::load_presets(dir.path()).unwrap();
    let store2 = PresetStore::new();
    store2.load(loaded);

    assert_eq!(store2.get("p1").unwrap()["sort_by"], "price:asc");
}

#[test]
fn stopword_persistence_roundtrip() {
    let dir = tempfile::tempdir().unwrap();

    let store = StopwordStore::new();
    store.upsert(StopwordSet::new(
        "en".into(),
        HashSet::from(["the".into(), "a".into(), "an".into()]),
        Some("en".into()),
    ));

    torque::persistence::save_stopwords(dir.path(), &store.list()).unwrap();

    let loaded = torque::persistence::load_stopwords(dir.path()).unwrap();
    let store2 = StopwordStore::new();
    store2.load(loaded);

    let sw = store2.get_stopwords("en").unwrap();
    assert!(sw.contains("the"));
    assert!(sw.contains("a"));
    assert!(sw.contains("an"));
}

#[test]
fn synonym_num_typos_resolves_corrected_terms() {
    let (registry, schema) = setup_shoes();
    let collection = registry.get("shoes").unwrap();
    let index = collection.snapshot();

    let syn_store = SynonymStore::new();
    syn_store.upsert_set(SynonymSet {
        name: "speed".into(),
        items: vec![SynonymItem {
            id: "1".into(),
            synonyms: vec!["fast".into(), "quick".into()],
            ..Default::default()
        }],
    });

    // "quikc" is a typo of "quick". Without synonym_num_typos the typo path
    // corrects it to "quick" but synonyms never expand a corrected term.
    let mut params = make_params("quikc");
    params.prefix = Some("false".into());
    params.synonym_sets = Some("speed".into());
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, Some(&syn_store), None).unwrap(),
    );
    assert_eq!(result.found, 1, "typo correction alone finds only the 'quick' doc");

    // With synonym_num_typos, the corrected term resolves through synonyms too.
    let mut params = make_params("quikc");
    params.prefix = Some("false".into());
    params.synonym_sets = Some("speed".into());
    params.synonym_num_typos = Some(2);
    let result = unwrap_ungrouped(
        search::execute_with_stores(&index, &schema, &params, Some(&syn_store), None).unwrap(),
    );
    assert_eq!(result.found, 3, "corrected term expands to fast/quick docs");
}
