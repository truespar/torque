//! String filters (`TextFilter`, `filter_by` on a string field) over a segment.
//!
//! A field with a text index answers with the documents holding every term of the value (the
//! terms' postings intersected); for `:=` and a phrase those whose value, or one array element,
//! holds the terms where they must be, read from their stored text. A faceted field can answer from
//! its distinct values instead, each tokenized as the field indexes it: done when there are fewer of
//! them than documents to check. A field without a text index answers from its values. Each way
//! finds the same documents: a document's text is analyzed in its language, as the text index holds
//! it, and the filter's words stand for their terms in every language (`TextFilter::others`), so
//! equal text matches whatever a document's language. A field without a text index compares in the
//! collection's language.

use super::{Match, TextFilter};
use crate::bitmap::Bitmap;
use crate::index::inverted::InvertedIndex;
use crate::index::values::{DocSet, ValueIndex};
use crate::index::Segment;
use crate::schema::{DocId, FieldDef, FieldId, FieldType, FieldValue, Language, Schema};
use crate::search::text::Deadline;

/// The segment's documents matching a string filter on field `fid`.
pub fn docs(fid: FieldId, filter: &TextFilter, seg: &Segment, schema: &Schema) -> Bitmap {
    docs_until(fid, filter, seg, schema, None)
}

/// `docs`, the documents checked by their stored text only until `deadline`.
pub fn docs_until(fid: FieldId, filter: &TextFilter, seg: &Segment, schema: &Schema, deadline: Option<Deadline>) -> Bitmap {
    let Some(def) = schema.fields.get(fid as usize) else { return Bitmap::new() };
    let values = seg.values.get(&fid).filter(|v| v.is_str());
    let Some(inv) = seg.inverted.get(&fid) else {
        return values.and_then(|v| value_docs(v, filter, schema, def, &[schema.default_language])).unwrap_or_default();
    };
    if filter.how != Match::Words {
        if let Some(v) = values.filter(|v| v.len() < to_check(inv, filter, v.len())) {
            // a stemmed field's value can match in one language and not in another: its documents'
            // languages decide, which the text index knows
            let languages: &[Language] = if def.stem { &Language::ALL } else { &[schema.default_language] };
            if let Some(docs) = value_docs(v, filter, schema, def, languages) {
                return docs;
            }
        }
    }
    index_docs(inv, filter, seg, schema, def, deadline)
}

/// Whether a string `value` of field `def`, in a document of `language`, matches the filter.
pub fn value_holds(value: &str, filter: &TextFilter, schema: &Schema, def: &FieldDef, language: Language) -> bool {
    let mut terms = Terms::default();
    terms.read(value, schema, def, language);
    filter.holds(&terms.slices())
}

// -- the text index ----------------------------------------------------------------------------------

/// The documents holding the `k`-th term in one of its forms (None: none does).
fn term_docs(inv: &InvertedIndex, filter: &TextFilter, k: usize) -> Option<Bitmap> {
    let mut docs: Option<Bitmap> = None;
    for form in filter.forms(k) {
        if let Some(p) = inv.postings(form) {
            docs = Some(match docs {
                None => p,
                Some(d) => &d | &p,
            });
        }
    }
    docs
}

/// About how many documents the text index would check (more than `limit` once past it): the
/// fewest documents of a whole term, else those of the prefix's terms.
fn to_check(inv: &InvertedIndex, filter: &TextFilter, limit: u64) -> u64 {
    if let Some(n) = (0..filter.fixed()).map(|k| filter.forms(k).map(|t| u64::from(inv.doc_freq(t))).sum::<u64>()).min() {
        return n;
    }
    let mut n = 0u64;
    for form in filter.forms(0) {
        inv.for_each_with_prefix(form, |r| {
            n += u64::from(inv.df_of(r));
            n <= limit
        });
    }
    n
}

