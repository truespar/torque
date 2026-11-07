//! BM25F over the terms a query matched, and the `text_match` a document ranks by.

use serde::Serialize;

const K1: f32 = 1.2;
const B: f32 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchType {
    MaxScore,
    MaxWeight,
    SumScore,
}

impl MatchType {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "max_score" => Ok(Self::MaxScore),
            "max_weight" => Ok(Self::MaxWeight),
            "sum_score" => Ok(Self::SumScore),
            other => Err(format!("Invalid text_match_type: `{other}`. Expected `max_score`, `max_weight`, or `sum_score`.")),
        }
    }

    /// One representative (score, weight) of a document's weighted field scores: the highest, the
    /// heaviest field's, or their sum (with the heaviest weight).
    pub fn combine(self, fields: impl Iterator<Item = (f32, u32)>) -> (f32, u32) {
        match self {
            Self::MaxScore => fields.fold((0.0, 0), |best, f| if f.0 > best.0 { f } else { best }),
            Self::MaxWeight => fields.fold((0.0, 0), |best, f| if f.1 > best.1 || (f.1 == best.1 && f.0 > best.0) { f } else { best }),
            Self::SumScore => fields.fold((0.0, 0), |sum, f| (sum.0 + f.0, sum.1.max(f.1))),
        }
    }
}

/// Inverse document frequency: how rare a term is across the corpus.
pub fn compute_idf(total_docs: u64, doc_freq: u64) -> f32 {
    let n = total_docs as f64;
    let df = doc_freq as f64;
    ((1.0 + (n - df + 0.5) / (df + 0.5)).ln()) as f32
}

/// BM25 score for a single term in a single field.
#[inline]
pub fn compute_bm25(tf: u32, doc_length: u32, avg_doc_length: f32, idf: f32) -> f32 {
    let tf = tf as f32;
    let dl = doc_length as f32;
    idf * (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * dl / avg_doc_length))
}

/// What ranks a text match, most significant first. `text_match` packs it into one integer:
///
/// ```text
/// 63..56  words matched
/// 55..50  63 - typos summed over the words (a phonetic or fold match counts as its cost)
/// 49..44  63 - words matched by completing a prefix or inside a term
/// 43      the words are a field's whole value (prioritize_exact_match)
/// 42..40  fields matched, at most 7 (prioritize_num_matching_fields)
/// 39..7   BM25F score, in 1/100,000
/// 6..0    the weight of the field that gave it
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rank {
    pub words: u32,
    pub typos: u32,
    pub completions: u32,
    pub exact: bool,
    pub fields: u32,
    pub score: f32,
    pub weight: u32,
}

const SCORE_SCALE: f32 = 100_000.0;
const SCORE_MAX: u64 = (1 << 33) - 1;

impl Rank {
    pub fn pack(&self) -> u64 {
        let score = ((self.score * SCORE_SCALE) as u64).min(SCORE_MAX);
        u64::from(self.words.min(255)) << 56
            | u64::from(63 - self.typos.min(63)) << 50
            | u64::from(63 - self.completions.min(63)) << 44
            | u64::from(self.exact) << 43
            | u64::from(self.fields.min(7)) << 40
            | score << 7
            | u64::from(self.weight.min(127))
    }
}

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
/// A word's match, with `explain_matches=true`.
#[derive(Debug, Clone, Serialize)]
pub struct WordMatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<&'static str>,
    pub kind: &'static str,
    pub term: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub typos: u32,
}

// Fields in key order: a search response is written as declared (`tqbf::json::TsJsonInOrder`).
/// The parts of a hit's `text_match`.
#[derive(Debug, Clone, Serialize)]
pub struct TextMatchInfo {
    pub best_field_score: String,
    pub best_field_weight: u32,
    pub fields_matched: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<Vec<WordMatch>>,
    pub num_tokens_dropped: u32,
    pub score: String,
    pub tokens_matched: u32,
    pub typo_prefix_score: u32,
}

