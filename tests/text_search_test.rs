//! Text search through `search::execute`: the query syntax, how words become terms, and the order
//! of what is found, as Typesense's documentation describes the parameters. `text_match` values are
//! Torque's own (BM25F behind words matched and typos), so only their order is asserted.

use serde_json::{json, Value};
use std::sync::Arc;
use torque::collection::{Collection, CollectionRegistry};
use torque::ingest;
use torque::schema::Schema;
use torque::search::{self, SearchParams, SearchResponse, SearchResult};

struct Fixture {
    _registry: CollectionRegistry,
    collection: Arc<Collection>,
}

fn fixture(schema: Value, docs: &[Value]) -> Fixture {
    let mut schema: Schema = serde_json::from_value(schema).unwrap();
    schema.assign_field_ids();
    let registry = CollectionRegistry::new(None);
    let collection = registry.create(schema).unwrap();
    let parsed = docs.iter().map(|d| ingest::parse_json_document(d, &collection.schema, 0).unwrap()).collect();
    collection.swap_index(ingest::build_index(&collection.schema, parsed));
    Fixture { _registry: registry, collection }
}

/// One string field `t`, the documents numbered from 1.
fn titles(titles: &[&str]) -> Fixture {
    titled(json!({"name": "c", "fields": [{"name": "t", "type": "string"}]}), titles)
}

fn titled(schema: Value, titles: &[&str]) -> Fixture {
    let docs: Vec<Value> = titles.iter().enumerate().map(|(i, t)| json!({"id": (i + 1).to_string(), "t": t})).collect();
    fixture(schema, &docs)
}

fn params(q: &str) -> SearchParams {
    SearchParams { q: q.into(), query_by: vec!["t".into()], per_page: 20, page: 1, ..Default::default() }
}

impl Fixture {
    fn search(&self, p: &SearchParams) -> SearchResult {
        match search::execute(&self.collection.snapshot(), &self.collection.schema, p).unwrap() {
            SearchResponse::Ungrouped(r) => r,
            _ => panic!("expected an ungrouped result"),
        }
    }

    fn ids(&self, p: &SearchParams) -> Vec<String> {
        self.search(p).hits.iter().map(|h| h.document["id"].as_str().unwrap().to_string()).collect()
    }

