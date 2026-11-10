//! The text stage of a search: which documents a query finds, over every segment at once, and how
//! each ranks.
//!
//! Per word and field the candidates are the terms the word may match: the word itself, the most
//! frequent completions of the last word, then by rising cost its typo corrections and a
//! name-matching field's channels (`crate::names`), and infix matches. Candidates come from the
//! dictionaries of all segments, by the documents still live, so what a search finds and every
//! score are those of one segment holding the live documents.
//!
//! A word matches a document when one of its candidates is in one of the fields; a query's words
//! must all match. The cost rises while fewer results than `typo_tokens_threshold` are found; then,
//! with nothing found, words are joined and split (`split_join_tokens`) and infix fallbacks run;
//! then words are dropped while fewer than `drop_tokens_threshold` are found, those matching
//! nothing first. Each round adds the documents it finds, scored by what it matched
//! (`scoring::Rank`): BM25F with the collection's live statistics, after the words matched and the
//! cost. When a field sort leaves the text match only its ties (`search`'s `pick`), the documents
//! that can reach the page are ranked once every round is done, each by its round's candidates.

use super::highlight::Mark;
use super::scoring::{compute_bm25, compute_idf, MatchType, Rank, WordMatch};
use crate::bitmap::Bitmap;
use crate::filter::{Match, TextFilter};
use crate::index::inverted::{InvertedIndex, TermEntry, TermRef};
use crate::index::Index;
use crate::names::Channel;
use crate::schema::{DocId, FieldDef, FieldId, FieldType, Language, Schema};
use crate::text::Analyzer;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The largest field weight (`query_by_weights`).
pub const FIELD_MAX_WEIGHT: u32 = 127;

/// Tokens longer than this get no typo candidates: the automaton grows with the token, and such
/// tokens are identifiers, not words.
const MAX_TYPO_TOKEN_CHARS: usize = 64;
/// Per word and field the terms an infix search expands to: very short words match large parts of
/// the vocabulary.
const MAX_INFIX_EXPANSIONS: usize = 100;
/// Documents scored between two looks at the clock.
const DEADLINE_CHUNK: usize = 4096;
/// At most this many documents, in all segments, are read for an array element holding exactly the
/// words (`Search::settle`): each costs decoding a stored document.
const UNSURE_MAX: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Infix {
    Off,
    Always,
    Fallback,
}

impl Infix {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "off" => Some(Self::Off),
            "always" => Some(Self::Always),
            "fallback" => Some(Self::Fallback),
            _ => None,
        }
    }
}

/// When a name-matching field's channels run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameMatching {
    Off,
    Fallback,
    Always,
}

impl NameMatching {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Self::Off),
            "fallback" => Some(Self::Fallback),
            "always" => Some(Self::Always),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitJoin {
    Off,
    Fallback,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropMode {
    RightToLeft,
    LeftToRight,
    /// Both ends for queries of up to 3 words, else as right to left.
    BothSides,
}

impl DropMode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "right_to_left" => Ok(Self::RightToLeft),
            "left_to_right" => Ok(Self::LeftToRight),
            "both_sides:3" | "both_sides" => Ok(Self::BothSides),
            other => Err(format!("Invalid drop_tokens_mode: `{other}`. Expected `right_to_left`, `left_to_right` or `both_sides:3`.")),
        }
    }
}

/// When a search must stop.
#[derive(Clone, Copy, Debug)]
pub struct Deadline(Instant);

impl Deadline {
    pub fn new(start: Instant, ms: u64) -> Self {
        Self(start + Duration::from_millis(ms))
    }

    #[inline]
    pub fn passed(&self) -> bool {
        Instant::now() >= self.0
    }
}

pub struct Params {
    pub typo_tokens_threshold: u64,
    pub drop_tokens_threshold: u64,
    pub drop_mode: DropMode,
    pub split_join: SplitJoin,
    pub max_candidates: usize,
    pub min_len_1typo: usize,
    pub min_len_2typo: usize,
    pub exhaustive: bool,
    pub match_type: MatchType,
    pub prioritize_exact_match: bool,
    pub prioritize_num_matching_fields: bool,
    pub typos_for_numerical: bool,
    pub typos_for_alpha_numerical: bool,
    pub deadline: Deadline,
    pub max_extra_prefix: usize,
    pub max_extra_suffix: usize,
}

/// A searched field, with its terms in each segment.
pub struct Field<'a> {
    pub fid: FieldId,
    pub def: &'a FieldDef,
    pub invs: Vec<Option<&'a InvertedIndex>>,
    pub weight: u32,
    pub num_typos: u32,
    pub prefix: bool,
    pub infix: Infix,
    pub names: NameMatching,
}

/// A segment's part in a search.
pub struct Segment<'a> {
    pub index: &'a Index,
    /// The documents a result may hold; deleted ones are out of it.
    pub filter: Option<&'a Bitmap>,
    pub excluded: Option<&'a Bitmap>,
    pub deleted: Option<&'a Bitmap>,
    /// With group_by, a document's group: `typo_tokens_threshold` counts groups.
    pub group_of: Option<&'a dyn Fn(DocId) -> Option<u64>>,
}

/// A query: its words as written, its phrases' words (which filter the documents already; scored
/// here), and what synonyms rewrite it to.
pub struct Query<'a> {
    pub words: &'a [String],
    pub phrases: &'a [Vec<String>],
    pub rewrites: &'a [Vec<String>],
}

/// Above this many scores a segment's move to one slot per document.
const SPARSE_SCORES: usize = 1 << 16;

/// Each document's rank (`Rank::pack`), 0 when it has none. A few documents of a large segment do
/// not pay for a slot per document; many do not pay for a hash per lookup.
#[derive(Clone, Default)]
pub struct Scores {
    sparse: FxHashMap<DocId, u64>,
    dense: Vec<u64>,
}

impl Scores {
    #[inline]
    pub fn get(&self, doc: DocId) -> u64 {
        if self.dense.is_empty() {
            self.sparse.get(&doc).copied().unwrap_or(0)
        } else {
            self.dense.get(doc as usize).copied().unwrap_or(0)
        }
    }

