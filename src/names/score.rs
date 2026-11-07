//! The name scorer: how well a query name
//! matches a listed name, between 0 and 1, with the evidence.
//!
//! Retrieval finds candidates for recall; the scorer orders them for a screening decision. The
//! approach follows OpenSanctions' logic-v2 matcher (rigour, MIT; see THIRD-PARTY-NOTICES), on
//! Torque's terms:
//!
//! - The parts of both names pair in any order, best pair first; a part may also pair with two or
//!   three neighbouring parts of the other name run together ("alsabah" and "al sabah").
//! - A pair is exact, a passport fold ("oedegaard", "ødegård"), an edit that keeps most of the word
//!   (`Weights::typo_floor`; confusable letters such as o/u and a doubled letter cost less, a
//!   letter added or dropped in front more: Anjou is not Kanjou; never in words of three letters),
//!   a shared sound of words not too far apart in spelling (Double Metaphone or Daitch-Mokotoff,
//!   weak evidence: it never decides alone), or an initial (weak too).
//! - Parts weigh by rarity in the collection, so a name sharing only "Mohammed" scores low;
//!   particles (al, bin, von), honorifics (Dr, Sheikh), single letters and the words that make a
//!   company's name a company's (AB, Ltd, Group, Holding) weigh little.
//! - A query part left unpaired costs more than a listed part left unpaired: a listed name has
//!   middle names and aliases the query does not repeat. But a person's name of one part
//!   identifies no one: then the listed name's other parts count in full ("Andersson" is not
//!   "William Anderson"), where a company's one distinctive word does ("Gazprom", "Gazprom Neft").
//!   The client says which the query is (`NameKind`); the names alone cannot tell.
//!
//! The weights are data (`Weights`), to be fitted to analysts' decisions once there are enough.

use super::{fold, keys, Channel};

/// The scorer's weights.
#[derive(Clone, Debug)]
pub struct Weights {
    /// A pair equal by the passport fold.
    pub fold: f32,
    /// A pair that shares only a sound, and how far apart in spelling the two words may be for it
    /// (edit cost per letter).
    pub phonetic: f32,
    pub phonetic_spread: f32,
    /// The least similarity of a pair by edits (1 - cost / length): below it, two words are
    /// different names (Lind and Lund, Nilsson and Olsson), whatever their sound.
    pub typo_floor: f32,
    /// A one-letter part and a part starting with that letter.
    pub initial: f32,
    /// How much an unpaired query part costs, per its weight.
    pub query_extra: f32,
    /// How much an unpaired listed part costs, per its weight.
    pub name_extra: f32,
    /// The weight of a connecting particle (al, bin, von) and of an honorific (Dr, Sheikh).
    pub particle: f32,
    pub honorific: f32,
    /// The least weight of a part, however common.
    pub rarity_floor: f32,
    /// A substitution of confusable letters (o/u, c/k, …), dropping a doubled letter, and adding or
    /// dropping the first letter.
    pub confusable: f32,
    pub doubled: f32,
    pub first: f32,
}

pub const WEIGHTS: Weights = Weights {
    fold: 0.97,
    phonetic: 0.6,
    phonetic_spread: 0.5,
    typo_floor: 0.8,
    initial: 0.5,
    query_extra: 0.8,
    name_extra: 0.2,
    particle: 0.2,
    honorific: 0.05,
    rarity_floor: 0.2,
    confusable: 0.7,
    doubled: 0.5,
    first: 1.5,
};

/// Letters that transliterations and spellings swap: their substitution costs `Weights::confusable`.
const CONFUSABLE: &[(char, char)] = &[
    ('a', 'e'),
    ('e', 'i'),
    ('i', 'y'),
    ('o', 'u'),
    ('c', 'k'),
    ('k', 'q'),
    ('g', 'q'),
    ('c', 's'),
    ('s', 'z'),
    ('v', 'w'),
    ('f', 'v'),
    ('j', 'y'),
    ('i', 'j'),
];

