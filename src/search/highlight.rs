//! Highlighting: a hit's string fields with the words the search matched marked, whole values or
//! snippets. Works from the stored text: a field's tokens are analyzed as the field indexes them,
//! so a stem, a folded word or a typo correction marks the word as written.

use crate::schema::{FieldDef, Language};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use serde_json::{json, Map, Value};

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
/// A field of a hit's `highlights`.
#[derive(Debug, Clone, Serialize)]
pub struct Highlight {
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indices: Option<Vec<usize>>,
    pub matched_tokens: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippets: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
}

/// How much of a matching word is marked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Whole,
    /// A completed prefix: its first characters.
    Prefix { chars: usize },
}

/// A field to highlight.
pub struct Field<'s> {
    pub def: &'s FieldDef,
    /// Listed in `highlight_full_fields`: the whole marked value too.
    pub full: bool,
}

pub struct Highlighter<'s> {
    pub fields: Vec<Field<'s>>,
    /// The terms the search matched.
    pub terms: &'s FxHashMap<Vec<u8>, Mark>,
    /// The query's phrases, as terms: marked where they occur in order.
    pub phrases: &'s [Vec<Vec<u8>>],
    pub symbols_to_index: &'s [char],
    pub token_separators: &'s [char],
    pub language: Language,
    pub start_tag: &'s str,
    pub end_tag: &'s str,
    pub affix_num_tokens: usize,
    pub snippet_threshold: usize,
}

/// A value's marks: byte ranges of the text, ascending and disjoint, and the tokens they cover.
struct Marked<'t> {
    tokens: Vec<&'t str>,
    /// (token index, byte range in the text).
    ranges: Vec<(usize, usize, usize)>,
}