fn index_docs(inv: &InvertedIndex, filter: &TextFilter, seg: &Segment, schema: &Schema, def: &FieldDef, deadline: Option<Deadline>) -> Bitmap {
    let mut postings: Vec<Bitmap> = Vec::with_capacity(filter.terms.len());
    for k in 0..filter.fixed() {
        match term_docs(inv, filter, k) {
            Some(p) => postings.push(p),
            None => return Bitmap::new(),
        }
    }
    postings.sort_by_key(|p| p.cardinality());
    let base = postings.split_first().map(|(first, rest)| {
        let mut acc = first.clone();
        for p in rest {
            acc &= p;
        }
        acc
    });
    if base.as_ref().is_some_and(Bitmap::is_empty) {
        return Bitmap::new();
    }
    let cand = if filter.prefix {
        let mut set = DocSet::new(inv.total_docs);
        for last in filter.forms(filter.terms.len() - 1) {
            inv.for_each_with_prefix(last, |r| {
                set.add_bitmap(&inv.postings_of(r));
                true
            });
        }
        let any = set.finish();
        match base {
            Some(b) => &b & &any,
            None => any,
        }
    } else {
        base.unwrap_or_default()
    };
    if filter.how == Match::Words {
        return cand;
    }
    // a string's whole value has as many words as the filter: most candidates go by their length,
    // and one word needs no more than that
    if filter.how == Match::Exact && !filter.prefix && matches!(def.field_type, FieldType::String | FieldType::Text) {
        let n = filter.terms.len() as u32;
        let whole: Vec<DocId> = cand.iter().filter(|&d| inv.doc_length(d) == n).collect();
        let whole = Bitmap::of(&whole);
        return if n == 1 { whole } else { verify(filter, &whole, seg, schema, def, deadline) };
    }
    verify(filter, &cand, seg, schema, def, deadline)
}

/// The candidates whose value, or one array element, holds the terms where the filter requires,
/// from their stored text (in the document's language, as the text index holds it).
fn verify(filter: &TextFilter, cand: &Bitmap, seg: &Segment, schema: &Schema, def: &FieldDef, deadline: Option<Deadline>) -> Bitmap {
    let mut terms = Terms::default();
    let mut out = Vec::new();
    for (i, d) in cand.iter().enumerate() {
        if i % 1024 == 0 && deadline.is_some_and(|t| t.passed()) {
            break;
        }
        let Some(doc) = seg.get_doc(d, schema) else { continue };
        let holds = |text: &str, terms: &mut Terms| {
            terms.read(text, schema, def, doc.language);
            filter.holds(&terms.slices())
        };
        let hit = match doc.fields.get(def.id as usize) {
            Some(FieldValue::Text(text)) => holds(text, &mut terms),
            Some(FieldValue::StringArray(items)) => items.iter().flatten().any(|text| holds(text, &mut terms)),
            _ => false,
        };
        if hit {
            out.push(d as DocId);
        }
    }
    Bitmap::of(&out)
}

// -- the distinct values ------------------------------------------------------------------------------

/// The documents of the values matching (for `Words`: of each term a value holding it, every term),
/// each value analyzed in `languages`, the languages its documents can have. None when a value
/// matches in some of them only: then which of its documents match depends on theirs.
fn value_docs(v: &ValueIndex, filter: &TextFilter, schema: &Schema, def: &FieldDef, languages: &[Language]) -> Option<Bitmap> {
    let words = filter.how == Match::Words;
    // per set the matching values' ordinals
    let mut sets: Vec<Vec<u64>> = vec![Vec::new(); if words { filter.terms.len() } else { 1 }];
    let mut terms = Terms::default();
    let mut folded = crate::text::Folded::default();
    let mut decided = true;
    v.for_each_str(|value, ord| {
        folded.read(value, schema, def);
        for (k, set) in sets.iter_mut().enumerate() {
            let mut hits = languages.iter().map(|&language| {
                terms.fold_in(&mut folded, language);
                let value_terms = terms.slices();
                if words { value_terms.iter().any(|t| filter.term_is(k, t)) } else { filter.holds(&value_terms) }
            });
            let hit = hits.next().unwrap_or(false);
            if hits.any(|h| h != hit) {
                decided = false;
                return false;
            }
            if hit {
                set.push(ord);
            }
        }
        true
    });
    if !decided {
        return None;
    }
    let mut out: Option<Bitmap> = None;
    for ords in sets {
        // one value: its postings as they are
        let bm = match ords[..] {
            [] => return Some(Bitmap::new()),
            [ord] => v.docs(ord),
            _ => {
                let mut set = DocSet::new(v.doc_count());
                ords.iter().for_each(|&o| v.add_to(o, &mut set));
                set.finish()
            }
        };
        out = Some(match out {
            None => bm,
            Some(acc) => &acc & &bm,
        });
    }
    Some(out.unwrap_or_default())
}

