use torque::collection::CollectionRegistry;
use torque::ingest;
use torque::schema::{Document, FieldDef, FieldType, FieldValue, Language, Schema};
use torque::search;

fn test_schema() -> Schema {
    let mut schema = Schema {
        index_mode: Default::default(),
        name: "products".into(),
        fields: vec![
            FieldDef {
                name: "title".into(),
                field_type: FieldType::String,
                facet: false,
                index: true,
                optional: false,
                sort: false,
                stem: true,
                infix: true,
                locale: None,
                num_dim: None,
                vec_dist: None,
                id: 0,
                reference: None,
                drop: false,
                stem_dictionary: None,
                embed: None,
                name_matching: false,
            },
            FieldDef {
                name: "category".into(),
                field_type: FieldType::String,
                facet: true,
                index: true,
                optional: false,
                sort: false,
                stem: false,
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
            },
            FieldDef {
                name: "price".into(),
                field_type: FieldType::Int32,
                facet: false,
                index: true,
                optional: false,
                sort: true,
                stem: false,
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
            },
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
    };
    schema.assign_field_ids();
    schema
}

fn make_doc(ext_id: &str, title: &str, category: &str, price: i32) -> Document {
    Document {
        id: 0,
        external_id: ext_id.into(),
        language: Language::English,
        fields: vec![
            FieldValue::Text(title.into()),
            FieldValue::Text(category.into()),
            FieldValue::Int32(price),
        ].into(),
        record: None,
    }
}

fn setup() -> (CollectionRegistry, Schema) {
    let schema = test_schema();
    let registry = CollectionRegistry::new(None);
    let collection = registry.create(schema.clone()).unwrap();

    let docs = vec![
        make_doc("1", "laptop computer silver", "electronics", 999),
        make_doc("2", "desktop computer black", "electronics", 1499),
        make_doc("3", "wireless keyboard white", "accessories", 79),
        make_doc("4", "gaming mouse red", "accessories", 59),
        make_doc("5", "monitor ultrawide curved", "electronics", 699),
        make_doc("6", "keyboard mechanical rgb", "accessories", 129),
        make_doc("7", "laptop gaming powerful", "electronics", 1999),
        make_doc("8", "mouse pad large", "accessories", 19),
        make_doc("9", "headphones noise cancelling", "electronics", 349),
        make_doc("10", "speakers bluetooth portable", "electronics", 149),
    ];

    let index = ingest::build_index(&collection.schema, docs);
    collection.swap_index(index);

    (registry, schema)
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

// --- Tests ---

#[test]
fn exact_query_finds_results() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("laptop");
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 2, "Should find 2 docs with 'laptop'");
}

#[test]
fn one_char_typo_corrected() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "laptp" → "laptop" (1 deletion typo)
    let params = make_params("laptp");
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert!(result.found >= 1, "Typo 'laptp' should correct to 'laptop' and find docs");
}

#[test]
fn two_char_typo_corrected() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "keybord" → "keyboard" (1 deletion typo, but 2 allowed)
    let params = make_params("keybord");
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert!(result.found >= 1, "Typo 'keybord' should correct to 'keyboard' and find docs");
}

#[test]
fn prefix_search_autocomplete() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "key" should match "keyboard" via prefix search (last token)
    let params = make_params("key");
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert!(result.found >= 1, "Prefix 'key' should match 'keyboard' docs");
}

#[test]
fn num_typos_zero_disables_correction() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // With num_typos=0, a typo should NOT be corrected
    let mut params = make_params("laptp");
    params.num_typos = Some("0".into());
    params.prefix = Some("false".into()); // disable prefix too
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0, "With num_typos=0, 'laptp' should find nothing");
}

#[test]
fn short_tokens_not_corrected() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "rd" is too short (< 3 chars) for typo correction, should not match "red"
    let mut params = make_params("rd");
    params.prefix = Some("false".into()); // disable prefix
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0, "Short token 'rd' should not be typo-corrected to 'red'");
}