impl<'s> Highlighter<'s> {
    /// Find the marks of `text`: `lookup` gives a term's mark.
    fn marks<'t>(&self, text: &'t str, def: &FieldDef, lookup: &dyn Fn(&[u8]) -> Option<Mark>, plausible: &[&[u8]]) -> Marked<'t> {
        let tokens = crate::text::tokenize(text, self.symbols_to_index, self.token_separators);
        let base = text.as_ptr() as usize;
        let mut analyzer = crate::text::Analyzer::default();
        let (mut lowered, mut folded) = (String::new(), String::new());
        // Stemmers change the end of a word: a token that starts unlike every term cannot match,
        // and stemming every token of a long field costs more than the rest of the search.
        let quick = def.stem && def.stem_dictionary.is_none() && !plausible.is_empty();
        let phrases = !self.phrases.is_empty();
        let mut terms: Vec<Vec<u8>> = Vec::new();
        let mut marks: Vec<Option<Mark>> = Vec::with_capacity(tokens.len());
        for token in &tokens {
            if quick {
                crate::text::normalize_into(token, self.symbols_to_index, &mut lowered, &mut folded);
                if !phrases && !plausible.iter().any(|p| folded.as_bytes().starts_with(p)) {
                    marks.push(None);
                    continue;
                }
            }
            let term = analyzer.analyze(token, self.symbols_to_index, def, self.language).map(|t| t.0);
            marks.push(term.and_then(|t| lookup(t.as_bytes())));
            if phrases {
                terms.push(term.unwrap_or("").as_bytes().to_vec());
            }
        }
        for phrase in self.phrases {
            if phrase.is_empty() || phrase.len() > terms.len() {
                continue;
            }
            for at in 0..=terms.len() - phrase.len() {
                if terms[at..at + phrase.len()] == phrase[..] {
                    marks[at..at + phrase.len()].fill(Some(Mark::Whole));
                }
            }
        }
        let mut ranges = Vec::new();
        for (i, (token, mark)) in tokens.iter().zip(&marks).enumerate() {
            let Some(mark) = mark else { continue };
            let start = token.as_ptr() as usize - base;
            let len = match *mark {
                Mark::Whole => token.len(),
                Mark::Prefix { chars } => token.char_indices().nth(chars).map_or(token.len(), |(at, _)| at),
            };
            ranges.push((i, start, start + len));
        }
        Marked { tokens, ranges }
    }

    fn tagged(&self, text: &str, ranges: impl Iterator<Item = (usize, usize)>) -> String {
        let mut out = String::with_capacity(text.len() + 32);
        let mut pos = 0;
        for (start, end) in ranges {
            out.push_str(&text[pos..start]);
            out.push_str(self.start_tag);
            out.push_str(&text[start..end]);
            out.push_str(self.end_tag);
            pos = end;
        }
        out.push_str(&text[pos..]);
        out
    }

    /// `value` with every word `term_mark` finds marked: a whole value, for facet values.
    pub fn mark_with(&self, value: &str, def: &FieldDef, term_mark: &dyn Fn(&[u8]) -> Option<Mark>) -> String {
        let m = self.marks(value, def, term_mark, &[]);
        self.tagged(value, m.ranges.iter().map(|r| (r.1, r.2)))
    }

    /// The snippet: the whole marked value below the threshold, else the first match with
    /// `affix_num_tokens` tokens on each side.
    fn snippet(&self, text: &str, m: &Marked<'_>) -> String {
        let all = || m.ranges.iter().map(|r| (r.1, r.2));
        if m.tokens.len() < self.snippet_threshold {
            return self.tagged(text, all());
        }
        let first = m.ranges[0].0;
        let (from, to) = (first.saturating_sub(self.affix_num_tokens), (first + self.affix_num_tokens + 1).min(m.tokens.len()));
        let base = text.as_ptr() as usize;
        let at = |i: usize| m.tokens[i].as_ptr() as usize - base;
        let (start, end) = (at(from), at(to - 1) + m.tokens[to - 1].len());
        let inside = m.ranges.iter().filter(|r| r.0 >= from && r.0 < to).map(|r| (r.1 - start, r.2 - start));
        let mut out = String::new();
        if from > 0 {
            out.push_str("...");
        }
        out.push_str(&self.tagged(&text[start..end], inside));
        if to < m.tokens.len() {
            out.push_str("...");
        }
        out
    }

    /// The marked words, each once, in order.
    fn matched(text: &str, m: &Marked<'_>) -> Vec<String> {
        let mut seen = FxHashSet::default();
        m.ranges.iter().map(|r| &text[r.1..r.2]).filter(|t| seen.insert(*t)).map(str::to_string).collect()
    }

    /// A hit's highlights: the `highlight` object, which mirrors the document, and `highlights`,
    /// Typesense's first format: top-level fields only.
    pub fn highlight(&self, document: &Value) -> (Value, Vec<Highlight>) {
        let mut object = Map::new();
        let mut list = Vec::new();
        let plausible: Vec<&[u8]> = self.terms.keys().map(|t| &t[..t.len().min(3)]).collect();
        let lookup = |t: &[u8]| self.terms.get(t).copied();
        for field in &self.fields {
            let path: Vec<&str> = field.def.name.split('.').collect();
            let mut texts: Vec<&str> = Vec::new();
            let mut array = false;
            // `highlights` lists top-level fields only, as Typesense's first format does (a flattened
            // name the document carries as such is one); a field below an object is in the `highlight`
            // object alone.
            let top_level = match document.get(&field.def.name) {
                Some(v) => {
                    collect(v, &[], &mut texts, &mut array);
                    true
                }
                None => {
                    collect(document, &path, &mut texts, &mut array);
                    path.len() == 1
                }
            };
            let marked: Vec<(usize, &str, Marked<'_>)> = texts
                .iter()
                .enumerate()
                .map(|(i, text)| (i, *text, self.marks(text, field.def, &lookup, &plausible)))
                .filter(|m| !m.2.ranges.is_empty())
                .collect();
            if marked.is_empty() {
                continue;
            }
            if array {
                let snippets: Vec<String> = marked.iter().map(|(_, t, m)| self.snippet(t, m)).collect();
                let tokens: Vec<Vec<String>> = marked.iter().map(|(_, t, m)| Self::matched(t, m)).collect();
                let values: Option<Vec<String>> = field.full.then(|| marked.iter().map(|(_, t, m)| self.tagged(t, m.ranges.iter().map(|r| (r.1, r.2)))).collect());
                let mut elements: Vec<Value> = texts.iter().map(|t| json!({"matched_tokens": [], "snippet": t})).collect();
                for (k, (i, _, _)) in marked.iter().enumerate() {
                    let mut e = json!({"matched_tokens": tokens[k], "snippet": snippets[k]});
                    if let Some(v) = &values {
                        e["value"] = json!(v[k]);
                    }
                    elements[*i] = e;
                }
                insert(&mut object, &path, Value::Array(elements));
                if top_level {
                    list.push(Highlight {
                        field: field.def.name.clone(),
                        snippet: None,
                        value: None,
                        snippets: Some(snippets),
                        values,
                        indices: Some(marked.iter().map(|m| m.0).collect()),
                        matched_tokens: json!(tokens),
                    });
                }
            } else {
                let (_, text, m) = &marked[0];
                let snippet = self.snippet(text, m);
                let tokens = Self::matched(text, m);
                let value = field.full.then(|| self.tagged(text, m.ranges.iter().map(|r| (r.1, r.2))));
                let mut e = json!({"matched_tokens": tokens, "snippet": snippet});
                if let Some(v) = &value {
                    e["value"] = json!(v);
                }
                insert(&mut object, &path, e);
                if top_level {
                    list.push(Highlight {
                        field: field.def.name.clone(),
                        snippet: Some(snippet),
                        value,
                        snippets: None,
                        values: None,
                        indices: None,
                        matched_tokens: json!(tokens),
                    });
                }
            }
        }
        (Value::Object(object), list)
    }
}