/// Particles that join a name's parts, and honorifics before it: they stay in the name (dropping
/// them loses matches) but weigh little.
const PARTICLES: &[&str] = &[
    "al", "el", "ul", "bin", "ben", "ibn", "bint", "binti", "ould", "von", "van", "der", "den", "de", "del", "della", "di", "da", "du", "le", "la",
    "af", "av", "zu", "y",
];
const HONORIFICS: &[&str] = &[
    "mr", "mrs", "ms", "miss", "dr", "prof", "sir", "sheikh", "shaikh", "sheik", "haji", "hajji", "sayed", "sayyid", "mullah", "maulana",
];
/// Legal forms and the generic words of company names: they say what a name is, not whose.
const ORG_TERMS: &[&str] = &[
    "ab", "aktiebolag", "hb", "kb", "as", "asa", "oy", "oyj", "aps", "ehf", "ltd", "limited", "llc", "llp", "lp", "inc", "incorporated", "corp",
    "corporation", "co", "company", "companies", "plc", "gmbh", "ag", "kg", "sa", "sas", "sarl", "sl", "srl", "spa", "bv", "nv", "jsc", "ojsc",
    "cjsc", "pjsc", "ooo", "zao", "oao", "pao", "fze", "fzco", "fzc", "dmcc", "pte", "pty", "sdn", "bhd", "cia", "ltda", "cv", "de", "group",
    "holding", "holdings", "trading", "international", "enterprise", "enterprises", "industries", "industry", "services", "service",
    "invest", "investment", "investments", "capital", "fund", "management", "financial", "finance", "consulting", "consultancy", "solutions",
    "technology", "technologies", "partners", "global", "sweden", "svenska", "sverige", "nordic", "import", "export", "logistics",
    "shipping",
];

/// What the query names, when the client says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NameKind {
    #[default]
    Unknown,
    Person,
    Organization,
}

impl NameKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "person" => Some(NameKind::Person),
            "organization" => Some(NameKind::Organization),
            _ => None,
        }
    }
}

/// A part of a name: a word as the field's tokenizer gives it, with its weight.
#[derive(Clone, Debug)]
pub struct Part {
    pub word: Vec<u8>,
    folded: Vec<char>,
    pub weight: f32,
}

impl Part {
    /// `rarity`: the word's weight by how common it is (`rarity`), before particles.
    pub fn new(word: Vec<u8>, rarity: f32, w: &Weights) -> Self {
        let text = String::from_utf8_lossy(&word);
        let weight = if HONORIFICS.contains(&text.as_ref()) {
            w.honorific
        } else if PARTICLES.contains(&text.as_ref()) || ORG_TERMS.contains(&text.as_ref()) || text.chars().count() == 1 {
            w.particle.min(rarity)
        } else {
            rarity
        };
        Part { folded: fold(&text).chars().collect(), word, weight }
    }
}

/// A word's weight by its document frequency `df` among `docs` documents: 1 for a word the
/// collection lacks, down to `floor` for the most common.
pub fn rarity(df: u32, docs: u32, floor: f32) -> f32 {
    let docs = docs as f64 + 1.0;
    let r = (docs / (df as f64 + 1.0)).ln() / docs.ln().max(f64::MIN_POSITIVE);
    (r as f32).clamp(floor, 1.0)
}

/// How a pair matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Exact,
    Fold,
    Typo,
    Phonetic(Channel),
    Initial,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Exact => "exact",
            Kind::Fold => "fold",
            Kind::Typo => "typo",
            Kind::Phonetic(_) => "phonetic",
            Kind::Initial => "initial",
        }
    }

    pub fn encoder(self) -> Option<&'static str> {
        match self {
            Kind::Phonetic(c) => Some(c.name()),
            _ => None,
        }
    }
}