    /// `docs`: the segment's documents, for the slots.
    fn set(&mut self, doc: DocId, score: u64, docs: u32) {
        if self.dense.is_empty() {
            self.sparse.insert(doc, score);
            if self.sparse.len() > SPARSE_SCORES {
                self.densify(docs);
            }
        } else {
            self.dense[doc as usize] = score;
        }
    }

    /// Room for `n` more documents' scores: the slots at once when they will be needed, rather
    /// than a map grown and then moved.
    fn reserve(&mut self, n: usize, docs: u32) {
        if !self.dense.is_empty() {
            return;
        }
        if self.sparse.len() + n > SPARSE_SCORES {
            self.densify(docs);
        } else {
            self.sparse.reserve(n);
        }
    }

    fn densify(&mut self, docs: u32) {
        // documents may come in any order (a fused ranking's)
        let last = self.sparse.keys().copied().max().unwrap_or(0);
        self.dense = vec![0; docs.max(last + 1) as usize];
        for (d, s) in self.sparse.drain() {
            self.dense[d as usize] = s;
        }
    }

    pub fn insert(&mut self, doc: DocId, score: u64) {
        if !self.dense.is_empty() && doc as usize >= self.dense.len() {
            self.dense.resize(doc as usize + 1, 0);
        }
        self.set(doc, score, doc + 1);
    }

    pub fn clear(&mut self) {
        self.sparse.clear();
        self.dense.clear();
    }

    /// The documents with a score, in no set order.
    pub fn iter(&self) -> impl Iterator<Item = (DocId, u64)> + '_ {
        let sparse = self.sparse.iter().map(|(&d, &s)| (d, s));
        let dense = self.dense.iter().enumerate().filter(|e| *e.1 != 0).map(|(d, &s)| (d as DocId, s));
        sparse.chain(dense)
    }

    pub fn len(&self) -> usize {
        self.sparse.len() + self.dense.iter().filter(|&&s| s != 0).count()
    }
}

impl FromIterator<(DocId, u64)> for Scores {
    fn from_iter<I: IntoIterator<Item = (DocId, u64)>>(iter: I) -> Self {
        let mut scores = Self::default();
        iter.into_iter().for_each(|(d, s)| scores.insert(d, s));
        scores
    }
}

/// What a word was matched by, for `explain_matches`.
#[derive(Clone, Debug)]
struct Explained {
    fid: FieldId,
    term: Vec<u8>,
    level: u8,
    kind: &'static str,
    encoder: Option<&'static str>,
}

/// What a search leaves for its hits.
#[derive(Default)]
pub struct Marks {
    /// The terms matched, for highlighting.
    pub terms: FxHashMap<Vec<u8>, Mark>,
    /// The query's phrases as terms: as each searched field analyzes them.
    pub phrases: Vec<Vec<Vec<u8>>>,
    /// Per query (the words, then rewrites) per word its text and candidates, best first.
    words: Vec<Vec<(String, Vec<Explained>)>>,
}