/// A value's terms as the field indexes them in a document of a language, in order.
struct Terms {
    bytes: Vec<u8>,
    ends: Vec<usize>,
    analyzer: crate::text::Analyzer,
}

impl Default for Terms {
    fn default() -> Self {
        Self { bytes: Vec::new(), ends: Vec::new(), analyzer: crate::text::Analyzer::remembering() }
    }
}

impl Terms {
    fn read(&mut self, value: &str, schema: &Schema, def: &FieldDef, language: Language) {
        self.bytes.clear();
        self.ends.clear();
        let Self { bytes, ends, analyzer } = self;
        crate::text::tokenize_each(value, &schema.symbols_to_index, &schema.token_separators, |token| {
            if let Some((term, _)) = analyzer.analyze(token, &schema.symbols_to_index, def, language) {
                bytes.extend_from_slice(term.as_bytes());
                ends.push(bytes.len());
            }
        });
    }

    /// The terms of a value read once (`Folded`) in `language`.
    fn fold_in(&mut self, folded: &mut crate::text::Folded, language: Language) {
        self.bytes.clear();
        self.ends.clear();
        let Self { bytes, ends, .. } = self;
        folded.each_in(language, |term| {
            bytes.extend_from_slice(term.as_bytes());
            ends.push(bytes.len());
        });
    }