/// A pair of the query's parts `query` with the name's parts `name` (each one part, or two or three
/// run together).
#[derive(Clone, Debug, PartialEq)]
pub struct Pair {
    pub query: std::ops::Range<usize>,
    pub name: std::ops::Range<usize>,
    pub kind: Kind,
    pub similarity: f32,
}

/// A name's score and its evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct Scored {
    pub score: f32,
    pub pairs: Vec<Pair>,
    /// The query's parts and the name's parts left unpaired.
    pub unmatched_query: Vec<usize>,
    pub unmatched_name: Vec<usize>,
}

/// At most this many parts of a name are compared (yente's budget): the rest count as unpaired.
pub const MAX_PARTS: usize = 16;
/// The most parts run together to pair with one.
const MAX_JOIN: usize = 3;

/// A run of `len` parts from `start`, run together.
struct Span {
    start: usize,
    len: usize,
    word: Vec<u8>,
    folded: Vec<char>,
    weight: f32,
}

fn spans(parts: &[Part]) -> Vec<Span> {
    let n = parts.len().min(MAX_PARTS);
    let mut out = Vec::with_capacity(n * MAX_JOIN);
    for start in 0..n {
        for len in 1..=MAX_JOIN.min(n - start) {
            let run = &parts[start..start + len];
            out.push(Span {
                start,
                len,
                word: run.iter().flat_map(|p| p.word.iter().copied()).collect(),
                folded: run.iter().flat_map(|p| p.folded.iter().copied()).collect(),
                weight: run.iter().map(|p| p.weight).fold(0.0, f32::max),
            });
        }
    }
    out
}

/// The weighted edit distance of `a` and `b` (restricted Damerau-Levenshtein: transpositions of
/// neighbours), or None when it exceeds `limit`.
fn distance(a: &[char], b: &[char], limit: f32, w: &Weights) -> Option<f32> {
    if (a.len() as f32 - b.len() as f32).abs() * w.doubled > limit {
        return None;
    }
    let sub = |x: char, y: char| {
        if x == y {
            0.0
        } else if CONFUSABLE.iter().any(|&(p, q)| (p, q) == (x, y) || (q, p) == (x, y)) {
            w.confusable
        } else {
            1.0
        }
    };
    // dropping a letter next to the same letter ("mm" → "m") costs less
    let del = |s: &[char], i: usize| if (i > 0 && s[i - 1] == s[i]) || (i + 1 < s.len() && s[i + 1] == s[i]) { w.doubled } else { 1.0 };
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0f32; m + 1]; n + 1];
    // a letter added or dropped in front makes another name (Anjou and Kanjou, Ryden and Dryden)
    for i in 1..=n {
        d[i][0] = d[i - 1][0] + if i == 1 { w.first } else { del(a, i - 1) };
    }
    for j in 1..=m {
        d[0][j] = d[0][j - 1] + if j == 1 { w.first } else { del(b, j - 1) };
    }
    for i in 1..=n {
        let mut row_min = f32::MAX;
        for j in 1..=m {
            let mut v = (d[i - 1][j] + del(a, i - 1)).min(d[i][j - 1] + del(b, j - 1)).min(d[i - 1][j - 1] + sub(a[i - 1], b[j - 1]));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] && a[i - 1] != a[i - 2] {
                v = v.min(d[i - 2][j - 2] + 1.0);
            }
            d[i][j] = v;
            row_min = row_min.min(v);
        }
        if row_min > limit {
            return None;
        }
    }
    (d[n][m] <= limit).then_some(d[n][m])
}

