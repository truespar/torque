pub mod ops;
pub mod parser;
pub mod text;

use crate::index::geo::Polygon;
use crate::schema::{FieldDef, FieldId, Language, Schema};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum FilterOp {
    Eq(FieldId, FilterValue),
    Neq(FieldId, FilterValue),
    In(FieldId, Vec<FilterValue>),
    AllOf(FieldId, Vec<FilterValue>),
    Gt(FieldId, FilterValue),
    Gte(FieldId, FilterValue),
    Lt(FieldId, FilterValue),
    Lte(FieldId, FilterValue),
    Range(FieldId, FilterValue, FilterValue),
    /// A filter on a string field (`field:=value`, `field: value`, `field: "a phrase"`): see
    /// `TextFilter`. `:!=` and `:!` are its `Not`, a list the `Or` of its values.
    Text(FieldId, TextFilter),
    /// field, lat, lng, radius in metres (the radius number read as a 32-bit float, times 1000 for
    /// km or 1609.34 for mi)
    GeoRadius(FieldId, f64, f64, f64),
    GeoBoundingBox(FieldId, f64, f64, f64, f64), // field, lat1, lng1, lat2, lng2
    /// Typesense's polygon: geodesic edges, the smaller of the two regions the loop bounds.
    GeoPolygon(FieldId, Arc<Polygon>),
    And(Vec<FilterOp>),
    Or(Vec<FilterOp>),
    Not(Box<FilterOp>),
    /// Element-level AND on a nested array. All equality conditions must match
    /// on the same array element index. E.g. `variants:(color:=Black && size:=M)`
    NestedAnd(Vec<(FieldId, FilterValue)>),
    /// JOIN filter: `$CollectionName(inner_filter_string)`.
    /// The inner filter is evaluated against the target collection's index,
    /// and matching target doc IDs are mapped back to local docs via the reverse reference index.
    JoinFilter {
        target_collection: String,
        inner_filter: String,
    },
    /// The documents with these ids (Typesense's `id` filter: `id:=a`, `id:[a, b]`; `id:!=a` is
    /// its `Not`, `id:*` the `Not` of none).
    Ids(Vec<String>),
}

impl FilterOp {
    /// Returns true if this filter tree contains any JoinFilter nodes.
    pub fn contains_join(&self) -> bool {
        match self {
            FilterOp::JoinFilter { .. } => true,
            FilterOp::And(ops) | FilterOp::Or(ops) => ops.iter().any(|o| o.contains_join()),
            FilterOp::Not(inner) => inner.contains_join(),
            _ => false,
        }
    }

    /// Returns true if the filter resolves entirely to pre-built bitmap lookups
    /// (no scanning required). These are cheap enough to always execute eagerly.
    pub fn has_prebuilt_bitmaps(&self) -> bool {
        match self {
            FilterOp::Eq(..) | FilterOp::In(..) | FilterOp::AllOf(..)
            | FilterOp::Neq(..) | FilterOp::Ids(..) => true,
            FilterOp::Text(_, t) => t.how == Match::Words,
            FilterOp::And(ops) | FilterOp::Or(ops) => ops.iter().all(|o| o.has_prebuilt_bitmaps()),
            FilterOp::Not(inner) => inner.has_prebuilt_bitmaps(),
            _ => false,
        }
    }
}

/// How a string filter's value must be in a document's field (Typesense's string comparators).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// `field:=value`: the field's value, or one of its array elements, is the value.
    Exact,
    /// `field: value`: every token of the value is in the field.
    Words,
    /// `field: "value"`: the value's tokens one after the other in the field's value or in one
    /// of its array elements.
    Phrase,
}

/// A filter on a string field as Typesense filters one: by the value's tokens, each the term the
/// field indexes for it (so case, accents and punctuation never matter, `'E'` is `E`), in the
/// field's value as `how` requires. A value ending in `*` makes its last token a prefix: `:=Chris P*`
/// is a value starting with "chris" and a token starting with "p", `: Chris P*` the two anywhere.
///
/// A stemmed field indexes each document's words in the document's language, so a word of the value
/// stands for its term in every language: `others` holds the forms that differ from the collection
/// language's. `category:=Toys` finds the Swedish documents (term "toys") with the English ("toy").
#[derive(Debug, Clone, PartialEq)]
pub struct TextFilter {
    /// The value's terms in the collection's language.
    pub terms: Vec<Vec<u8>>,
    /// Per term, the other languages' forms of its word (empty: none differ).
    pub others: Vec<Vec<Vec<u8>>>,
    pub how: Match,
    pub prefix: bool,
}

impl TextFilter {
    /// The filter of `value` on `field`; None when the value has no token.
    pub fn new(value: &str, how: Match, schema: &Schema, field: &FieldDef) -> Option<Self> {
        let prefix = value.len() > 1 && value.ends_with('*');
        let value = if prefix { &value[..value.len() - 1] } else { value };
        let mut folded = crate::text::Folded::default();
        folded.read(value, schema, field);
        let mut terms_in = |language| -> Vec<Vec<u8>> {
            let mut out = Vec::new();
            folded.each_in(language, |t| out.push(t.as_bytes().to_vec()));
            out
        };
        let terms = terms_in(schema.default_language);
        if terms.is_empty() {
            return None;
        }
        let mut others: Vec<Vec<Vec<u8>>> = Vec::new();
        if field.stem {
            for language in Language::ALL.into_iter().filter(|&l| l != schema.default_language) {
                let theirs = terms_in(language);
                // tokens never depend on the language: only the stems do
                debug_assert_eq!(theirs.len(), terms.len());
                for (k, t) in theirs.into_iter().enumerate().take(terms.len()) {
                    if t != terms[k] {
                        others.resize_with(terms.len(), Vec::new);
                        if !others[k].contains(&t) {
                            others[k].push(t);
                        }
                    }
                }
            }
        }
        // one token is a phrase of itself
        let how = if how == Match::Phrase && terms.len() == 1 { Match::Words } else { how };
        Some(Self { terms, others, how, prefix })
    }

    /// How many terms are matched whole (all but a prefix).
    pub fn fixed(&self) -> usize {
        self.terms.len() - usize::from(self.prefix)
    }

    /// The `k`-th term's forms: the collection language's, then the other languages'.
    pub fn forms(&self, k: usize) -> impl Iterator<Item = &[u8]> {
        std::iter::once(self.terms[k].as_slice()).chain(self.others.get(k).into_iter().flatten().map(Vec::as_slice))
    }

    /// Whether the filter's `k`-th term is `term` (the last one a prefix of it), in some language.
    #[inline]
    pub fn term_is(&self, k: usize, term: &[u8]) -> bool {
        let prefix = self.prefix && k + 1 == self.terms.len();
        self.forms(k).any(|form| if prefix { term.starts_with(form) } else { term == form })
    }

    /// Whether a value with these terms (in order) matches: for `Words` holds them all.
    pub fn holds(&self, value: &[&[u8]]) -> bool {
        let n = self.terms.len();
        match self.how {
            Match::Exact => {
                (if self.prefix { value.len() >= n } else { value.len() == n }) && (0..n).all(|k| self.term_is(k, value[k]))
            }
            Match::Phrase => value.len() >= n && (0..=value.len() - n).any(|p| (0..n).all(|k| self.term_is(k, value[p + k]))),
            Match::Words => (0..n).all(|k| value.iter().any(|t| self.term_is(k, t))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}
