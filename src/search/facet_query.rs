//! facet_query: the facet values holding the query's words, the last one as a prefix
//! (`facet_query=category:shoe` keeps "Running Shoes"), each word within `facet_query_num_typos`
//! typos (default 2; a word of 3 characters or more may have one, from 7 two). Typos widen the
//! match only where nothing matches without: the values kept match with the fewest typos by which
//! any result of the search matches (a value's typos are the most any of the words needs). The
//! matched words are marked in `highlighted`. A number or bool matches when its text starts with
//! the query.

use crate::bitmap::Bitmap;
use crate::index::facet::{FacetBy, ValueQuery};
use crate::index::Index;
use crate::schema::{FieldDef, FieldId, FieldType, Language, Schema};
use crate::search::highlight::{Highlighter, Mark};
use levenshtein_automata::{Distance, DFA};

/// A word of the query, as the field indexes its words.
struct Word {
    term: Vec<u8>,
    /// The last word, matched as a prefix.
    prefix: bool,
    /// Its characters: the part of a completion marked.
    chars: usize,
    /// The typos it may have.
    max: u8,
    dfa: Option<DFA>,
}

impl Word {
    /// The typos by which `term` is this word; None when too many.
    fn typos(&self, term: &[u8]) -> Option<u32> {
        if term == self.term || (self.prefix && term.starts_with(&self.term)) {
            return Some(0);
        }
        let dfa = self.dfa.as_ref()?;
        match dfa.eval(term) {
            Distance::Exact(d) if d <= self.max => Some(u32::from(d)),
            _ => None,
        }
    }
}

pub struct FacetQuery<'s> {
    pub fid: FieldId,
    def: &'s FieldDef,
    language: Language,
    schema: &'s Schema,
    words: Vec<Word>,
    /// A number or bool field: the start of its values' text.
    text: Option<String>,
    start_tag: &'s str,
    end_tag: &'s str,
    no_terms: rustc_hash::FxHashMap<Vec<u8>, Mark>,
}

impl<'s> FacetQuery<'s> {
    /// `facet_query` (`field: value`) on the facets of `facet_by`; None for an empty value.
    pub fn parse(fq: &str, facet_by: &FacetBy, schema: &'s Schema, num_typos: u8, start_tag: &'s str, end_tag: &'s str) -> Result<Option<Self>, String> {
        let (name, value) = fq.split_once(':').ok_or("Facet query must be in the `facet_field: value` format.")?;
        if facet_by.local.is_empty() && facet_by.joins.is_empty() {
            return Err("The `facet_query` parameter is supplied without a `facet_by` parameter.".into());
        }
        let (name, value) = (name.trim(), value.trim());
        if value.is_empty() {
            return Ok(None);
        }
        if !facet_by.local.iter().any(|f| f.name == name) {
            return Err(format!("Facet query refers to a facet field `{name}` that is not part of `facet_by` parameter."));
        }
        let fid = schema
            .name_to_id
            .get(name)
            .copied()
            .filter(|&f| schema.fields[f as usize].facet)
            .ok_or_else(|| format!("Could not find a facet field named `{name}` in the schema."))?;
        let def = &schema.fields[fid as usize];
        let string = matches!(def.field_type, FieldType::String | FieldType::Text | FieldType::StringArray | FieldType::StringStar);
        let language = schema.default_language;
        let words = if string {
            let raw = crate::text::field_terms(value, schema, def, language);
            let n = raw.len();
            raw.into_iter()
                .enumerate()
                .map(|(i, term)| {
                    let chars = term.chars().count();
                    let max = match chars {
                        7.. => num_typos.min(2),
                        3.. => num_typos.min(1),
                        _ => 0,
                    };
                    let prefix = i + 1 == n;
                    let dfa = (max > 0).then(|| if prefix { crate::index::inverted::prefix_typo_dfa(&term, max) } else { crate::index::inverted::typo_dfa(&term, max) });
                    Word { term: term.into_bytes(), prefix, chars, max, dfa }
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Some(FacetQuery {
            fid,
            def,
            language,
            schema,
            words,
            text: (!string).then(|| value.to_lowercase()),
            start_tag,
            end_tag,
            no_terms: Default::default(),
        }))
    }

    /// The documents of a segment holding every word of the query (within its typos) in the
    /// field, which hold every value it matches; None for a number or bool field, or a segment
    /// without words of the field.
    pub fn docs(&self, index: &Index) -> Option<Bitmap> {
        if self.text.is_some() {
            return None;
        }
        let inv = index.inverted.get(&self.fid)?;
        let mut all: Option<Bitmap> = None;
        for w in &self.words {
            let mut postings = Vec::new();
            if w.prefix {
                inv.for_each_with_prefix(&w.term, |r| {
                    postings.push(inv.postings_of(r));
                    true
                });
            } else {
                postings.extend(inv.postings(&w.term));
            }
            if let Some(dfa) = &w.dfa {
                postings.extend(inv.fuzzy(dfa).into_iter().map(|(_, _, r)| inv.postings_of(r)));
            }
            let docs = Bitmap::union_many(postings.iter());
            let docs = match all {
                Some(a) => &a & &docs,
                None => docs,
            };
            let empty = docs.is_empty();
            all = Some(docs);
            if empty {
                break;
            }
        }
        Some(all.unwrap_or_default())
    }

    /// A value's words as the field indexes them.
    fn terms(&self, value: &str) -> Vec<Vec<u8>> {
        crate::text::field_terms(value, self.schema, self.def, self.language).into_iter().map(String::into_bytes).collect()
    }

    fn marker(&self) -> Highlighter<'_> {
        Highlighter {
            fields: Vec::new(),
            terms: &self.no_terms,
            phrases: &[],
            symbols_to_index: &self.schema.symbols_to_index,
            token_separators: &self.schema.token_separators,
            language: self.language,
            start_tag: self.start_tag,
            end_tag: self.end_tag,
            affix_num_tokens: 0,
            snippet_threshold: usize::MAX,
        }
    }
}

impl ValueQuery for FacetQuery<'_> {
    fn cost(&self, value: &str) -> Option<u32> {
        if let Some(text) = &self.text {
            return value.to_lowercase().starts_with(text.as_str()).then_some(0);
        }
        if self.words.is_empty() {
            return None;
        }
        let terms = self.terms(value);
        self.words.iter().map(|w| terms.iter().filter_map(|t| w.typos(t)).min()).try_fold(0, |worst, typos| Some(worst.max(typos?)))
    }