/// How `q` and `n` pair, the best way they do, or None.
fn compare(q: &Span, n: &Span, w: &Weights, buf: (&mut Vec<Vec<u8>>, &mut Vec<Vec<u8>>)) -> Option<(Kind, f32)> {
    if q.word == n.word {
        return Some((Kind::Exact, 1.0));
    }
    if q.folded == n.folded {
        return Some((Kind::Fold, w.fold));
    }
    // the edit budget: none up to three letters (short names merge too easily), then what keeps
    // the similarity above the floor
    let len = q.folded.len().max(n.folded.len());
    let budget = if len <= 3 { 0.0 } else { len as f32 * (1.0 - w.typo_floor) };
    if let Some(d) = distance(&q.folded, &n.folded, budget, w) {
        return Some((Kind::Typo, 1.0 - d / len as f32));
    }
    // a shared sound: weak evidence, for spellings beyond the edit budget but not too far from each
    // other (codes collide: Captor and Jupiter)
    let sound = [Channel::DoubleMetaphone, Channel::DaitchMokotoff].into_iter().find(|&c| {
        keys(&q.word, c, buf.0);
        keys(&n.word, c, buf.1);
        buf.0.iter().any(|k| buf.1.contains(k))
    });
    let sound = sound.filter(|_| distance(&q.folded, &n.folded, len as f32 * w.phonetic_spread, w).is_some());
    match sound {
        Some(c) => Some((Kind::Phonetic(c), w.phonetic)),
        None if q.len == 1 && n.len == 1 && (q.folded.len() == 1 || n.folded.len() == 1) && q.folded.first() == n.folded.first() => Some((Kind::Initial, w.initial)),
        None => None,
    }
}