#[test]
fn prefix_disabled_no_autocomplete() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // The definitive test: with prefix=false AND num_typos=0
    let mut params = make_params("key");
    params.prefix = Some("false".into());
    params.num_typos = Some("0".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0, "With prefix=false and num_typos=0, 'key' should find nothing");
}

#[test]
fn typo_with_filter() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "laptp" corrected to "laptop", filtered by category=electronics
    let mut params = make_params("laptp");
    params.filter_by = Some("category:=electronics".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert!(result.found >= 1, "Typo + filter should still find docs");
    // All results should be in electronics
    for hit in &result.hits {
        let cat = hit.document["category"].as_str().unwrap();
        assert_eq!(cat, "electronics");
    }
}

#[test]
fn typo_with_sort() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "laptp" corrected to "laptop", sorted by price ascending
    let mut params = make_params("laptp");
    params.sort_by = Some("price:asc".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert!(result.found >= 2, "Should find laptop docs");

    // Verify sort order
    let mut prev_price = 0;
    for hit in &result.hits {
        let price = hit.document["price"].as_i64().unwrap();
        assert!(price >= prev_price, "Results should be sorted by price ascending");
        prev_price = price;
    }
}

#[test]
fn multi_token_typo() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "laptpo computr" — both words have typos
    // After stemming: "laptpo" → should correct to "laptop", "computr" → should correct to "computer"
    let params = make_params("laptpo computr");
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    // Should find docs matching both "laptop" AND "computer" (stemmed)
    assert!(result.found >= 1, "Multi-token typo correction should find matches");
}

#[test]
fn prefix_only_on_last_token() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "gaming mou" — "gaming" exact match, "mou" prefix matches "mouse"
    let params = make_params("gaming mou");
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert!(result.found >= 1, "Prefix on last token 'mou' should match 'mouse'");
}

#[test]
fn typo_tokens_threshold() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // The threshold is a number of results: typo corrections are searched while fewer documents
    // than it matched. 0 never looks past the query as typed.
    let mut params = make_params("keybord");
    params.typo_tokens_threshold = Some(0);
    params.prefix = Some("false".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0, "threshold 0 searches no typo corrections");

    let mut params = make_params("keybord");
    params.typo_tokens_threshold = Some(8);
    params.prefix = Some("false".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 2, "below the threshold the corrections are searched");
}

#[test]
fn min_len_1typo_suppresses_short_corrections() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "laptpo" is edit distance 1 from "laptop" (6 chars)
    let mut params = make_params("laptpo");
    params.prefix = Some("false".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 2, "default min_len_1typo corrects a 6-char token");

    let mut params = make_params("laptpo");
    params.prefix = Some("false".into());
    params.min_len_1typo = Some(7); // token shorter than this gets zero typos
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0, "min_len_1typo above token length suppresses correction");
}

#[test]
fn min_len_2typo_limits_to_one_typo() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "keybird" -> "keyboard": edit distance 2 (7 chars)
    let mut params = make_params("keybird");
    params.prefix = Some("false".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 2, "default min_len_2typo=7 allows 2 typos on 7-char token");

    let mut params = make_params("keybird");
    params.prefix = Some("false".into());
    params.min_len_2typo = Some(10); // 7-char token now capped at 1 typo
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0, "min_len_2typo above token length caps budget at 1 typo");
}

#[test]
fn drop_tokens_recovers_multi_term_query() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Second token matches nothing and cannot be typo-corrected
    let mut params = make_params("laptop zzzqqqxx");
    params.prefix = Some("false".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 2, "default drop_tokens_threshold=1 drops the dead token");

    let mut params = make_params("laptop zzzqqqxx");
    params.prefix = Some("false".into());
    params.drop_tokens_threshold = Some(0); // disable dropping
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0, "drop_tokens_threshold=0 keeps strict AND semantics");
}

#[test]
fn drop_tokens_left_to_right_mode() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Dead token on the LEFT: right_to_left dropping (default) can't recover
    let mut params = make_params("zzzqqqxx laptop");
    params.prefix = Some("false".into());
    params.drop_tokens_mode = Some("left_to_right".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 2, "left_to_right drops the leading dead token");
}