    fn q(&self, q: &str) -> Vec<String> {
        self.ids(&params(q))
    }
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn a_phrase_finds_its_words_in_order() {
    let f = titles(&["new york pizza", "york new pizza", "new pizza in york"]);
    assert_eq!(f.q("\"new york\""), ["1"]);
    assert_eq!(f.q("pizza \"new york\""), ["1"]);
    assert_eq!(f.q("\"pizza york\""), Vec::<String>::new());
    // a word of a phrase has no typos and is no prefix
    assert_eq!(f.q("\"new yorc\""), Vec::<String>::new());
    assert_eq!(f.q("\"new yo\""), Vec::<String>::new());

    // only the phrase is marked, not its words elsewhere
    let r = f.search(&params("\"new york\""));
    assert_eq!(r.hits[0].highlight["t"]["snippet"], "<mark>new</mark> <mark>york</mark> pizza");
}

#[test]
fn a_phrase_stays_within_one_array_element() {
    let f = fixture(
        json!({"name": "c", "fields": [{"name": "t", "type": "string[]"}]}),
        &[json!({"id": "1", "t": ["red shoe", "blue"]}), json!({"id": "2", "t": ["shoe", "red"]}), json!({"id": "3", "t": ["red", "shoe"]})],
    );
    assert_eq!(f.q("\"red shoe\""), ["1"]);
}

#[test]
fn an_exclusion_leaves_its_documents_out() {
    let f = titles(&["new york pizza", "chicago pizza", "new delhi pizza"]);
    assert_eq!(sorted(f.q("pizza -chicago")), ["1", "3"]);
    assert_eq!(f.q("pizza -\"new york\" -chicago"), ["3"]);
    // nothing but an exclusion: every other document
    assert_eq!(sorted(f.q("-chicago")), ["1", "3"]);
}

#[test]
fn punctuation_inside_a_word_is_dropped_unless_it_separates() {
    let schema = |separators: Value| json!({"name": "c", "token_separators": separators, "fields": [{"name": "t", "type": "string"}]});
    let f = titled(schema(json!([])), &["non-stick pan", "556000-1234"]);
    assert_eq!(f.q("nonstick"), ["1"]);
    assert_eq!(f.q("non-stick"), ["1"]);
    assert_eq!(f.q("5560001234"), ["2"]);
    let p = SearchParams { num_typos: Some("0".into()), prefix: Some("false".into()), ..params("stick") };
    assert!(f.ids(&p).is_empty(), "one word, `nonstick`");

    let f = titled(schema(json!(["-"])), &["non-stick pan"]);
    assert_eq!(f.ids(&p), ["1"]);
}

#[test]
fn indexed_symbols_are_part_of_a_word() {
    let f = titled(json!({"name": "c", "symbols_to_index": ["+"], "fields": [{"name": "t", "type": "string"}]}), &["learn c++ today", "learn c today"]);
    assert_eq!(f.q("c++"), ["1"]);
    let p = SearchParams { num_typos: Some("0".into()), prefix: Some("false".into()), ..params("c") };
    assert_eq!(f.ids(&p), ["2"]);
}

#[test]
fn a_prefix_keeps_its_accents() {
    let f = titles(&["trädgård", "transport"]);
    let p = |q: &str| SearchParams { num_typos: Some("0".into()), ..params(q) };
    assert_eq!(f.ids(&p("trä")), ["1"], "ä is not a");
    assert_eq!(sorted(f.ids(&p("tra"))), ["1", "2"], "typed without accents, both");
    assert_eq!(f.ids(&p("trädgård")), ["1"]);
    assert_eq!(f.ids(&p("tradgard")), ["1"], "a whole word folds");
}

#[test]
fn a_prefix_finds_words_by_how_they_are_written_when_they_are_stemmed() {
    let schema = json!({"name": "c", "fields": [{"name": "t", "type": "string", "stem": true}]});
    let f = titled(schema, &["running shoes", "gamer"]);
    let p = |q: &str| SearchParams { num_typos: Some("0".into()), ..params(q) };
    assert_eq!(f.ids(&p("runni")), ["1"], "the term is `run`");
    assert_eq!(f.search(&p("runni")).hits[0].highlight["t"]["snippet"], "<mark>runni</mark>ng shoes");
}

#[test]
fn more_words_rank_before_fewer() {
    let f = titles(&["blue shoes", "blue blue blue blue", "shoes for you"]);
    assert_eq!(f.q("blue shoes")[0], "1");
    let p = SearchParams { drop_tokens_threshold: Some(10), ..params("blue shoes") };
    let r = f.ids(&p);
    assert_eq!(r, ["1", "2"], "the full match first, then \"blue\": the last word goes first");
}

#[test]
fn a_word_as_typed_ranks_before_its_completions_and_typos() {
    let f = titles(&["shoe", "shoes"]);
    assert_eq!(f.q("shoe"), ["1", "2"]);
    // typos widen only below the threshold
    let f = titles(&["lamp", "lamb"]);
    assert_eq!(f.q("lamp"), ["1"]);
    let p = SearchParams { typo_tokens_threshold: Some(5), ..params("lamp") };
    assert_eq!(f.ids(&p), ["1", "2"]);
}

#[test]
fn exhaustive_search_looks_past_what_it_found() {
    let f = titles(&["lamp", "lamb"]);
    let p = SearchParams { exhaustive_search: Some(true), ..params("lamp") };
    assert_eq!(f.ids(&p), ["1", "2"]);
}

#[test]
fn a_field_holding_just_the_query_ranks_first() {
    // the same frequency and length norm would tie: the short field also is the whole value
    let f = titles(&["acme corporation of sweden holdings", "acme"]);
    assert_eq!(f.q("acme"), ["2", "1"]);
    let r = f.search(&params("acme"));
    assert!(r.hits[0].text_match > r.hits[1].text_match);
}

#[test]
fn earlier_fields_weigh_more_and_given_weights_count() {
    let f = fixture(
        json!({"name": "c", "fields": [{"name": "a", "type": "string"}, {"name": "b", "type": "string"}]}),
        &[json!({"id": "1", "a": "other", "b": "needle"}), json!({"id": "2", "a": "needle", "b": "other"})],
    );
    let p = |by: &[&str]| SearchParams { query_by: by.iter().map(|s| s.to_string()).collect(), ..params("needle") };
    assert_eq!(f.ids(&p(&["a", "b"])), ["2", "1"]);
    assert_eq!(f.ids(&p(&["b", "a"])), ["1", "2"]);
    let weighted = SearchParams { query_by_weights: Some("1,5".into()), ..p(&["a", "b"]) };
    assert_eq!(f.ids(&weighted), ["1", "2"]);
    let info = f.search(&weighted).hits[0].text_match_info.clone().unwrap();
    assert_eq!(info.best_field_weight, 5);
}

#[test]
fn text_match_type_chooses_how_fields_combine() {
    let f = fixture(
        json!({"name": "c", "fields": [{"name": "a", "type": "string"}, {"name": "b", "type": "string"}]}),
        &[json!({"id": "1", "a": "needle", "b": "needle"}), json!({"id": "2", "a": "needle", "b": "x"})],
    );
    let p = |t: &str| SearchParams { text_match_type: Some(t.into()), prioritize_num_matching_fields: Some(false), query_by: vec!["a".into(), "b".into()], ..params("needle") };
    let score = |t: &str| f.search(&p(t)).hits.iter().find(|h| h.document["id"] == "1").unwrap().text_match.unwrap();
    assert!(score("sum_score") > score("max_score"));
    let bad = search::execute(&f.collection.snapshot(), &f.collection.schema, &p("nope"));
    assert!(bad.is_err());
}

#[test]
fn matching_more_fields_ranks_first_when_asked() {
    let f = fixture(
        json!({"name": "c", "fields": [{"name": "a", "type": "string"}, {"name": "b", "type": "string"}]}),
        &[json!({"id": "1", "a": "needle", "b": "x"}), json!({"id": "2", "a": "needle y", "b": "needle y"})],
    );
    let p = SearchParams { query_by: vec!["a".into(), "b".into()], prioritize_exact_match: Some(false), ..params("needle") };
    assert_eq!(f.ids(&p), ["2", "1"]);
    assert_eq!(f.search(&p).hits[0].text_match_info.as_ref().unwrap().fields_matched, 2);
}

#[test]
fn drop_modes_go_from_their_side() {
    let f = titles(&["alpha", "gamma"]);
    let p = |mode: &str| SearchParams { drop_tokens_mode: Some(mode.into()), drop_tokens_threshold: Some(2), num_typos: Some("0".into()), prefix: Some("false".into()), split_join_tokens: Some("off".into()), ..params("alpha beta gamma") };
    // "beta" matches nothing and goes first: "alpha gamma" still matches nothing together
    assert_eq!(f.ids(&p("right_to_left")), ["1"]);
    assert_eq!(f.ids(&p("left_to_right")), ["2"]);
    assert_eq!(sorted(f.ids(&p("both_sides:3"))), ["1", "2"]);
    let bad = search::execute(&f.collection.snapshot(), &f.collection.schema, &p("sideways"));
    assert!(bad.is_err());
}

#[test]
fn text_match_info_says_what_matched() {
    let f = titles(&["blue shoes"]);
    let info = f.search(&params("blue shoes red")).hits[0].text_match_info.clone().unwrap();
    assert_eq!((info.tokens_matched, info.num_tokens_dropped, info.fields_matched), (2, 1, 1));
    assert_eq!(info.typo_prefix_score, 0);
}

#[test]
fn highlights_mark_what_the_search_matched() {
    let f = titles(&["The laptop computer"]);
    assert_eq!(f.search(&params("laptpo")).hits[0].highlight["t"]["snippet"], "The <mark>laptop</mark> computer");
    assert_eq!(f.search(&params("lap")).hits[0].highlight["t"]["snippet"], "The <mark>lap</mark>top computer");
    let p = SearchParams { highlight_start_tag: Some("[".into()), highlight_end_tag: Some("]".into()), ..params("computer") };
    assert_eq!(f.search(&p).hits[0].highlight["t"]["snippet"], "The laptop [computer]");
}

#[test]
fn a_word_still_being_typed_may_hold_a_typo() {
    let f = titles(&["stockholm", "goteborg"]);
    assert_eq!(f.q("stokh"), ["1"]);
    assert_eq!(f.search(&params("stokh")).hits[0].highlight["t"]["snippet"], "<mark>stockholm</mark>");
    let whole = SearchParams { prefix: Some("false".into()), ..params("stokh") };
    assert!(f.ids(&whole).is_empty(), "not the whole word");
}

#[test]
fn synonyms_rewrite_several_words_and_both_ways() {
    use torque::synonyms::{SynonymItem, SynonymSet, SynonymStore};
    let store = SynonymStore::new();
    store.upsert_set(SynonymSet {
        name: "s".into(),
        items: vec![
            SynonymItem { id: "1".into(), synonyms: vec!["sneakers".into(), "tennis shoes".into()], ..Default::default() },
            SynonymItem { id: "2".into(), root: Some("pants".into()), synonyms: vec!["trousers".into()], ..Default::default() },
        ],
    });
    let f = titles(&["white sneakers", "white tennis shoes", "blue trousers", "blue pants"]);
    let run = |q: &str| -> Vec<String> {
        let p = SearchParams { synonym_sets: Some("s".into()), ..params(q) };
        let r = search::execute_with_stores(&f.collection.snapshot(), &f.collection.schema, &p, Some(&store), None).unwrap();
        let SearchResponse::Ungrouped(r) = r else { panic!() };
        sorted(r.hits.iter().map(|h| h.document["id"].as_str().unwrap().to_string()).collect())
    };
    assert_eq!(run("white sneakers"), ["1", "2"]);
    assert_eq!(run("tennis shoes"), ["1", "2"]);
    // one-way: from the root only
    assert_eq!(run("pants"), ["3", "4"]);
    assert_eq!(run("trousers"), ["3"]);
}

#[test]
fn a_filter_on_a_whole_value_reads_the_field_as_tokens() {
    let f = titles(&["Acme", "Acme Holding", "Holding Acme"]);
    let p = |filter: &str| SearchParams { filter_by: Some(filter.into()), ..params("*") };
    assert_eq!(f.ids(&p("t:=acme")), ["1"], "one word, by the field's length");
    assert_eq!(f.ids(&p("t:=Acme Holding")), ["2"], "several, by the stored text");
    assert_eq!(sorted(f.ids(&p("t:Acme Holding"))), ["2", "3"]);
}

#[test]
fn filters_on_stemmed_facets_answer_from_their_values() {
    let f = fixture(
        json!({"name": "c", "fields": [{"name": "t", "type": "string", "stem": true, "facet": true}, {"name": "tags", "type": "string[]", "stem": true, "optional": true}]}),
        &[
            json!({"id": "1", "t": "Stockholms län", "tags": ["running shoes", "red"]}),
            json!({"id": "2", "t": "Skåne län", "tags": ["red"]}),
            json!({"id": "3", "t": "Stockholms län"}),
        ],
    );
    let p = |filter: &str| SearchParams { filter_by: Some(filter.into()), ..params("*") };
    assert_eq!(sorted(f.ids(&p("t:=Stockholms län"))), ["1", "3"]);
    assert_eq!(f.ids(&p("t:=Skåne län")), ["2"]);
    assert_eq!(sorted(f.ids(&p("t:!=Skåne län"))), ["1", "3"]);
    assert!(f.ids(&p("t:=Stockholms")).is_empty(), "a part of the value");
    // an array field's values, stemmed as indexed
    assert_eq!(f.ids(&p("tags:=running shoes")), ["1"]);
}

#[test]
fn stopwords_with_accents_leave_the_query() {
    let store = torque::stopwords::StopwordStore::new();
    store.upsert(torque::stopwords::StopwordSet::new("sv".into(), ["för".to_string()].into_iter().collect(), None));
    let f = titles(&["för alla", "alla"]);
    let p = SearchParams { stopwords: Some("sv".into()), ..params("för") };
    let r = search::execute_with_stores(&f.collection.snapshot(), &f.collection.schema, &p, None, Some(&store)).unwrap();
    let SearchResponse::Ungrouped(r) = r else { panic!() };
    assert_eq!(r.found, 0, "every word a stopword");
}