/// The strings at `path` under `node`, through arrays of objects; `array`: one was an array.
fn collect<'d>(node: &'d Value, path: &[&str], out: &mut Vec<&'d str>, array: &mut bool) {
    match node {
        Value::Array(items) => {
            *array = true;
            items.iter().for_each(|item| collect(item, path, out, array));
        }
        Value::Object(fields) => {
            if let Some((first, rest)) = path.split_first()
                && let Some(child) = fields.get(*first)
            {
                collect(child, rest, out, array);
            }
        }
        Value::String(s) if path.is_empty() => out.push(s),
        _ => {}
    }
}

/// Put `value` at `path`, making the objects above it.
fn insert(object: &mut Map<String, Value>, path: &[&str], value: Value) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut node = object;
    for key in parents {
        let entry = node.entry(key.to_string()).or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        node = entry.as_object_mut().expect("just made an object");
    }
    node.insert(last.to_string(), value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(stem: bool) -> FieldDef {
        FieldDef { name: "title".into(), stem, ..Default::default() }
    }

    fn terms(list: &[(&str, Mark)]) -> FxHashMap<Vec<u8>, Mark> {
        list.iter().map(|(t, m)| (t.as_bytes().to_vec(), *m)).collect()
    }

    fn highlighter<'s>(fields: Vec<Field<'s>>, t: &'s FxHashMap<Vec<u8>, Mark>, phrases: &'s [Vec<Vec<u8>>]) -> Highlighter<'s> {
        Highlighter {
            fields,
            terms: t,
            phrases,
            symbols_to_index: &[],
            token_separators: &[],
            language: Language::English,
            start_tag: "<mark>",
            end_tag: "</mark>",
            affix_num_tokens: 4,
            snippet_threshold: 30,
        }
    }

    fn snippet(text: &str, d: &FieldDef, t: &FxHashMap<Vec<u8>, Mark>) -> Option<String> {
        let h = highlighter(vec![Field { def: d, full: false }], t, &[]);
        let (_, list) = h.highlight(&json!({"title": text}));
        list.first().and_then(|l| l.snippet.clone())
    }

    #[test]
    fn marks_words_as_written() {
        let d = def(false);
        let t = terms(&[("quick", Mark::Whole), ("fox", Mark::Whole)]);
        assert_eq!(snippet("The Quick brown fox", &d, &t).as_deref(), Some("The <mark>Quick</mark> brown <mark>fox</mark>"));
        assert_eq!(snippet("nothing here", &d, &t), None);
        assert_eq!(snippet("", &d, &t), None);
    }

    #[test]
    fn marks_stems_and_folded_words() {
        let t = terms(&[("run", Mark::Whole)]);
        assert_eq!(snippet("The running fox", &def(true), &t).as_deref(), Some("The <mark>running</mark> fox"));
        let t = terms(&[("cafe", Mark::Whole)]);
        assert_eq!(snippet("Café Ström", &def(false), &t).as_deref(), Some("<mark>Café</mark> Ström"));
    }

    #[test]
    fn a_completed_prefix_marks_what_was_typed() {
        let t = terms(&[("shoes", Mark::Prefix { chars: 3 })]);
        assert_eq!(snippet("Running Shoes", &def(false), &t).as_deref(), Some("Running <mark>Sho</mark>es"));
    }

    #[test]
    fn matched_tokens_are_listed_once() {
        let d = def(false);
        let t = terms(&[("red", Mark::Whole)]);
        let h = highlighter(vec![Field { def: &d, full: false }], &t, &[]);
        let (_, list) = h.highlight(&json!({"title": "red and blue and red"}));
        assert_eq!(list[0].snippet.as_deref(), Some("<mark>red</mark> and blue and <mark>red</mark>"));
        assert_eq!(list[0].matched_tokens, json!(["red"]));
        assert!(list[0].value.is_none(), "value requires highlight_full_fields");
    }

    #[test]
    fn long_values_are_cut_around_the_first_match() {
        let d = def(false);
        let t = terms(&[("needle", Mark::Whole)]);
        let mut h = highlighter(vec![Field { def: &d, full: true }], &t, &[]);
        h.snippet_threshold = 5;
        h.affix_num_tokens = 1;
        let (_, list) = h.highlight(&json!({"title": "a b c d needle e f g h"}));
        assert_eq!(list[0].snippet.as_deref(), Some("...d <mark>needle</mark> e..."));
        assert_eq!(list[0].value.as_deref(), Some("a b c d <mark>needle</mark> e f g h"));
    }

    #[test]
    fn phrases_mark_their_words_in_order_only() {
        let d = def(false);
        let t = FxHashMap::default();
        let phrases = vec![vec![b"new".to_vec(), b"york".to_vec()]];
        let h = highlighter(vec![Field { def: &d, full: false }], &t, &phrases);
        let (_, list) = h.highlight(&json!({"title": "york is new but New York is old"}));
        assert_eq!(list[0].snippet.as_deref(), Some("york is new but <mark>New</mark> <mark>York</mark> is old"));
    }

    #[test]
    fn arrays_give_every_element_and_the_matching_ones() {
        let d = def(false);
        let t = terms(&[("quick", Mark::Whole)]);
        let h = highlighter(vec![Field { def: &d, full: false }], &t, &[]);
        let (object, list) = h.highlight(&json!({"title": ["The quick fox", "A lazy dog", "quick cat"]}));
        assert_eq!(list[0].indices.as_deref(), Some(&[0, 2][..]));
        assert_eq!(list[0].snippets.as_ref().unwrap()[1], "<mark>quick</mark> cat");
        assert!(list[0].snippet.is_none() && list[0].values.is_none());
        assert_eq!(object["title"][1], json!({"matched_tokens": [], "snippet": "A lazy dog"}));
        assert_eq!(object["title"][2]["matched_tokens"], json!(["quick"]));
    }

    #[test]
    fn nested_fields_mirror_the_document_and_stay_out_of_the_list() {
        let d = FieldDef { name: "addr.city".into(), ..Default::default() };
        let t = terms(&[("lund", Mark::Whole)]);
        let h = highlighter(vec![Field { def: &d, full: false }], &t, &[]);
        let (object, list) = h.highlight(&json!({"addr": {"city": "Lund"}}));
        assert_eq!(object["addr"]["city"]["snippet"], "<mark>Lund</mark>");
        assert!(list.is_empty(), "highlights lists top-level fields only, as Typesense's first format does");
        // a field of an array of objects: every element in the object, nothing in the list
        let d = FieldDef { name: "names.nameOrIdentifier".into(), ..Default::default() };
        let t = terms(&[("volvo", Mark::Whole)]);
        let h = highlighter(vec![Field { def: &d, full: false }], &t, &[]);
        let names = ["Volvo Personvagnar AB", "Volvo Car Corporation", "Other"];
        let doc = json!({"names": names.iter().map(|n| json!({"nameOrIdentifier": n})).collect::<Vec<_>>()});
        let (object, list) = h.highlight(&doc);
        let elements = &object["names"]["nameOrIdentifier"];
        assert_eq!(elements[0]["matched_tokens"], json!(["Volvo"]));
        assert_eq!(elements[2], json!({"matched_tokens": [], "snippet": "Other"}));
        assert!(list.is_empty());
        // the flattened name the document carries as such is a top-level field: listed, in the element form
        let (_, list) = h.highlight(&json!({"names.nameOrIdentifier": names}));
        assert_eq!(list[0].field, "names.nameOrIdentifier");
        assert_eq!(list[0].indices.as_deref(), Some(&[0, 1][..]));
        assert_eq!(list[0].matched_tokens, json!([["Volvo"], ["Volvo"]]));
    }
}