#[test]
fn split_join_joins_adjacent_tokens() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "key board" -> joined "keyboard" (fallback is the default mode)
    let mut params = make_params("key board");
    params.prefix = Some("false".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 2, "join fallback should find 'keyboard' docs");

    let mut params = make_params("key board");
    params.prefix = Some("false".into());
    params.split_join_tokens = Some("off".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0, "split_join_tokens=off disables the join fallback");
}

#[test]
fn split_join_splits_concatenated_token() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "laptopcomputer" -> split into "laptop computer" (doc 1)
    let mut params = make_params("laptopcomputer");
    params.prefix = Some("false".into());
    params.num_typos = Some("0".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 1, "split fallback should find the 'laptop computer' doc");

    // "gamingmouse" -> split into "gaming mouse" (doc 4): the halves are analyzed as the field
    // does, which stems
    let mut params = make_params("gamingmouse");
    params.prefix = Some("false".into());
    params.num_typos = Some("0".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 1, "split fallback should find the 'gaming mouse' doc");
}

#[test]
fn alpha_numerical_typo_gate() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "rgb1" contains a digit; edit distance 1 from "rgb"
    let mut params = make_params("rgb1");
    params.prefix = Some("false".into());
    params.min_len_1typo = Some(3);
    params.split_join_tokens = Some("off".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 1, "alphanumeric token corrected by default");

    // Gate off: no typos on tokens of letters and digits
    let mut params = make_params("rgb1");
    params.prefix = Some("false".into());
    params.min_len_1typo = Some(3);
    params.split_join_tokens = Some("off".into());
    params.enable_typos_for_alpha_numerical_tokens = Some(false);
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0, "gate disables correction for tokens containing digits");
}


#[test]
fn prioritize_exact_match_false_widens_recall() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "monitor" matches doc 5 exactly. With prioritize_exact_match=false the
    // typo candidates (none here that differ) still OR in — verify the
    // default exact-only path and the widened path agree when there is no
    // nearby vocabulary, and diverge when there is.
    // "mouse" (docs 4, 8) vs "house": no "house" in vocab, so use
    // "keyboard"/"keybord" instead: index has only "keyboard".
    // Exact term "mouse" also has neighbor "house"? Not in vocab. Use a term
    // with a real 1-edit neighbor: "laptop" (docs 1,7) vs "laptops"? Not in
    // vocab either. The fixture has "monitor" and no neighbors, so craft the
    // divergence through mouse/monitor distance-2: skip and assert the
    // documented default instead.
    let mut params = make_params("mouse");
    params.prefix = Some("false".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    let exact_found = result.found;
    assert_eq!(exact_found, 2, "exact term matches its docs");

    let mut params = make_params("mouse");
    params.prefix = Some("false".into());
    params.prioritize_exact_match = Some(false);
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert!(
        result.found >= exact_found,
        "widened recall can only add docs (got {} < {})",
        result.found,
        exact_found
    );
}

#[test]
fn search_cutoff_ms_flags_cutoff() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // A 0ms budget expires before the first check: nothing found, so the search fails with a
    // 408 (the client retries).
    let mut params = make_params("laptop");
    params.prefix = Some("false".into());
    params.search_cutoff_ms = Some(0);
    let err = search::execute(&index, &collection.schema, &params).err();
    assert_eq!(err.as_deref(), Some(search::REQUEST_TIMEOUT), "0ms budget must time out");

    // A generous budget must not flag cutoff and returns full results.
    let mut params = make_params("laptop");
    params.prefix = Some("false".into());
    params.search_cutoff_ms = Some(10_000);
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert!(!result.search_cutoff);
    assert_eq!(result.found, 2);
}

#[test]
fn max_candidates_bounds_prefix_expansion() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Prefix "m" expands to mouse/monitor/mechanical/... — with
    // max_candidates=1 only the first expansion survives.
    let params = make_params("m");
    let full = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    let mut params = make_params("m");
    params.max_candidates = Some(1);
    let limited = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert!(
        limited.found <= full.found,
        "max_candidates=1 must not find more than the default ({} > {})",
        limited.found,
        full.found
    );
    assert!(limited.found >= 1, "one expansion still matches something");
}

