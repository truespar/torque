mod normalizer;
mod stemmer;
mod tokenizer;

pub use normalizer::{lowercase_only, normalize, normalize_into};
pub use stemmer::stem;
pub use tokenizer::{tokenize, tokenize_each};

use crate::schema::{FieldDef, Language};
use std::collections::HashSet;

/// Full analysis pipeline: tokenize → normalize → stem.
/// Used during both indexing and query time for text fields.
pub fn analyze(
    text: &str,
    language: Language,
    symbols_to_index: &[char],
    token_separators: &[char],
) -> Vec<String> {
    tokenize(text, symbols_to_index, token_separators)
        .into_iter()
        .map(normalize)
        .filter(|t| !t.is_empty())
        .map(|t| stem(&t, language))
        .collect()
}

/// Query-time analysis with optional stopword stripping. Tokens are stemmed only when `stem`
/// is set (the queried fields are stemmed); stopwords are checked against the final form.
pub fn analyze_query(
    text: &str,
    language: Language,
    symbols_to_index: &[char],
    token_separators: &[char],
    stopwords: Option<&HashSet<String>>,
    stem: bool,
) -> Vec<String> {
    let mut tokens: Vec<String> = if stem {
        analyze(text, language, symbols_to_index, token_separators)
    } else {
        tokenize(text, symbols_to_index, token_separators)
            .into_iter()
            .map(normalize)
            .filter(|t| !t.is_empty())
            .collect()
    };
    if let Some(sw) = stopwords {
        tokens.retain(|t| !sw.contains(t));
    }
    tokens
}

/// What a field indexes for a token, with reusable buffers: the one analysis of the builders, the
/// query, filters, facet queries and highlighting.
#[derive(Default)]
pub struct Analyzer {
    lowered: String,
    term: String,
    remember: bool,
}

impl Analyzer {
    /// An analyzer whose stems are remembered (`stemmer::stem_remembered`): for values analyzed
    /// again and again, in every language.
    pub fn remembering() -> Self {
        Self { remember: true, ..Self::default() }
    }

    /// The (term, lower-cased token) of `token` in `field`; None when nothing of it is indexed.
    /// The term is folded, then stemmed by the field's dictionary or its stemmer. The lower-cased
    /// token keeps its accents, for prefix search.
    pub fn analyze(&mut self, token: &str, symbols: &[char], field: &FieldDef, language: Language) -> Option<(&str, &str)> {
        if self.fold(token, symbols, field)? {
            if self.remember {
                stemmer::stem_remembered(&mut self.term, language);
            } else {
                self.term = stem(&self.term, language);
            }
        }
        (!self.term.is_empty()).then_some((&self.term, &self.lowered))
    }

    /// `analyze` up to the stemmer, the same in every language: the token folded, or the field's
    /// dictionary root; whether the stemmer applies, None when nothing of the token is indexed.
    fn fold(&mut self, token: &str, symbols: &[char], field: &FieldDef) -> Option<bool> {
        normalize_into(token, symbols, &mut self.lowered, &mut self.term);
        if self.term.is_empty() {
            return None;
        }
        if let Some(root) = field.stem_dictionary.as_deref().and_then(|dict| crate::stemming::global().lookup(dict, &self.term)) {
            self.term = root;
            return Some(false);
        }
        Some(field.stem)
    }
}

/// A value's words for a field up to the stemmer (`Analyzer::fold`), read once for its terms in
/// many languages: the terms `Analyzer::analyze` gives in each, the stems remembered.
#[derive(Default)]
pub struct Folded {
    text: String,
    /// Per word its end in `text` and whether the stemmer applies.
    words: Vec<(usize, bool)>,
    analyzer: Analyzer,
    term: String,
}