    fn slices(&self) -> Vec<&[u8]> {
        let mut start = 0;
        self.ends
            .iter()
            .map(|&end| {
                let s = &self.bytes[start..end];
                start = end;
                s
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::filter::parser::parse_filter;
    use crate::filter::FilterOp;
    use crate::schema::FieldType;

    fn schema() -> Schema {
        let field = |name: &str, field_type, facet| FieldDef { name: name.into(), field_type, facet, index: true, ..Default::default() };
        let mut schema = Schema {
            name: "companies".into(),
            fields: vec![
                field("name", FieldType::String, false),
                field("form", FieldType::String, true),
                field("tags", FieldType::StringArray, true),
                field("reg", FieldType::String, false),
            ],
            ..Default::default()
        };
        schema.assign_field_ids();
        schema
    }

    fn segment(schema: &Schema) -> Segment {
        let rows: [(&str, &str, &[&str], &str); 5] = [
            ("Oskiftade dödsbon", "Aktiebolag", &["red shoe", "blue"], "556000-1234"),
            ("Dödsbon Oskiftade AB", "aktiebolag", &["blue shoe"], "5560001235"),
            ("Chris Parnell", "Handelsbolag", &["red"], "5560001236"),
            ("Chris Pine", "AKTIEBOLAG", &["shoe", "red", "shoe red"], "5560001237"),
            ("Hip hip hurray", "Enskild firma", &[], "5560001238"),
        ];
        let docs = rows
            .iter()
            .enumerate()
            .map(|(i, (name, form, tags, reg))| {
                let json = serde_json::json!({"id": i.to_string(), "name": name, "form": form, "tags": tags, "reg": reg});
                crate::ingest::parse_json_document(&json, schema, i as DocId).unwrap()
            })
            .collect();
        crate::ingest::build_index(schema, docs)
    }

    #[test]
    fn string_filters_match() {
        let schema = schema();
        let seg = segment(&schema);
        let found = |filter: &str| super::super::ops::execute_filter(&parse_filter(filter, &schema).unwrap().unwrap(), &seg, &schema).to_vec();
        let cases: &[(&str, &[u32])] = &[
            // a field without facet (no exact values) filters by its text index
            ("reg:=5560001235", &[1]),
            ("reg:5560001236", &[2]),
            ("reg:!=5560001235", &[0, 2, 3, 4]),
            ("reg:=[5560001235, 5560001237]", &[1, 3]),
            // by tokens: punctuation and case never matter
            ("reg:=556000-1234", &[0]),
            ("reg:=5560001234", &[0]),
            ("form:=aktiebolag", &[0, 1, 3]),
            ("form:=['Aktiebolag','Handelsbolag']", &[0, 1, 2, 3]),
            ("form:=Aktie", &[]),
            ("form:!=['Aktiebolag']", &[2, 4]),
            // `:=` the whole value, `:` every word anywhere, `"..."` one after the other
            ("name:=oskiftade", &[]),
            ("name:=Oskiftade dödsbon", &[0]),
            ("name:dödsbon oskiftade", &[0, 1]),
            ("name:\"dödsbon oskiftade\"", &[1]),
            ("name:\"oskiftade dödsbon\"", &[0]),
            ("name:!oskiftade", &[2, 3, 4]),
            ("name:=hip hip hurray", &[4]),
            ("name:=hip hurray", &[]),
            ("name:\"hip hurray\"", &[4]),
            // a trailing `*`: `:=` a value starting so, `:` the words anywhere
            ("name:=Chris P*", &[2, 3]),
            ("name:=Chris Pi*", &[3]),
            ("name:=P*", &[]),
            ("name:P*", &[2, 3]),
            ("name:=Osk*", &[0]),
            ("name:Osk*", &[0, 1]),
            ("name:\"chris p*\"", &[2, 3]),
            // arrays: `:=` one element, `:` the words anywhere in the document's elements
            ("tags:=red", &[2, 3]),
            ("tags:=red shoe", &[0]),
            ("tags:=shoe", &[3]),
            ("tags:shoe blue", &[0, 1]),
            ("tags:\"shoe blue\"", &[]),
            ("tags:\"blue shoe\"", &[1]),
            ("tags:!=red", &[0, 1, 4]),
            ("tags:=ALL[red, shoe]", &[3]),
            ("tags:=red*", &[0, 2, 3]),
        ];
        for (filter, want) in cases {
            assert_eq!(found(filter), *want, "{filter}");
        }
    }

    #[test]
    fn values_and_text_index_find_the_same_documents() {
        let schema = schema();
        let seg = segment(&schema);
        let filters = [
            "form:=aktiebolag", "form:=Enskild firma", "form:=enskild", "form:enskild", "form:=Aktie*", "form:\"enskild firma\"",
            "form:=E*", "tags:=red", "tags:=red shoe", "tags:shoe blue", "tags:\"blue shoe\"", "tags:=red*", "tags:=s*", "tags:red sh*",
            "tags:\"shoe r*\"", "tags:=shoe red",
        ];
        for filter in filters {
            let Some(FilterOp::Text(fid, f)) = parse_filter(filter, &schema).unwrap() else { panic!("{filter}") };
            let def = &schema.fields[fid as usize];
            let by_values = value_docs(&seg.values[&fid], &f, &schema, def, &[schema.default_language]).unwrap().to_vec();
            assert_eq!(index_docs(&seg.inverted[&fid], &f, &seg, &schema, def, None).to_vec(), by_values, "{filter}");
            assert_eq!(docs(fid, &f, &seg, &schema).to_vec(), by_values, "{filter}");
        }
    }

    #[test]
    fn a_documents_language_never_hides_it_from_a_filter() {
        use crate::schema::Language;
        // a stemmed field indexes each document in its language: "Toys" is "toy" in English, "toys"
        // in Swedish; "Electronics" "electron" and "electron" + "ic"
        let rows: [(Option<&str>, &str); 10] = [
            (Some("en"), "Toys"),
            (Some("sv"), "Toys"),
            (None, "Toys"),
            (Some("sv"), "Electronics"),
            (Some("en"), "Electronics"),
            (Some("sv"), "Red Toys"),
            (Some("en"), "red toys"),
            (Some("fi"), "Books"),
            (Some("sv"), "Toys toy"),
            (Some("en"), "Toys toy"),
        ];
        let field = |name: &str, facet| FieldDef { name: name.into(), field_type: FieldType::String, facet, stem: true, index: true, ..Default::default() };
        for default_language in [Language::English, Language::Swedish] {
            let mut schema = Schema { name: "c".into(), fields: vec![field("faceted", true), field("plain", false)], default_language, ..Default::default() };
            schema.assign_field_ids();
            // three copies of each row: more documents than values, so the values answer too
            let docs = rows
                .iter()
                .cycle()
                .take(3 * rows.len())
                .enumerate()
                .map(|(i, (language, value))| {
                    let mut json = serde_json::json!({"id": i.to_string(), "faceted": value, "plain": value});
                    if let Some(l) = language {
                        json["language"] = (*l).into();
                    }
                    let mut doc = crate::ingest::parse_json_document(&json, &schema, i as DocId).unwrap();
                    assert_eq!(doc.language, language.and_then(Language::from_locale).unwrap_or(default_language));
                    // typed TCP ingest has no record: the stored one is rendered, with the language
                    if i % 2 == 1 {
                        doc.record = None;
                    }
                    doc
                })
                .collect();
            let seg = crate::ingest::build_index(&schema, docs);
            let row = |d: DocId| d as usize % rows.len();
            for d in 0..3 * rows.len() as DocId {
                assert_eq!(seg.get_doc(d, &schema).unwrap().language, rows[row(d)].0.and_then(Language::from_locale).unwrap_or(default_language));
            }
            let cases: &[(&str, &[u32])] = &[
                ("=Toys", &[0, 1, 2]),
                ("Toys", &[0, 1, 2, 5, 6, 8, 9]),
                ("=Electronics", &[3, 4]),
                ("=[Toys, Books]", &[0, 1, 2, 7]),
                ("=Red Toys", &[5, 6]),
                ("\"red toys\"", &[5, 6]),
                ("=Toy*", &[0, 1, 2, 8, 9]),
                // the words of a Swedish document are Swedish: "toys" is not "toy" there
                ("\"toy toy\"", &[9]),
                ("=Toy", &[0, 2]),
            ];
            for (value, want) in cases {
                let mut want = want.to_vec();
                // a document without a language is in the collection's
                if default_language == Language::Swedish && *value == "=Toy" {
                    want.retain(|&d| d != 2);
                }
                let want: Vec<u32> = (0..3 * rows.len() as DocId).filter(|&d| want.contains(&(row(d) as u32))).collect();
                for name in ["faceted", "plain"] {
                    let filter = format!("{name}:{value}");
                    let op = parse_filter(&filter, &schema).unwrap().unwrap();
                    assert_eq!(super::super::ops::execute_filter(&op, &seg, &schema).to_vec(), want, "{filter} ({default_language:?})");
                    // the values answer only what does not depend on a document's language
                    let FilterOp::Text(fid, f) = &op else { continue };
                    let Some(v) = seg.values.get(fid) else { continue };
                    if let Some(by_values) = value_docs(v, f, &schema, &schema.fields[*fid as usize], &Language::ALL) {
                        assert_eq!(by_values.to_vec(), want, "{filter} by its values ({default_language:?})");
                    }
                }
            }
        }
    }
}