impl TextMatchInfo {
    /// `score` a hit's `text_match` (`Rank::pack`), `words` the words of the query.
    pub fn new(score: u64, words: usize) -> Self {
        let tokens_matched = (score >> 56) as u32;
        let typos = 63 - (score >> 50 & 63) as u32;
        let completions = 63 - (score >> 44 & 63) as u32;
        Self {
            best_field_score: (score >> 7 & SCORE_MAX).to_string(),
            best_field_weight: (score & 127) as u32,
            fields_matched: ((score >> 40 & 7) as u32).max(u32::from(tokens_matched > 0)),
            num_tokens_dropped: (words as u32).saturating_sub(tokens_matched),
            score: score.to_string(),
            tokens_matched,
            typo_prefix_score: if tokens_matched > 0 { typos + completions } else { 0 },
            matches: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idf_rare_term() {
        // Term in 1 of 1000 docs → high IDF
        assert!(compute_idf(1000, 1) > 5.0);
    }

    #[test]
    fn idf_common_term() {
        // Term in 900 of 1000 docs → low IDF
        assert!(compute_idf(1000, 900) < 1.0);
    }

    #[test]
    fn idf_all_docs() {
        // Term in every doc → IDF near 0
        assert!(compute_idf(1000, 1000) < 0.1);
    }

    #[test]
    fn bm25_basic() {
        assert!(compute_bm25(1, 100, 100.0, 1.0) > 0.0);
    }

    #[test]
    fn bm25_higher_tf_higher_score() {
        assert!(compute_bm25(5, 100, 100.0, 1.0) > compute_bm25(1, 100, 100.0, 1.0));
    }

    #[test]
    fn bm25_shorter_doc_higher_score() {
        // Same TF, shorter doc → higher score (field length normalization)
        assert!(compute_bm25(2, 10, 100.0, 1.0) > compute_bm25(2, 200, 100.0, 1.0));
    }

    #[test]
    fn text_match_type_parse() {
        assert_eq!(MatchType::parse("max_score").unwrap(), MatchType::MaxScore);
        assert_eq!(MatchType::parse("max_weight").unwrap(), MatchType::MaxWeight);
        assert_eq!(MatchType::parse("sum_score").unwrap(), MatchType::SumScore);
        assert!(MatchType::parse("invalid").is_err());
    }

    #[test]
    fn match_types_pick_their_field() {
        let fields = [(1.0f32, 3u32), (5.0, 2), (0.0, 9)];
        assert_eq!(MatchType::MaxScore.combine(fields.into_iter()), (5.0, 2));
        assert_eq!(MatchType::MaxWeight.combine(fields.into_iter().filter(|f| f.0 > 0.0)), (1.0, 3));
        assert_eq!(MatchType::SumScore.combine(fields.into_iter()), (6.0, 9));
    }

    #[test]
    fn more_words_outrank_fewer_typos_outrank_a_better_score() {
        let high = Rank { words: 1, score: 500.0, ..Default::default() };
        let two = Rank { words: 2, typos: 4, score: 0.1, ..Default::default() };
        assert!(two.pack() > high.pack());
        let typo = Rank { words: 1, typos: 1, score: 900.0, ..Default::default() };
        assert!(high.pack() > typo.pack());
        let completed = Rank { words: 1, completions: 1, score: 900.0, ..Default::default() };
        assert!(high.pack() > completed.pack());
        assert!(completed.pack() > typo.pack());
    }

    #[test]
    fn info_reads_the_rank_back() {
        let r = Rank { words: 2, typos: 1, completions: 1, exact: false, fields: 3, score: 2.5, weight: 15 };
        let info = TextMatchInfo::new(r.pack(), 3);
        assert_eq!((info.tokens_matched, info.num_tokens_dropped, info.fields_matched), (2, 1, 3));
        assert_eq!((info.typo_prefix_score, info.best_field_weight), (2, 15));
        assert_eq!(info.best_field_score, "250000");
        assert_eq!(info.score, r.pack().to_string());
    }
}