impl Folded {
    pub fn read(&mut self, value: &str, schema: &crate::schema::Schema, field: &FieldDef) {
        self.text.clear();
        self.words.clear();
        let Self { text, words, analyzer, .. } = self;
        tokenize_each(value, &schema.symbols_to_index, &schema.token_separators, |t| {
            if let Some(stems) = analyzer.fold(t, &schema.symbols_to_index, field) {
                text.push_str(&analyzer.term);
                words.push((text.len(), stems));
            }
        });
    }

    /// The value's terms in `language`, in order.
    pub fn each_in(&mut self, language: Language, mut f: impl FnMut(&str)) {
        let mut start = 0;
        for &(end, stems) in &self.words {
            self.term.clear();
            self.term.push_str(&self.text[start..end]);
            start = end;
            if stems {
                stemmer::stem_remembered(&mut self.term, language);
            }
            if !self.term.is_empty() {
                f(&self.term);
            }
        }
    }
}

/// The terms a field indexes for `text`, in order.
pub fn field_terms(text: &str, schema: &crate::schema::Schema, field: &FieldDef, language: Language) -> Vec<String> {
    let mut analyzer = Analyzer::default();
    let mut out = Vec::new();
    tokenize_each(text, &schema.symbols_to_index, &schema.token_separators, |t| {
        if let Some((term, _)) = analyzer.analyze(t, &schema.symbols_to_index, field, language) {
            out.push(term.to_string());
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_pipeline_english() {
        let result = analyze("The Quick Brown FOX", Language::English, &[], &[]);
        // "the" → stem → "the", "quick" → "quick", "brown" → "brown", "fox" → "fox"
        assert_eq!(result, vec!["the", "quick", "brown", "fox"]);
    }

    #[test]
    fn pipeline_with_stopwords() {
        let mut stopwords = HashSet::new();
        stopwords.insert("the".to_string());

        let result =
            analyze_query("the brown fox", Language::English, &[], &[], Some(&stopwords), true);
        assert_eq!(result, vec!["brown", "fox"]);
    }

    #[test]
    fn pipeline_no_stopwords() {
        let result = analyze_query("the brown fox", Language::English, &[], &[], None, true);
        assert_eq!(result, vec!["the", "brown", "fox"]);
    }

    #[test]
    fn query_unstemmed_when_fields_are() {
        let result = analyze_query("Gaming Laptops", Language::English, &[], &[], None, false);
        assert_eq!(result, vec!["gaming", "laptops"]);
        let stemmed = analyze_query("Gaming Laptops", Language::English, &[], &[], None, true);
        assert_eq!(stemmed, vec!["game", "laptop"]);
    }

    #[test]
    fn pipeline_empty_string() {
        let result = analyze("", Language::English, &[], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn pipeline_whitespace_only() {
        let result = analyze("   \t\n  ", Language::English, &[], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn pipeline_consistency() {
        let a = analyze("Running cats", Language::English, &[], &[]);
        let b = analyze("Running cats", Language::English, &[], &[]);
        assert_eq!(a, b);
    }

    #[test]
    fn pipeline_unicode_normalized() {
        let result = analyze("Café Ström", Language::English, &[], &[]);
        assert_eq!(result, vec!["cafe", "strom"]);
    }

    #[test]
    fn pipeline_swedish() {
        let result = analyze("bilar husens", Language::Swedish, &[], &[]);
        assert_eq!(result, vec!["bil", "hus"]);
    }

    #[test]
    fn analyzer_gives_the_term_and_the_lowered_token() {
        let mut a = Analyzer::default();
        let plain = FieldDef { name: "f".into(), ..Default::default() };
        let stemmed = FieldDef { name: "f".into(), stem: true, ..Default::default() };
        assert_eq!(a.analyze("Trädgård", &[], &plain, Language::Swedish), Some(("tradgard", "trädgård")));
        assert_eq!(a.analyze("Bilar", &[], &stemmed, Language::Swedish), Some(("bil", "bilar")));
        assert_eq!(a.analyze("---", &[], &plain, Language::Swedish), None);
    }
}