    fn highlight(&self, value: &str, level: u32) -> String {
        if let Some(text) = &self.text {
            let n = text.len().min(value.len());
            return match (value.get(..n), value.get(n..)) {
                (Some(head), Some(tail)) if n > 0 => format!("{}{head}{}{tail}", self.start_tag, self.end_tag),
                _ => value.to_string(),
            };
        }
        self.marker().mark_with(value, self.def, &|term| {
            self.words.iter().find_map(|w| {
                let typos = w.typos(term).filter(|&t| t <= level)?;
                Some(if w.prefix && typos == 0 { Mark::Prefix { chars: w.chars } } else { Mark::Whole })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::facet::parse_facet_by;

    fn schema() -> Schema {
        let fields = vec![
            crate::schema::FieldDef { name: "category".into(), field_type: FieldType::String, facet: true, index: true, id: 0, ..Default::default() },
            crate::schema::FieldDef { name: "year".into(), field_type: FieldType::Int32, facet: true, index: true, id: 1, ..Default::default() },
        ];
        let mut s = Schema { name: "t".into(), fields, ..Default::default() };
        s.name_to_id = s.fields.iter().map(|f| (f.name.clone(), f.id)).collect();
        s
    }

    fn query<'s>(schema: &'s Schema, fq: &str) -> FacetQuery<'s> {
        let by = parse_facet_by("category,year", schema).unwrap();
        FacetQuery::parse(fq, &by, schema, 2, "<mark>", "</mark>").unwrap().unwrap()
    }

    #[test]
    fn words_anywhere_the_last_a_prefix_with_typos() {
        let s = schema();
        let q = query(&s, "category: shoe");
        assert_eq!(q.cost("Running Shoes"), Some(0));
        assert_eq!(q.cost("Shoe horn"), Some(0));
        assert_eq!(q.cost("Shirts"), None);
        assert_eq!(q.highlight("Running Shoes", 0), "Running <mark>Shoe</mark>s");
        let q = query(&s, "category: runing sho");
        assert_eq!(q.cost("Running Shoes"), Some(1));
        assert_eq!(q.cost("Shoes for running"), Some(1));
        assert_eq!(q.highlight("Running Shoes", 1), "<mark>Running</mark> <mark>Sho</mark>es");
        // a word under 3 characters has no typos
        let q = query(&s, "category: xo");
        assert_eq!(q.cost("Xbox"), None);
        assert_eq!(q.cost("Xoo"), Some(0));
    }

    #[test]
    fn numbers_by_their_text_and_messages() {
        let s = schema();
        let q = query(&s, "year: 20");
        assert_eq!((q.cost("2019"), q.cost("1920")), (Some(0), None));
        assert_eq!(q.highlight("2019", 0), "<mark>20</mark>19");
        let by = parse_facet_by("category", &s).unwrap();
        let err = |fq: &str, by: &FacetBy| FacetQuery::parse(fq, by, &s, 2, "", "").err();
        assert_eq!(err("category", &by).as_deref(), Some("Facet query must be in the `facet_field: value` format."));
        assert_eq!(err("year: 2", &by).as_deref(), Some("Facet query refers to a facet field `year` that is not part of `facet_by` parameter."));
        assert_eq!(err("year: 2", &FacetBy::default()).as_deref(), Some("The `facet_query` parameter is supplied without a `facet_by` parameter."));
        assert!(FacetQuery::parse("category:  ", &by, &s, 2, "", "").unwrap().is_none());
    }
}