/// How well `query` (a name of `kind`) matches `name`: their parts paired best pair first, each pair
/// weighing as the name's parts it covers, the unpaired parts of either side counting against.
pub fn score(query: &[Part], name: &[Part], kind: NameKind, w: &Weights) -> Scored {
    let (qs, ns) = (spans(query), spans(name));
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut options: Vec<(f32, usize, usize, Kind, f32, f32)> = Vec::new();
    for (i, q) in qs.iter().enumerate() {
        for (j, n) in ns.iter().enumerate() {
            // a run pairs with one part, not with another run
            if q.len > 1 && n.len > 1 {
                continue;
            }
            if let Some((kind, similarity)) = compare(q, n, w, (&mut a, &mut b)) {
                // an initial weighs as the word it stands for, whichever side has it
                let weight = if kind == Kind::Initial { n.weight.max(q.weight) } else { n.weight };
                options.push((similarity * weight, i, j, kind, similarity, weight));
            }
        }
    }
    // the best pair first; at equal gain the one of fewer parts, then the earlier
    options.sort_by(|x, y| {
        y.0.total_cmp(&x.0)
            .then((qs[x.1].len + ns[x.2].len).cmp(&(qs[y.1].len + ns[y.2].len)))
            .then((x.1, x.2).cmp(&(y.1, y.2)))
    });
    let (mut q_used, mut n_used) = (vec![false; query.len()], vec![false; name.len()]);
    let mut pairs = Vec::new();
    let (mut gained, mut weighed) = (0f32, 0f32);
    for (gain, i, j, kind, similarity, weight) in options {
        let (q, n) = (&qs[i], &ns[j]);
        if q_used[q.start..q.start + q.len].iter().any(|&u| u) || n_used[n.start..n.start + n.len].iter().any(|&u| u) {
            continue;
        }
        q_used[q.start..q.start + q.len].iter_mut().for_each(|u| *u = true);
        n_used[n.start..n.start + n.len].iter_mut().for_each(|u| *u = true);
        gained += gain;
        weighed += weight;
        pairs.push(Pair { query: q.start..q.start + q.len, name: n.start..n.start + n.len, kind, similarity });
    }
    let unmatched_query: Vec<usize> = (0..query.len()).filter(|&i| !q_used[i]).collect();
    let unmatched_name: Vec<usize> = (0..name.len()).filter(|&i| !n_used[i]).collect();
    weighed += w.query_extra * unmatched_query.iter().map(|&i| query[i].weight).sum::<f32>();
    // a person's name of one part (besides particles and the like) says nothing of the listed
    // name's other parts
    let single = kind == NameKind::Person && query.iter().filter(|p| p.weight > w.particle).count() <= 1;
    weighed += if single { 1.0 } else { w.name_extra } * unmatched_name.iter().map(|&i| name[i].weight).sum::<f32>();
    let score = if weighed > 0.0 { (gained / weighed).clamp(0.0, 1.0) } else { 0.0 };
    pairs.sort_by_key(|p| p.query.start);
    Scored { score, pairs, unmatched_query, unmatched_name }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(name: &str) -> Vec<Part> {
        name.split_whitespace().map(|p| Part::new(p.to_lowercase().into_bytes(), 1.0, &WEIGHTS)).collect()
    }

    fn s(q: &str, n: &str) -> f32 {
        score(&parts(q), &parts(n), NameKind::Unknown, &WEIGHTS).score
    }

    fn person(q: &str, n: &str) -> f32 {
        score(&parts(q), &parts(n), NameKind::Person, &WEIGHTS).score
    }

    #[test]
    fn order_free_and_exact() {
        assert_eq!(s("anna karlsson", "anna karlsson"), 1.0);
        assert_eq!(s("karlsson anna", "anna karlsson"), 1.0);
        assert!(s("anna karlsson", "anna maria karlsson") > 0.9, "a listed middle name costs little");
        assert!(s("anna maria karlsson", "anna karlsson") < 0.8, "a query part the list lacks costs more");
    }

    #[test]
    fn spellings_score_by_kind() {
        // fold
        assert!((s("oedegaard", "ødegård".replace('ø', "o").replace('å', "a").as_str()) - WEIGHTS.fold).abs() < 1e-6);
        // edits: confusable letters and doubled letters cost less
        assert!(s("karlsson", "carlsson") > 0.9);
        assert!(s("hasan", "hassan") > s("hasan", "hasin"));
        // three edits apart: by sound only ("mohammed" and "muhamad")
        let m = s("mohammed ali", "muhamad ali");
        assert!((m - (WEIGHTS.phonetic + 1.0) / 2.0).abs() < 1e-6, "{m}");
        // different names, though one or two letters apart
        for (a, b) in [("lind", "lund"), ("berg", "borg"), ("nilsson", "olsson"), ("rahimi", "qasimi")] {
            assert!(s(a, b) <= WEIGHTS.phonetic + 1e-6, "{a} {b}: {}", s(a, b));
        }
        // short words pair only exactly or by sound
        assert!(s("ali", "aly") <= WEIGHTS.phonetic + 1e-6);
        assert_eq!(s("lee", "lie"), 0.0);
    }

    #[test]
    fn sound_alone_does_not_decide() {
        // Hassan and Hussein sound alike: as one-word names they stay below a screening threshold
        let h = s("hassan", "hussein");
        assert!(h > 0.0 && h <= WEIGHTS.phonetic + 1e-6, "{h}");
        let r = score(&parts("hassan"), &parts("hussein"), NameKind::Unknown, &WEIGHTS);
        assert!(matches!(r.pairs[0].kind, Kind::Phonetic(_)));
    }

    #[test]
    fn joined_parts_pair() {
        assert!(s("alsabah", "al sabah") > 0.95);
        assert!(s("abdulrahman", "abdul rahman") > 0.95);
        let r = score(&parts("abdulrahman ali"), &parts("abdul rahman ali"), NameKind::Unknown, &WEIGHTS);
        assert_eq!(r.pairs[0].name, 0..2);
        assert!(r.unmatched_name.is_empty());
    }

    #[test]
    fn common_words_weigh_less() {
        let common = |w: &str| Part::new(w.as_bytes().to_vec(), rarity(3000, 128_000, WEIGHTS.rarity_floor), &WEIGHTS);
        let rare = |w: &str| Part::new(w.as_bytes().to_vec(), rarity(3, 128_000, WEIGHTS.rarity_floor), &WEIGHTS);
        // sharing only the common first name
        let only_common = score(&[common("mohammed"), rare("rahimi")], &[common("mohammed"), rare("qasimi")], NameKind::Unknown, &WEIGHTS).score;
        // sharing only the rare family name
        let only_rare = score(&[common("mohammed"), rare("rahimi")], &[common("ahmad"), rare("rahimi")], NameKind::Unknown, &WEIGHTS).score;
        assert!(only_rare > only_common + 0.2, "{only_rare} {only_common}");
        assert_eq!(rarity(0, 100, 0.2), 1.0);
        assert!(rarity(100, 100, 0.2) == 0.2);
    }

    #[test]
    fn particles_and_honorifics_weigh_little() {
        assert!(s("sheikh osama bin laden", "osama laden") > 0.9);
        assert!(s("osama bin laden", "osama laden") > 0.9);
    }

    #[test]
    fn initials() {
        let r = score(&parts("m babaie"), &parts("mohammad babaie"), NameKind::Unknown, &WEIGHTS);
        assert_eq!(r.pairs.iter().map(|p| p.kind).collect::<Vec<_>>(), [Kind::Initial, Kind::Exact]);
    }

    #[test]
    fn one_part_of_a_persons_name_identifies_no_one() {
        // found from the replay of stored screenings: one family name against a listed full name
        assert!(person("andersson", "william anderson") < 0.6, "{}", person("andersson", "william anderson"));
        assert!(person("zhang", "yu zhang") <= 0.5);
        assert_eq!(person("hezbollah", "hezbollah"), 1.0, "a one-part listed name is matched in full");
        assert!(person("olsson", "olsson capital") > 0.8, "a company word is not another name part");
        assert!(person("yu zhang", "zhang yu") == 1.0);
        // a company's one distinctive word does identify it, and so does a name of unknown kind
        let org = |q: &str, n: &str| score(&parts(q), &parts(n), NameKind::Organization, &WEIGHTS).score;
        assert!(org("gazprom", "gazprom neft") > 0.8);
        assert!(org("bmc holding", "ojsc bsw management company of bmc holding") > 0.6);
        assert!(s("gazprom", "gazprom neft") > 0.8);
    }

    #[test]
    fn letters_in_front_make_another_name() {
        for (a, b) in [("anjou", "kanjou"), ("ryden", "dryden"), ("rolin", "brolin")] {
            assert!(s(a, b) <= WEIGHTS.phonetic + 1e-6, "{a} {b}: {}", s(a, b));
        }
        assert!(s("karlsson", "carlsson") > 0.9, "a letter changed in front is a spelling");
    }

    #[test]
    fn sounds_need_spellings_close_enough() {
        for (a, b) in [("captor", "jupiter"), ("pool", "blue"), ("chimes", "komz"), ("reunify", "renova")] {
            assert_eq!(s(a, b), 0.0, "{a} {b}");
        }
    }

    #[test]
    fn single_letters_and_company_words_weigh_little() {
        assert!(s("s e", "terra minas e inversiones del pacifico s a") < 0.5);
        let r = score(&parts("m o restauranger"), &parts("doctor m r o"), NameKind::Unknown, &WEIGHTS);
        assert!(r.score < 0.7, "{}", r.score);
        assert!(s("captor fund management", "jupiter fund management") < 0.5);
    }

    #[test]
    fn nothing_in_common() {
        assert_eq!(s("anna karlsson", "per nilsson"), 0.0);
        assert_eq!(s("", "per nilsson"), 0.0);
        let r = score(&parts("anna"), &parts("per nilsson"), NameKind::Unknown, &WEIGHTS);
        assert_eq!((r.unmatched_query, r.unmatched_name), (vec![0], vec![0, 1]));
    }

    #[test]
    fn long_names_stay_bounded() {
        let long = (0..40).map(|i| format!("part{i}")).collect::<Vec<_>>().join(" ");
        let r = score(&parts(&long), &parts(&long), NameKind::Unknown, &WEIGHTS);
        assert_eq!(r.pairs.len(), MAX_PARTS);
        assert!(r.score < 1.0, "parts past the budget count as unpaired");
    }
}
