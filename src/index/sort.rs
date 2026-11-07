//! Sorting by a field reads its value index (TQBF spec §7.3): strings and numbers through their
//! per-document ordinals, which are ranks in value order, bools through their BOOL bitmaps (false
//! before true). Walking the values in order with their postings yields documents in sorted order,
//! so no per-field sorted doc list is stored.
//!
//! The order: by value in the requested direction, ties with the highest doc id (latest inserted)
//! first in both directions. A document without a value goes last, numbers and bools in both
//! directions and strings ascending; descending, a string without a value goes first.
//! `missing_values: first|last` overrides that.

use crate::bitmap::Bitmap;
use crate::index::values::{BoolIndex, ValueIndex};
use crate::index::Segment;
use crate::schema::{DocId, FieldId};
use std::borrow::Cow;

/// Where documents without a value go (Typesense's `missing_values`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Missing {
    /// Last, but first for strings sorted descending.
    #[default]
    Normal,
    First,
    Last,
}

#[derive(Clone, Copy)]
pub enum SortColumn<'a> {
    Values(&'a ValueIndex),
    Bool(&'a BoolIndex),
}

impl<'a> SortColumn<'a> {
    /// The sort column of a field: single-valued ordinals or a bool field.
    pub fn of(seg: &'a Segment, field: FieldId) -> Option<Self> {
        if let Some(v) = seg.values.get(&field) {
            if v.doc_ords().is_some_and(|o| !o.is_multi()) {
                return Some(Self::Values(v));
            }
        }
        seg.bools.get(&field).map(Self::Bool)
    }

    /// Number of distinct values.
    #[inline]
    fn ranks(&self) -> u64 {
        match self {
            Self::Values(v) => v.len(),
            Self::Bool(_) => 2,
        }
    }

    /// Rank of the document's value in ascending order.
    #[inline]
    fn rank(&self, doc: DocId) -> Option<u64> {
        match self {
            Self::Values(v) => v.doc_ord(doc),
            Self::Bool(b) => b.value(doc).map(u64::from),
        }
    }

    /// Whether documents without a value come first.
    pub fn missing_first(&self, descending: bool, missing: Missing) -> bool {
        match missing {
            Missing::First => true,
            Missing::Last => false,
            Missing::Normal => descending && matches!(self, Self::Values(v) if v.is_str()),
        }
    }

    /// Place of the document's value in the sort order: equal values, equal places.
    #[inline]
    pub fn order(&self, doc: DocId, descending: bool, missing_first: bool) -> u64 {
        match self.rank(doc) {
            Some(r) => (if descending { self.ranks() - 1 - r } else { r }) + u64::from(missing_first),
            None if missing_first => 0,
            None => self.ranks(),
        }
    }

    /// Ascending sort key: `order`, then the highest doc id first.
    #[inline]
    pub fn key(&self, doc: DocId, descending: bool, missing_first: bool) -> u64 {
        self.order(doc, descending, missing_first) << 32 | u64::from(u32::MAX - doc)
    }

    /// The documents with a value.
    fn present(&self) -> Cow<'a, Bitmap> {
        match self {
            Self::Values(v) => match v.doc_ords() {
                Some(o) => Cow::Borrowed(o.present_docs()),
                None => Cow::Owned(Bitmap::new()),
            },
            Self::Bool(b) => Cow::Owned(&b.true_docs | &b.false_docs),
        }
    }

    /// Documents of rank `r` (below `ranks()`).
    fn docs_of_rank(&self, r: u64) -> Bitmap {
        match self {
            Self::Values(v) => v.docs(r),
            Self::Bool(b) => b.docs(r == 1).clone(),
        }
    }

    /// The first `needed` candidates in `key` order: the values walked in order, each value's
    /// postings intersected with the candidates (highest doc id first), the candidates without a
    /// value before or after them.
    pub fn presorted(&self, candidates: &Bitmap, descending: bool, missing_first: bool, needed: usize) -> Vec<DocId> {
        self.presorted_within(candidates, descending, missing_first, needed, u64::MAX).unwrap_or_default()
    }

    /// `presorted`, None once the values walked hold more than `budget` documents (the
    /// candidates are then cheaper to sort by key: a filter on the sort field leaves the first
    /// values without any).
    pub fn presorted_within(&self, candidates: &Bitmap, descending: bool, missing_first: bool, needed: usize, budget: u64) -> Option<Vec<DocId>> {
        let mut out = Vec::with_capacity(needed.min(1024));
        // The candidates without a value, highest first: a lazy backward scan when they are not
        // rare, else the difference with the documents with a value.
        let missing = |out: &mut Vec<DocId>| {
            let take = needed - out.len();
            let present = self.present();
            let n = candidates.cardinality();
            let n_missing = n - candidates.and_cardinality(&present);
            if take == 0 || n_missing == 0 {
                return;
            }
            if n_missing * 64 >= n {
                let start = out.len();
                candidates.for_each_rev(|d| {
                    if !present.contains(d) {
                        out.push(d);
                    }
                    out.len() - start < take
                });
            } else {
                out.extend((candidates - &*present).top_desc(take));
            }
        };
        if missing_first {
            missing(&mut out);
        }
        let n = self.ranks();
        let mut walked = 0u64;
        for i in 0..n {
            if out.len() >= needed {
                break;
            }
            let take = needed - out.len();
            let docs = self.docs_of_rank(if descending { n - 1 - i } else { i });
            walked += docs.cardinality();
            if walked > budget {
                return None;
            }
            if docs.cardinality() <= 64 {
                let mut hits: Vec<DocId> = docs.iter().filter(|&d| candidates.contains(d)).collect();
                hits.reverse();
                hits.truncate(take);
                out.extend(hits);
            } else {
                out.extend((&docs & candidates).top_desc(take));
            }
        }
        if !missing_first && out.len() < needed {
            missing(&mut out);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::values::{Ords, ValueCollector};

    fn segment(vals: &[Option<i64>]) -> Segment {
        let n = vals.len() as u32;
        let mut c = ValueCollector::<i64>::new(n, Ords::Single);
        for (d, v) in vals.iter().enumerate() {
            if let Some(v) = v {
                c.add(d as DocId, *v);
            }
        }
        let mut seg = Segment::empty();
        seg.values.insert(0, ValueIndex::build_numbers(c, false).unwrap());
        seg.all_docs.add_range(0..n);
        seg
    }

    fn string_segment(vals: &[Option<&str>]) -> Segment {
        let n = vals.len() as u32;
        let mut c = ValueCollector::<String>::new(n, Ords::Single);
        for (d, v) in vals.iter().enumerate() {
            if let Some(v) = v {
                c.add(d as DocId, v.to_string());
            }
        }
        let mut seg = Segment::empty();
        seg.values.insert(0, ValueIndex::build_strings(c).unwrap());
        seg.all_docs.add_range(0..n);
        seg
    }

    fn by_key(seg: &Segment, desc: bool, missing: Missing) -> Vec<DocId> {
        let col = SortColumn::of(seg, 0).unwrap();
        let mf = col.missing_first(desc, missing);
        let mut docs: Vec<DocId> = seg.all_docs.iter().collect();
        docs.sort_by_key(|&d| col.key(d, desc, mf));
        docs
    }

    #[test]
    fn numbers_ties_highest_doc_first_missing_last_both_ways() {
        let seg = segment(&[Some(30), Some(10), None, Some(20), Some(10), None]);
        assert_eq!(by_key(&seg, false, Missing::Normal), [4, 1, 3, 0, 5, 2]);
        assert_eq!(by_key(&seg, true, Missing::Normal), [0, 3, 4, 1, 5, 2]);
        assert_eq!(by_key(&seg, true, Missing::First), [5, 2, 0, 3, 4, 1]);
        assert_eq!(by_key(&seg, false, Missing::First), [5, 2, 4, 1, 3, 0]);
        let col = SortColumn::of(&seg, 0).unwrap();
        assert_eq!(col.order(1, true, false), col.order(4, true, false));
    }

    #[test]
    fn strings_without_a_value_first_descending() {
        let seg = string_segment(&[Some("b"), None, Some("a"), Some("b"), None]);
        assert_eq!(by_key(&seg, false, Missing::Normal), [2, 3, 0, 4, 1]);
        assert_eq!(by_key(&seg, true, Missing::Normal), [4, 1, 3, 0, 2]);
        assert_eq!(by_key(&seg, true, Missing::Last), [3, 0, 2, 4, 1]);
    }

    #[test]
    fn a_walk_past_its_budget_gives_up() {
        let vals: Vec<Option<i64>> = (0..3000).map(|i| Some((i % 100) as i64)).collect();
        let seg = segment(&vals);
        let col = SortColumn::of(&seg, 0).unwrap();
        // the candidates hold only the largest values: an ascending walk meets none for long
        let cand: Bitmap = (0..3000u32).filter(|d| d % 100 >= 90).collect();
        assert_eq!(col.presorted_within(&cand, false, false, 10, 100), None);
        assert_eq!(col.presorted_within(&cand, false, false, 10, u64::MAX), Some(col.presorted(&cand, false, false, 10)));
        // descending, the first value holds them
        assert_eq!(col.presorted_within(&cand, true, false, 10, 100), Some(col.presorted(&cand, true, false, 10)));
    }

    #[test]
    fn presorted_walk_equals_sorting_by_key() {
        let vals: Vec<Option<i64>> = (0..3000).map(|i| if i % 11 == 0 { None } else { Some((i * 37 % 101) as i64) }).collect();
        let seg = segment(&vals);
        let col = SortColumn::of(&seg, 0).unwrap();
        let cand: Bitmap = (0..3000u32).filter(|d| d % 3 != 1).collect();
        for desc in [false, true] {
            for mf in [false, true] {
                let mut want: Vec<DocId> = cand.iter().collect();
                want.sort_by_key(|&d| col.key(d, desc, mf));
                for needed in [1, 10, 500, 2000, 5000] {
                    let got = col.presorted(&cand, desc, mf, needed);
                    assert_eq!(got, want[..needed.min(want.len())], "desc {desc} missing first {mf} needed {needed}");
                }
            }
        }
    }

    #[test]
    fn bools_sort_false_before_true() {
        let mut seg = Segment::empty();
        seg.bools.insert(0, BoolIndex::build(&[1, 3], &[0, 2]).unwrap());
        seg.all_docs.add_range(0..5);
        assert_eq!(by_key(&seg, false, Missing::Normal), [2, 0, 3, 1, 4]);
        assert_eq!(SortColumn::of(&seg, 0).unwrap().presorted(&seg.all_docs, true, false, 3), [3, 1, 2]);
    }
}