impl Marks {
    /// How the words of the first query that explains `doc` matched it.
    pub fn explain(&self, index: &Index, doc: DocId) -> Vec<WordMatch> {
        for query in &self.words {
            let mut out = Vec::new();
            for (word, candidates) in query {
                let hit = candidates.iter().find(|c| index.inverted.get(&c.fid).is_some_and(|inv| inv.term_freq(&c.term, doc) > 0));
                if let Some(c) = hit {
                    out.push(WordMatch { token: Some(word.clone()), term: String::from_utf8_lossy(&c.term).into_owned(), kind: c.kind, typos: u32::from(c.level), encoder: c.encoder });
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
        Vec::new()
    }
}

pub struct Outcome {
    /// Per segment the documents found, and their ranks.
    pub found: Vec<Bitmap>,
    pub scores: Vec<Scores>,
    pub search_cutoff: bool,
    pub marks: Arc<Marks>,
}

/// A term a word may match in a field.
#[derive(Clone, Debug)]
struct Candidate {
    term: Vec<u8>,
    level: u8,
    /// Found by completing the word or inside a term, not as the word.
    completion: bool,
    kind: &'static str,
    encoder: Option<&'static str>,
}

/// A word of a query being searched.
struct Word {
    raw: String,
    lowered: String,
    /// Prefix-searched, in the fields that do.
    last: bool,
    /// Only as written: a phrase's word, a synonym's, or a word of a split or join.
    exact_only: bool,
    /// Never dropped: a phrase's word.
    required: bool,
    kind: &'static str,
    /// Per field its term.
    terms: Vec<Option<Vec<u8>>>,
    /// Per field its candidates.
    candidates: Vec<Vec<Candidate>>,
}

/// A candidate in a segment.
struct SegTerm {
    field: usize,
    level: u8,
    completion: bool,
    idf: f32,
    entry: TermEntry,
}

/// A word in a segment: the documents of its candidates, and the candidates by cost.
#[derive(Default)]
struct SegWord {
    docs: Bitmap,
    terms: Vec<SegTerm>,
}

/// One query of the search (the words, a rewrite, a split or a join).
struct Variant {
    id: usize,
    words: Vec<Word>,
    /// Per segment per word.
    segs: Vec<Vec<SegWord>>,
}

/// Documents found and not yet ranked (`search`'s `pick`): matched by `words` of variant `variant`
/// in segment `seg`, each word by its first `lens` candidates there (those it had then).
struct Pending {
    variant: usize,
    words: Vec<usize>,
    lens: Vec<usize>,
    seg: usize,
    docs: Bitmap,
}

/// Documents of segment `seg` matched by `words` of variant `variant`, each with an array field (by
/// index) whose whole value counts that holds every word, and other words besides: one of its
/// elements may hold exactly the words, which only the stored text tells.
struct Unsure {
    variant: usize,
    words: Vec<usize>,
    seg: usize,
    docs: Vec<(DocId, usize)>,
}

/// The collection's live statistics of a field.
struct FieldStats {
    avg_length: f32,
}

struct Search<'a, 's> {
    fields: &'a [Field<'s>],
    segs: &'a [Segment<'s>],
    params: &'a Params,
    schema: &'s Schema,
    language: Language,
    live_docs: u64,
    stats: Vec<FieldStats>,
    found: Vec<Bitmap>,
    scores: Vec<Scores>,
    total: u64,
    groups: FxHashSet<u64>,
    cutoff: bool,
    analyzer: RefCell<Analyzer>,
    variants: Cell<usize>,
    /// With `pick`: the documents found, ranked once the search is done.
    pending: Option<Vec<Pending>>,
    unsure: Vec<Unsure>,
}

impl<'a, 's> Search<'a, 's> {
    fn new(fields: &'a [Field<'s>], segs: &'a [Segment<'s>], params: &'a Params, schema: &'s Schema) -> Self {
        let live_docs = segs.iter().map(|s| u64::from(s.index.total_docs()) - s.deleted.map_or(0, Bitmap::cardinality)).sum();
        let stats = fields
            .iter()
            .map(|f| {
                let (mut total, mut docs) = (0u64, 0u64);
                for (inv, seg) in f.invs.iter().zip(segs) {
                    if let Some(inv) = inv {
                        let (t, d) = seg.deleted.map_or((0, 0), |deleted| seg.index.deleted_lengths(f.fid, inv, deleted));
                        total += inv.total_length - t;
                        docs += u64::from(inv.docs_with_tokens) - d;
                    }
                }
                FieldStats { avg_length: if docs == 0 { 1.0 } else { (total as f64 / docs as f64) as f32 } }
            })
            .collect();
        Self {
            fields,
            segs,
            params,
            schema,
            language: schema.default_language,
            live_docs,
            stats,
            found: vec![Bitmap::new(); segs.len()],
            scores: vec![Scores::default(); segs.len()],
            total: 0,
            groups: FxHashSet::default(),
            cutoff: false,
            analyzer: RefCell::default(),
            variants: Cell::new(0),
            pending: None,
            unsure: Vec::new(),
        }
    }

    /// Documents holding `r` in segment `s`, deleted copies left out.
    fn live_df(&self, inv: &InvertedIndex, r: TermRef, s: usize) -> u64 {
        let df = u64::from(inv.df_of(r));
        match self.segs[s].deleted {
            Some(d) => df - inv.postings_of(r).and_cardinality(d),
            None => df,
        }
    }

    /// Live documents holding `term` in field `f`.
    fn term_df(&self, f: usize, term: &[u8]) -> u64 {
        self.fields[f].invs.iter().enumerate().filter_map(|(s, inv)| inv.map(|inv| (s, inv))).filter_map(|(s, inv)| inv.lookup(term).map(|r| self.live_df(inv, r, s))).sum()
    }

    fn word(&self, raw: &str, last: bool, exact_only: bool, required: bool, kind: &'static str) -> Word {
        let mut analyzer = self.analyzer.borrow_mut();
        let symbols = &self.schema.symbols_to_index;
        let mut lowered = String::new();
        let terms = self
            .fields
            .iter()
            .map(|f| {
                analyzer.analyze(raw, symbols, f.def, self.language).map(|(term, low)| {
                    lowered.clear();
                    lowered.push_str(low);
                    term.as_bytes().to_vec()
                })
            })
            .collect();
        Word { raw: raw.to_string(), lowered, last, exact_only, required, kind, terms, candidates: vec![Vec::new(); self.fields.len()] }
    }

    fn variant(&self, words: Vec<Word>) -> Variant {
        let segs = self.segs.iter().map(|_| words.iter().map(|_| SegWord::default()).collect()).collect();
        let id = self.variants.get();
        self.variants.set(id + 1);
        Variant { id, words, segs }
    }

    /// Add `found` to word `w`'s candidates in field `f`, the ones it has not.
    fn add(&self, v: &mut Variant, w: usize, f: usize, found: Vec<Candidate>) {
        for c in found {
            if v.words[w].candidates[f].iter().any(|x| x.term == c.term) {
                continue;
            }
            let idf = compute_idf(self.live_docs, self.term_df(f, &c.term));
            for (s, inv) in self.fields[f].invs.iter().enumerate() {
                let Some(entry) = inv.and_then(|inv| inv.term_entry(&c.term)) else { continue };
                let sw = &mut v.segs[s][w];
                sw.docs |= entry.bitmap();
                sw.terms.push(SegTerm { field: f, level: c.level, completion: c.completion, idf, entry });
            }
            v.words[w].candidates[f].push(c);
        }
    }

    /// The `limit` terms of `dfs` with the most live documents (ties in term order).
    fn most_frequent(mut dfs: Vec<(Vec<u8>, u64)>, limit: usize) -> Vec<Vec<u8>> {
        dfs.retain(|e| e.1 > 0);
        dfs.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        dfs.truncate(limit);
        dfs.into_iter().map(|e| e.0).collect()
    }

    /// The word itself, the completions of a last word and `always` infix matches.
    fn expand_exact(&self, v: &mut Variant, w: usize) {
        let k = self.params.max_candidates;
        for (f, field) in self.fields.iter().enumerate() {
            let word = &v.words[w];
            let kind = word.kind;
            let mut found = Vec::new();
            if let Some(term) = word.terms[f].clone()
                && self.term_df(f, &term) > 0
            {
                found.push(Candidate { term, level: 0, completion: false, kind, encoder: None });
            }
            if word.last && field.prefix && !word.exact_only && !word.lowered.is_empty() {
                // One segment's walk gives the most frequent completions. Over several, or with
                // deleted copies, their order is only near the live one, so each gives more of them
                // for the sums below to choose from.
                let exact_order = self.segs.len() == 1 && self.segs[0].deleted.is_none();
                let walk = if exact_order { k.saturating_add(1) } else { k.saturating_mul(4).max(32) };
                let mut dfs: FxHashMap<Vec<u8>, u64> = FxHashMap::default();
                for (s, inv) in field.invs.iter().enumerate() {
                    let Some(inv) = inv else { continue };
                    for (term, r) in inv.prefix(word.lowered.as_bytes(), walk) {
                        *dfs.entry(term).or_default() += self.live_df(inv, r, s);
                    }
                }
                let exact = word.terms[f].as_deref();
                let completions = dfs.into_iter().filter(|e| Some(e.0.as_slice()) != exact).collect();
                found.extend(Self::most_frequent(completions, k).into_iter().map(|term| Candidate { term, level: 0, completion: true, kind: "prefix", encoder: None }));
            }
            self.add(v, w, f, found);
        }
    }

    /// The terms of field `f` holding the word inside, within `max_extra_prefix` and
    /// `max_extra_suffix`.
    fn infix_terms(&self, word: &Word, f: usize) -> Vec<Candidate> {
        let Some(term) = word.terms[f].as_deref() else { return Vec::new() };
        let (max_pre, max_suf) = (self.params.max_extra_prefix, self.params.max_extra_suffix);
        let mut dfs: FxHashMap<Vec<u8>, u64> = FxHashMap::default();
        for (s, inv) in self.fields[f].invs.iter().enumerate() {
            let Some(inv) = inv else { continue };
            inv.for_each_term(|t, r| {
                if t.len() > term.len()
                    && let Some(pos) = t.windows(term.len()).position(|x| x == term)
                    && pos <= max_pre
                    && t.len() - pos - term.len() <= max_suf
                {
                    *dfs.entry(t.to_vec()).or_default() += self.live_df(inv, r, s);
                }
                true
            });
        }
        Self::most_frequent(dfs.into_iter().collect(), MAX_INFIX_EXPANSIONS).into_iter().map(|term| Candidate { term, level: 0, completion: true, kind: "infix", encoder: None }).collect()
    }

    /// Run the infix searches of `mode`; Err for a field without an infix index.
    fn expand_infix(&self, v: &mut Variant, mode: Infix) -> Result<bool, String> {
        let mut added = false;
        for w in 0..v.words.len() {
            if v.words[w].exact_only {
                continue;
            }
            for (f, field) in self.fields.iter().enumerate() {
                if field.infix != mode {
                    continue;
                }
                if !field.def.infix {
                    return Err(format!("Could not find `{}` in the infix index. Make sure to enable infix search by specifying `infix: true` in the schema.", field.def.name));
                }
                let found = self.infix_terms(&v.words[w], f);
                added |= !found.is_empty();
                self.add(v, w, f, found);
            }
        }
        Ok(added)
    }

    /// How many typos field `f` allows word `term`.
    fn typo_budget(&self, term: &str, f: usize) -> u32 {
        let chars = term.chars().count();
        let digits = term.chars().filter(char::is_ascii_digit).count();
        let gate = match digits {
            0 => true,
            d if d == chars => self.params.typos_for_numerical,
            _ => self.params.typos_for_alpha_numerical,
        };
        if !gate || chars > MAX_TYPO_TOKEN_CHARS || chars < self.params.min_len_1typo {
            0
        } else if chars < self.params.min_len_2typo {
            self.fields[f].num_typos.min(1)
        } else {
            self.fields[f].num_typos
        }
    }

    /// The candidates of cost `level`: typo corrections, then the channels of a name-matching field.
    fn expand_level(&self, v: &mut Variant, w: usize, level: u8) {
        let k = self.params.max_candidates;
        for (f, field) in self.fields.iter().enumerate() {
            let word = &v.words[w];
            let Some(term) = word.terms[f].as_deref().filter(|_| !word.exact_only) else { continue };
            let mut found = Vec::new();
            let text = std::str::from_utf8(term).unwrap_or("");
            if self.typo_budget(text, f) >= u32::from(level) {
                // the last word may be unfinished too: its typos are in what was typed
                let whole = crate::index::inverted::typo_dfa(text, level);
                let completes = word.last && field.prefix;
                let prefix_dfa = completes.then(|| crate::index::inverted::prefix_typo_dfa(text, level));
                let dfa = prefix_dfa.as_ref().unwrap_or(&whole);
                let mut dfs: FxHashMap<Vec<u8>, u64> = FxHashMap::default();
                for (s, inv) in field.invs.iter().enumerate() {
                    let Some(inv) = inv else { continue };
                    for (t, d, r) in inv.fuzzy(dfa) {
                        if d == level {
                            *dfs.entry(t).or_default() += self.live_df(inv, r, s);
                        }
                    }
                }
                found.extend(Self::most_frequent(dfs.into_iter().collect(), k).into_iter().map(|term| {
                    let completion = completes && !matches!(whole.eval(&term), levenshtein_automata::Distance::Exact(d) if d == level);
                    Candidate { term, level, completion, kind: "typo", encoder: None }
                }));
            }
            if field.names != NameMatching::Off && field.num_typos >= u32::from(level) {
                let mut keys = Vec::new();
                for channel in Channel::ALL.into_iter().filter(|c| c.cost() == i32::from(level)) {
                    crate::names::keys(term, channel, &mut keys);
                    let mut dfs: FxHashMap<Vec<u8>, u64> = FxHashMap::default();
                    for key in &keys {
                        for (s, inv) in field.invs.iter().enumerate() {
                            let Some((inv, ks)) = inv.and_then(|inv| Some((inv, inv.keys()?))) else { continue };
                            ks.terms_of(channel, key, |t| {
                                if let Some(r) = inv.lookup(t) {
                                    *dfs.entry(t.to_vec()).or_default() += self.live_df(inv, r, s);
                                }
                            });
                        }
                    }
                    let (kind, encoder) = if channel == Channel::Fold { ("fold", None) } else { ("phonetic", Some(channel.name())) };
                    found.extend(Self::most_frequent(dfs.into_iter().collect(), usize::MAX).into_iter().map(|term| Candidate { term, level, completion: false, kind, encoder }));
                }
            }
            self.add(v, w, f, found);
        }
    }

    /// Find the documents with every word of `words` (indexes into `v`), add the new ones.
    fn run(&mut self, v: &Variant, words: &[usize]) {
        if words.is_empty() {
            return;
        }
        for s in 0..self.segs.len() {
            if self.cutoff {
                return;
            }
            let mut order: Vec<&Bitmap> = words.iter().map(|&w| &v.segs[s][w].docs).collect();
            order.sort_by_key(|d| d.cardinality());
            let mut docs = order[0].clone();
            for d in &order[1..] {
                if docs.is_empty() {
                    break;
                }
                docs &= *d;
            }
            let seg = &self.segs[s];
            if let Some(f) = seg.filter {
                docs &= f;
            }
            if let Some(d) = seg.deleted {
                docs.andnot_inplace(d);
            }
            if let Some(x) = seg.excluded {
                docs.andnot_inplace(x);
            }
            docs.andnot_inplace(&self.found[s]);
            if !docs.is_empty() {
                self.score(v, words, s, &docs);
            }
        }
    }

    /// Rank `docs` of segment `s`, matched by `words`, and add them to what was found. Past the
    /// deadline the rest are left out. With `pick` they are found now and ranked at the end.
    fn score(&mut self, v: &Variant, words: &[usize], s: usize, docs: &Bitmap) {
        if let Some(pending) = self.pending.as_mut() {
            let lens = words.iter().map(|&w| v.segs[s][w].terms.len()).collect();
            pending.push(Pending { variant: v.id, words: words.to_vec(), lens, seg: s, docs: docs.clone() });
            self.total += docs.cardinality();
            self.found[s] |= docs;
            return;
        }
        let done = self.rank(v, words, None, s, docs);
        if let Some(group_of) = self.segs[s].group_of {
            self.groups.extend(done.iter().filter_map(|&d| group_of(d)));
            self.total = self.groups.len() as u64;
        } else {
            self.total += done.len() as u64;
        }
        if done.len() as u64 == docs.cardinality() {
            self.found[s] |= docs;
        } else {
            self.found[s] |= &Bitmap::of(&done);
        }
    }

    /// Rank `docs` of segment `s`, matched by `words`, into the scores; the documents ranked. A
    /// round's ranking (no `lens`) stops at the deadline; a pending one ranks each word by the first
    /// `lens` candidates it had when found, and all of `docs`.
    fn rank(&mut self, v: &Variant, words: &[usize], lens: Option<&[usize]>, s: usize, docs: &Bitmap) -> Vec<DocId> {
        let total_docs = self.segs[s].index.total_docs();
        let n = docs.cardinality();
        self.scores[s].reserve(n as usize, total_docs);
        // a document's probe of a term (hit or not) costs about what reading two of the term's
        // postings in order does
        let (mut postings, mut terms) = (0u64, 0u64);
        for (k, &w) in words.iter().enumerate() {
            for t in v.segs[s][w].terms.iter().take(lens.map_or(usize::MAX, |l| l[k])) {
                postings += t.entry.bitmap().cardinality();
                terms += 1;
            }
        }
        if postings <= 2 * n * terms {
            return self.rank_by_terms(v, words, lens, s, docs);
        }
        let params = self.params;
        let fields = self.fields;
        let n_fields = fields.len();
        let whole = self.whole_counts();
        let mut unsure = Vec::new();
        let mut cursors: Vec<Vec<_>> = words.iter().map(|&w| v.segs[s][w].terms.iter().map(|t| t.entry.cursor()).collect()).collect();
        let invs: Vec<Option<&InvertedIndex>> = self.fields.iter().map(|f| f.invs[s]).collect();
        let mut field_score = vec![0f32; n_fields];
        let mut exact_words = vec![0usize; n_fields];
        let mut lengths = vec![0u32; n_fields];
        let mut in_word = vec![usize::MAX; n_fields];
        let mut done: Vec<DocId> = Vec::with_capacity(docs.cardinality().min(1 << 20) as usize);
        for (i, doc) in docs.iter().enumerate() {
            if lens.is_none() && i % DEADLINE_CHUNK == 0 && params.deadline.passed() {
                self.cutoff = true;
                break;
            }
            field_score.fill(0.0);
            exact_words.fill(0);
            lengths.fill(u32::MAX);
            let mut rank = Rank { words: words.len() as u32, ..Default::default() };
            for (k, &w) in words.iter().enumerate() {
                let mut best: Option<(u8, bool)> = None;
                let n = lens.map_or(usize::MAX, |l| l[k]);
                for (t, cursor) in v.segs[s][w].terms.iter().zip(&mut cursors[k]).take(n) {
                    // a field counts a word once, by its best candidate there
                    if in_word[t.field] == k {
                        continue;
                    }
                    let tf = cursor.tf(doc);
                    if tf == 0 {
                        continue;
                    }
                    in_word[t.field] = k;
                    if lengths[t.field] == u32::MAX {
                        lengths[t.field] = invs[t.field].map_or(0, |inv| inv.doc_length(doc));
                    }
                    field_score[t.field] += compute_bm25(tf, lengths[t.field], self.stats[t.field].avg_length, t.idf);
                    exact_words[t.field] += usize::from(t.level == 0 && !t.completion);
                    best = Some(best.map_or((t.level, t.completion), |b| b.min((t.level, t.completion))));
                }
                if let Some((level, completion)) = best {
                    rank.typos += u32::from(level);
                    rank.completions += u32::from(completion);
                }
            }
            in_word.fill(usize::MAX);
            let weighted = field_score.iter().zip(self.fields).filter(|e| *e.0 > 0.0).map(|(&s, f)| (s * f.weight.max(1) as f32, f.weight));
            let (score, weight) = params.match_type.combine(weighted);
            rank.score = score;
            rank.weight = weight;
            if params.prioritize_exact_match {
                // every word as written, in a field whose whole value counts
                let all = |f: usize| exact_words[f] == words.len() && whole[f];
                rank.exact = (0..n_fields).any(|f| all(f) && lengths[f] as usize == words.len());
                if !rank.exact {
                    let arrays = (0..n_fields).filter(|&f| all(f) && fields[f].def.field_type == FieldType::StringArray);
                    unsure.extend(arrays.max_by_key(|&f| fields[f].weight).map(|f| (doc, f)));
                }
            }
            if params.prioritize_num_matching_fields {
                rank.fields = field_score.iter().filter(|&&s| s > 0.0).count() as u32;
            }
            self.scores[s].set(doc, rank.pack(), total_docs);
            done.push(doc);
        }
        if !unsure.is_empty() {
            self.unsure.push(Unsure { variant: v.id, words: words.to_vec(), seg: s, docs: unsure });
        }
        done
    }

    /// `rank` term by term: per window of documents, each candidate's postings read in order (a
    /// frequency by its position, no lookup; a term not in a document costs nothing). Each
    /// document's state is added to as `rank` adds to it, word by word and candidate by candidate,
    /// so the scores are the same.
    fn rank_by_terms(&mut self, v: &Variant, words: &[usize], lens: Option<&[usize]>, s: usize, docs: &Bitmap) -> Vec<DocId> {
        const WINDOW: u32 = 4096;
        let params = self.params;
        let fields = self.fields;
        let n_fields = fields.len();
        let whole = self.whole_counts();
        let mut unsure = Vec::new();
        let total_docs = self.segs[s].index.total_docs();
        let invs: Vec<Option<&InvertedIndex>> = self.fields.iter().map(|f| f.invs[s]).collect();
        // per word its candidates, each with its postings read once, in order, across the windows
        let mut postings: Vec<Vec<(&SegTerm, std::iter::Peekable<std::iter::Enumerate<crate::bitmap::Iter<'_>>>)>> = words
            .iter()
            .enumerate()
            .map(|(k, &w)| {
                let n = lens.map_or(usize::MAX, |l| l[k]);
                v.segs[s][w].terms.iter().take(n).map(|t| (t, t.entry.bitmap().iter().enumerate().peekable())).collect()
            })
            .collect();
        let slots = WINDOW as usize * n_fields;
        let mut field_score = vec![0f32; slots];
        let mut exact_words = vec![0u32; slots];
        let mut lengths = vec![u32::MAX; slots];
        let mut in_word = vec![usize::MAX; slots];
        let mut best = vec![u16::MAX; WINDOW as usize];
        let mut typos = vec![0u32; WINDOW as usize];
        let mut completions = vec![0u32; WINDOW as usize];
        let mut is_cand = vec![0u64; WINDOW as usize / 64];
        let mut window: Vec<DocId> = Vec::with_capacity(WINDOW as usize);
        let mut done: Vec<DocId> = Vec::with_capacity(docs.cardinality().min(1 << 20) as usize);
        let mut cands = docs.iter().peekable();
        while let Some(&first) = cands.peek() {
            if lens.is_none() && params.deadline.passed() {
                self.cutoff = true;
                break;
            }
            let lo = first - first % WINDOW;
            let hi = lo.saturating_add(WINDOW);
            window.clear();
            while let Some(d) = cands.next_if(|&d| d < hi) {
                window.push(d);
                let i = (d - lo) as usize;
                is_cand[i / 64] |= 1 << (i % 64);
            }
            for (k, terms) in postings.iter_mut().enumerate() {
                for (t, it) in terms.iter_mut() {
                    while let Some((rank, doc)) = it.next_if(|&(_, d)| d < hi) {
                        let i = doc.wrapping_sub(lo) as usize;
                        if doc < lo || is_cand[i / 64] & (1 << (i % 64)) == 0 {
                            continue;
                        }
                        // a field counts a word once, by its best candidate there
                        let slot = i * n_fields + t.field;
                        if in_word[slot] == k {
                            continue;
                        }
                        let tf = t.entry.tf_at(rank as u64);
                        if tf == 0 {
                            continue;
                        }
                        in_word[slot] = k;
                        if lengths[slot] == u32::MAX {
                            lengths[slot] = invs[t.field].map_or(0, |inv| inv.doc_length(doc));
                        }
                        field_score[slot] += compute_bm25(tf, lengths[slot], self.stats[t.field].avg_length, t.idf);
                        exact_words[slot] += u32::from(t.level == 0 && !t.completion);
                        best[i] = best[i].min(u16::from(t.level) << 1 | u16::from(t.completion));
                    }
                }
                for &d in &window {
                    let i = (d - lo) as usize;
                    if best[i] != u16::MAX {
                        typos[i] += u32::from(best[i] >> 1);
                        completions[i] += u32::from(best[i] & 1);
                        best[i] = u16::MAX;
                    }
                }
            }
            for &doc in &window {
                let i = (doc - lo) as usize;
                let at = i * n_fields;
                let (fs, ew, ls) = (&field_score[at..at + n_fields], &exact_words[at..at + n_fields], &lengths[at..at + n_fields]);
                let mut rank = Rank { words: words.len() as u32, typos: typos[i], completions: completions[i], ..Default::default() };
                let weighted = fs.iter().zip(self.fields).filter(|e| *e.0 > 0.0).map(|(&s, f)| (s * f.weight.max(1) as f32, f.weight));
                let (score, weight) = params.match_type.combine(weighted);
                rank.score = score;
                rank.weight = weight;
                if params.prioritize_exact_match {
                    let all = |f: usize| ew[f] as usize == words.len() && whole[f];
                    rank.exact = (0..n_fields).any(|f| all(f) && ls[f] as usize == words.len());
                    if !rank.exact {
                        let arrays = (0..n_fields).filter(|&f| all(f) && fields[f].def.field_type == FieldType::StringArray);
                        unsure.extend(arrays.max_by_key(|&f| fields[f].weight).map(|f| (doc, f)));
                    }
                }
                if params.prioritize_num_matching_fields {
                    rank.fields = fs.iter().filter(|&&s| s > 0.0).count() as u32;
                }
                self.scores[s].set(doc, rank.pack(), total_docs);
                done.push(doc);
                // the window's slots as the next window needs them
                field_score[at..at + n_fields].fill(0.0);
                exact_words[at..at + n_fields].fill(0);
                lengths[at..at + n_fields].fill(u32::MAX);
                in_word[at..at + n_fields].fill(usize::MAX);
                typos[i] = 0;
                completions[i] = 0;
                is_cand[i / 64] = 0;
            }
        }
        if !unsure.is_empty() {
            self.unsure.push(Unsure { variant: v.id, words: words.to_vec(), seg: s, docs: unsure });
        }
        done
    }

    /// Per field whether its whole value holding the words ranks a document first
    /// (`prioritize_exact_match`): any field's, but under max_weight, where the heaviest field
    /// matched decides, only the heaviest fields'.
    fn whole_counts(&self) -> Vec<bool> {
        let top = self.fields.iter().map(|f| f.weight).max().unwrap_or(0);
        self.fields.iter().map(|f| self.params.match_type != MatchType::MaxWeight || f.weight == top).collect()
    }

    /// The unsure documents with an array element of exactly the words (in any order) match whole,
    /// read from their stored text: those of the heaviest fields first, all of a weight or none, at
    /// most `UNSURE_MAX`.
    fn settle(&mut self, variants: &[Variant]) {
        let mut per_weight: BTreeMap<u32, usize> = BTreeMap::new();
        for u in &self.unsure {
            for &(_, f) in &u.docs {
                *per_weight.entry(self.fields[f].weight).or_default() += 1;
            }
        }
        let (mut room, mut lightest) = (UNSURE_MAX, u32::MAX);
        for (&weight, &n) in per_weight.iter().rev() {
            if n > room {
                break;
            }
            room -= n;
            lightest = weight;
        }
        for u in std::mem::take(&mut self.unsure) {
            let v = variants.iter().find(|v| v.id == u.variant).expect("an unsure round's variant");
            let value = u.words.iter().map(|&w| v.words[w].raw.as_str()).collect::<Vec<_>>().join(" ");
            let index = self.segs[u.seg].index;
            let mut filters: FxHashMap<usize, Option<TextFilter>> = FxHashMap::default();
            for (i, (doc, f)) in u.docs.into_iter().enumerate() {
                if i % 64 == 0 && self.params.deadline.passed() {
                    self.cutoff = true;
                    return;
                }
                if self.fields[f].weight < lightest {
                    continue;
                }
                let def = self.fields[f].def;
                let filter = filters.entry(f).or_insert_with(|| TextFilter::new(&value, Match::Words, self.schema, def));
                if filter.as_ref().is_some_and(|filter| crate::filter::text::element_is(index, doc, filter, self.schema, def)) {
                    let rank = self.scores[u.seg].get(doc);
                    self.scores[u.seg].set(doc, rank | Rank::EXACT, index.total_docs());
                }
            }
        }
    }

    /// The word split in two or joined with the next, wherever the pieces are terms of a field.
    fn split_join(&self, base: &Variant) -> Vec<Variant> {
        let plain = |raw: &str, last: bool| self.word(raw, last, true, false, "split_join");
        let has = |w: &Word| w.terms.iter().enumerate().any(|(f, t)| t.as_ref().is_some_and(|t| self.term_df(f, t) > 0));
        let again = |w: &Word| self.word(&w.raw, w.last, w.exact_only, w.required, w.kind);
        let words = &base.words;
        let n = words.iter().filter(|w| !w.required).count();
        let mut out = Vec::new();
        for i in 0..n {
            if i + 1 < n {
                let joined = plain(&format!("{}{}", words[i].lowered, words[i + 1].lowered), words[i + 1].last);
                if has(&joined) {
                    let mut list: Vec<Word> = words[..i].iter().map(again).collect();
                    list.push(joined);
                    list.extend(words[i + 2..].iter().map(again));
                    out.push(self.variant(list));
                }
            }
            let token = &words[i].lowered;
            for (at, _) in token.char_indices().skip(1) {
                let (left, right) = (plain(&token[..at], false), plain(&token[at..], words[i].last));
                if has(&left) && has(&right) {
                    let mut list: Vec<Word> = words[..i].iter().map(again).collect();
                    list.push(left);
                    list.push(right);
                    list.extend(words[i + 1..].iter().map(again));
                    out.push(self.variant(list));
                }
            }
        }
        out
    }

    /// The word lists to fall back to, best first: those that match something, then fewer from the
    /// end or ends `drop_mode` says. Required words stay in each.
    fn drops(&self, v: &Variant) -> Vec<Vec<usize>> {
        let matched = |w: usize| v.segs.iter().any(|s| !s[w].docs.is_empty());
        let required: Vec<usize> = (0..v.words.len()).filter(|&w| v.words[w].required).collect();
        let free: Vec<usize> = (0..v.words.len()).filter(|&w| !v.words[w].required).collect();
        let live: Vec<usize> = free.iter().copied().filter(|&w| matched(w)).collect();
        let with_required = |list: &[usize]| -> Vec<usize> { list.iter().chain(&required).copied().collect() };
        let mut out = Vec::new();
        if live.len() < free.len() {
            out.push(with_required(&live));
        }
        let n = live.len();
        let both = self.params.drop_mode == DropMode::BothSides && free.len() <= 3;
        let left = self.params.drop_mode == DropMode::LeftToRight;
        for k in 1..n {
            if !left {
                out.push(with_required(&live[..n - k]));
            }
            if left || both {
                out.push(with_required(&live[k..]));
            }
        }
        out
    }
}