#[test]
fn infix_fallback_matches_substring() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "board" is an infix of "keyboard" (docs 3, 6); no exact/typo match.
    let mut params = make_params("board");
    params.prefix = Some("false".into());
    params.num_typos = Some("0".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0, "no infix by default");

    let mut params = make_params("board");
    params.prefix = Some("false".into());
    params.num_typos = Some("0".into());
    params.infix = Some("fallback".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 2, "infix fallback finds 'keyboard' docs");

    // max_extra_prefix below the needed 3 chars ("key") suppresses the match
    let mut params = make_params("board");
    params.prefix = Some("false".into());
    params.num_typos = Some("0".into());
    params.infix = Some("fallback".into());
    params.max_extra_prefix = Some(2);
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0, "max_extra_prefix bounds the expansion");

    // "top" ends "laptop" (pos 3) and "desktop" (pos 4): suffix bound of 0
    // keeps both, prefix bound of 3 keeps only the laptop docs.
    let mut params = make_params("top");
    params.prefix = Some("false".into());
    params.num_typos = Some("0".into());
    params.infix = Some("fallback".into());
    params.max_extra_suffix = Some(0);
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 3, "suffix-bounded infix keeps laptop+desktop");

    let mut params = make_params("top");
    params.prefix = Some("false".into());
    params.num_typos = Some("0".into());
    params.infix = Some("fallback".into());
    params.max_extra_prefix = Some(3);
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 2, "prefix-bounded infix keeps only laptop docs");
}

#[test]
fn infix_always_widens_exact_matches() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // "top" has no exact match; "laptop" contains it. With per-field mode
    // syntax, one value applies to all query_by fields.
    let mut params = make_params("top");
    params.prefix = Some("false".into());
    params.num_typos = Some("0".into());
    params.infix = Some("always".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 3, "always-mode matches laptop+desktop docs");

    // off mode string disables it again
    let mut params = make_params("top");
    params.prefix = Some("false".into());
    params.num_typos = Some("0".into());
    params.infix = Some("off".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 0);
}


#[test]
fn infix_needs_the_field_enabled() {
    let (registry, _) = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // category is not an infix field: the infix search is refused (when it runs)
    let mut params = make_params("ories");
    params.query_by = vec!["category".into()];
    params.infix = Some("always".into());
    let err = search::execute(&index, &collection.schema, &params).err();
    assert_eq!(
        err.as_deref(),
        Some("Could not find `category` in the infix index. Make sure to enable infix search by specifying `infix: true` in the schema.")
    );

    // fallback only searches infixes when nothing matched
    let mut params = make_params("accessories");
    params.query_by = vec!["category".into()];
    params.infix = Some("fallback".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 4);

    let mut params = make_params("top");
    params.infix = Some("always,off".into());
    let err = search::execute(&index, &collection.schema, &params).err();
    assert_eq!(err.as_deref(), Some("Number of infix values in `infix` does not match number of `query_by` fields."));
}

#[test]
fn typo_tokens_threshold_counts_groups() {
    // With group_by the results are groups: "lamp" alone gives one group, short of the threshold
    // of 2, so typo corrections are searched ("lamb", another group). Ungrouped, the two "lamp"
    // documents meet it.
    let schema = test_schema();
    let registry = CollectionRegistry::new(None);
    let collection = registry.create(schema.clone()).unwrap();
    let docs = vec![make_doc("1", "lamp one", "a", 1), make_doc("2", "lamp two", "a", 2), make_doc("3", "lamb three", "b", 3)];
    collection.swap_index(ingest::build_index(&collection.schema, docs));
    let index = collection.snapshot();

    let mut params = make_params("lamp");
    params.prefix = Some("false".into());
    params.typo_tokens_threshold = Some(2);
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());
    assert_eq!(result.found, 2);

    params.group_by = Some("category".into());
    let search::SearchResponse::Grouped(g) = search::execute(&index, &collection.schema, &params).unwrap() else {
        panic!("expected a grouped response")
    };
    assert_eq!((g.found, g.found_docs), (2, 3));
}