/// The documents of a phrase in segment `seg`, in any of the fields, as far as `deadline` lets it
/// check them.
pub fn phrase_docs(phrase: &[String], fields: &[Field<'_>], seg: &Index, schema: &Schema, deadline: Deadline) -> Bitmap {
    let value = phrase.join(" ");
    let mut docs = Bitmap::new();
    for f in fields {
        if let Some(filter) = TextFilter::new(&value, Match::Phrase, schema, f.def) {
            docs |= &crate::filter::text::docs_until(f.fid, &filter, seg, schema, Some(deadline));
        }
    }
    docs
}

/// Search `query` over `segs`.
/// `pick`: per segment (by index) the documents to rank among those found (the rest keep no
/// score: a field sort puts them past the page whatever their match); every one when None, and
/// with group_by, which counts groups as documents are ranked.
pub fn search<'s>(
    fields: &[Field<'s>],
    segs: &[Segment<'s>],
    query: &Query<'_>,
    params: &Params,
    schema: &'s Schema,
    pick: Option<&dyn Fn(usize, &Bitmap) -> Bitmap>,
) -> Result<Outcome, String> {
    let mut search = Search::new(fields, segs, params, schema);
    let pick = pick.filter(|_| segs.iter().all(|s| s.group_of.is_none()));
    if pick.is_some() {
        search.pending = Some(Vec::new());
    }
    let mut marks = Marks::default();
    let n = query.words.len();
    let mut words: Vec<Word> = query.words.iter().enumerate().map(|(i, w)| search.word(w, i + 1 == n, false, false, "exact")).collect();
    for phrase in query.phrases {
        words.extend(phrase.iter().map(|w| search.word(w, false, true, true, "exact")));
    }
    let mut variants = vec![search.variant(words)];
    for rewrite in query.rewrites {
        let words = rewrite.iter().map(|w| search.word(w, false, true, false, "synonym")).collect();
        variants.push(search.variant(words));
    }
    for v in &mut variants {
        for w in 0..v.words.len() {
            search.expand_exact(v, w);
        }
        search.expand_infix(v, Infix::Always)?;
    }
    if params.deadline.passed() {
        search.cutoff = true;
    }

    // every cost at once, whatever is found by the first
    let all_costs = params.exhaustive || fields.iter().any(|f| f.names == NameMatching::Always);
    let typos_on = params.typo_tokens_threshold > 0 || all_costs;
    let top = if typos_on { fields.iter().map(|f| f.num_typos.min(2)).max().unwrap_or(0) as u8 } else { 0 };
    let mut level = 0u8;
    if all_costs {
        while level < top {
            level += 1;
            for v in &mut variants {
                for w in 0..v.words.len() {
                    search.expand_level(v, w, level);
                }
            }
        }
    }
    let every = |v: &Variant| -> Vec<usize> { (0..v.words.len()).collect() };
    for v in &variants {
        search.run(v, &every(v));
    }
    while !search.cutoff && level < top && search.total < params.typo_tokens_threshold {
        level += 1;
        for v in &mut variants {
            for w in 0..v.words.len() {
                search.expand_level(v, w, level);
            }
            search.run(v, &every(v));
        }
    }

    if search.total == 0 && !search.cutoff {
        for v in &mut variants {
            if search.expand_infix(v, Infix::Fallback)? {
                search.run(v, &every(v));
            }
        }
    }

    let split_join = match params.split_join {
        SplitJoin::Off => false,
        SplitJoin::Always => true,
        SplitJoin::Fallback => search.total == 0,
    };
    if split_join && !search.cutoff {
        let mut more = search.split_join(&variants[0]);
        for v in &mut more {
            for w in 0..v.words.len() {
                search.expand_exact(v, w);
                for l in 1..=level {
                    search.expand_level(v, w, l);
                }
            }
            search.run(v, &every(v));
        }
        variants.append(&mut more);
    }

    if params.drop_tokens_threshold > 0 && search.total < params.drop_tokens_threshold && !search.cutoff {
        for list in search.drops(&variants[0]) {
            search.run(&variants[0], &list);
            if search.total >= params.drop_tokens_threshold || search.cutoff {
                break;
            }
        }
    }

    if let (Some(pick), Some(pending)) = (pick, search.pending.take()) {
        let picked: Vec<Bitmap> = search.found.iter().enumerate().map(|(s, found)| pick(s, found)).collect();
        for p in pending {
            let docs = &p.docs & &picked[p.seg];
            if !docs.is_empty() {
                let v = variants.iter().find(|v| v.id == p.variant).expect("a pending round's variant");
                search.rank(v, &p.words, Some(&p.lens), p.seg, &docs);
            }
        }
    }
    if !search.cutoff {
        search.settle(&variants);
    }

    for v in &variants {
        let mut explained = Vec::with_capacity(v.words.len());
        for word in &v.words {
            let chars = word.lowered.chars().count();
            let mut list: Vec<Explained> = Vec::new();
            for (f, candidates) in word.candidates.iter().enumerate() {
                for c in candidates {
                    if !word.required {
                        let mark = if c.completion && c.kind == "prefix" { Mark::Prefix { chars } } else { Mark::Whole };
                        let entry = marks.terms.entry(c.term.clone()).or_insert(mark);
                        if mark == Mark::Whole {
                            *entry = Mark::Whole;
                        }
                    }
                    list.push(Explained { fid: fields[f].fid, term: c.term.clone(), level: c.level, kind: c.kind, encoder: c.encoder });
                }
            }
            list.sort_by_key(|c| c.level);
            if !word.required {
                for term in word.terms.iter().flatten() {
                    marks.terms.insert(term.clone(), Mark::Whole);
                }
            }
            explained.push((word.lowered.clone(), list));
        }
        marks.words.push(explained);
    }
    for phrase in query.phrases {
        for f in fields {
            let terms: Vec<Vec<u8>> = phrase.iter().flat_map(|w| crate::text::field_terms(w, schema, f.def, schema.default_language)).map(String::into_bytes).collect();
            if !marks.phrases.contains(&terms) {
                marks.phrases.push(terms);
            }
        }
    }
    let Search { found, scores, cutoff, .. } = search;
    Ok(Outcome { found, scores, search_cutoff: cutoff, marks: Arc::new(marks) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_move_to_slots_past_the_sparse_limit_in_any_order() {
        let n = (SPARSE_SCORES + 10) as u32;
        // descending: each document below the ones before it, as a fused ranking can give them
        let mut scores = Scores::default();
        for d in (0..n).rev() {
            scores.insert(d * 2, u64::from(d) + 1);
        }
        assert!(!scores.dense.is_empty() && scores.sparse.is_empty());
        for d in 0..n {
            assert_eq!(scores.get(d * 2), u64::from(d) + 1);
        }
        assert_eq!(scores.get(1), 0);
        assert_eq!(scores.len(), n as usize);
        assert_eq!(scores.iter().count(), n as usize);
        // a document past the slots, then past everything asked so far
        scores.insert(n * 3, 7);
        assert_eq!(scores.get(n * 3), 7);
        assert_eq!(scores.get(n * 4), 0);
        scores.clear();
        assert_eq!((scores.len(), scores.get(0)), (0, 0));
    }

    #[test]
    fn few_scores_of_a_large_segment_take_no_slots() {
        let mut scores = Scores::default();
        scores.set(4_000_000, 5, 5_000_000);
        assert!(scores.dense.is_empty());
        assert_eq!(scores.get(4_000_000), 5);
    }
}
