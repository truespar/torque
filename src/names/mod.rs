//! Name matching: the keys by which a name-matching field's words are
//! found besides themselves, and the KEYS section that holds them.
//!
//! A channel maps a word to keys; two words that share a key are candidates for each other. A
//! name-matching field does not stem, so its terms are its words (folded by the analyzer) and the
//! keys are computed from the terms: when a segment's TERMS are encoded, and from the merged TERMS
//! when segments merge. KEYS is a function of TERMS, the same whatever built the segment.
//!
//! In a search a channel's candidates come at its cost: a fold at cost 1, like one typo, a phonetic
//! key at cost 2, like two. They are terms of the field, so scoring and highlighting take them as
//! they take typo candidates.
//!
//! KEYS (all integers little-endian):
//!
//! ```text
//! u32 channel count, u32 0, per channel: u32 id, u32 0, u64 offset, u64 length (from the
//! section's start), then per channel an FST set of `key 0x01 term`, on an 8-byte boundary
//! ```

pub mod score;

use crate::tqbf::bytes::Bytes;
use fst::{IntoStreamer, Streamer};
use std::io;
use std::sync::OnceLock;

/// How a word is keyed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Passport spellings of Nordic and German letters (`fold`).
    Fold,
    /// Double Metaphone, primary and alternate code, at most 6 characters.
    DoubleMetaphone,
    /// Daitch-Mokotoff Soundex, every branch.
    DaitchMokotoff,
}

impl Channel {
    pub const ALL: [Channel; 3] = [Channel::Fold, Channel::DoubleMetaphone, Channel::DaitchMokotoff];

    fn id(self) -> u32 {
        match self {
            Channel::Fold => 1,
            Channel::DoubleMetaphone => 2,
            Channel::DaitchMokotoff => 3,
        }
    }

    /// What a match by the channel costs, in typos.
    pub fn cost(self) -> i32 {
        match self {
            Channel::Fold => 1,
            Channel::DoubleMetaphone | Channel::DaitchMokotoff => 2,
        }
    }

    /// `fold`, or the phonetic encoder's name.
    pub fn name(self) -> &'static str {
        match self {
            Channel::Fold => "fold",
            Channel::DoubleMetaphone => "double_metaphone",
            Channel::DaitchMokotoff => "daitch_mokotoff",
        }
    }
}

/// Between a key and its term in a channel's set: below every byte of a key and of a term (the
/// analyzer keeps no control character).
const SEP: u8 = 0x01;

/// The passport (ICAO 9303) spellings of Nordic and German letters as the plain letter: aa, ae, oe
/// and ue as a, a, o and u ("oedegaard" → "odegard", "mueller" → "muller"). The analyzer has
/// already folded å, ä, æ, ö, ø and ü into a, a, ae, o, o and u.
pub fn fold(word: &str) -> String {
    let b = word.as_bytes();
    let mut out = String::with_capacity(word.len());
    let mut i = 0;
    while i < b.len() {
        let pair = if i + 1 < b.len() { &b[i..i + 2] } else { &b[i..i + 1] };
        let plain = match pair {
            b"aa" | b"ae" => Some('a'),
            b"oe" => Some('o'),
            b"ue" => Some('u'),
            _ => None,
        };
        match plain {
            Some(c) => {
                out.push(c);
                i += 2;
            }
            None => {
                let c = word[i..].chars().next().expect("a char at a char boundary");
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

fn double_metaphone() -> &'static rphonetic::DoubleMetaphone {
    static ENCODER: OnceLock<rphonetic::DoubleMetaphone> = OnceLock::new();
    ENCODER.get_or_init(|| rphonetic::DoubleMetaphone::new(Some(6)))
}

fn daitch_mokotoff() -> &'static rphonetic::DaitchMokotoffSoundex {
    static ENCODER: OnceLock<rphonetic::DaitchMokotoffSoundex> = OnceLock::new();
    ENCODER.get_or_init(rphonetic::DaitchMokotoffSoundex::default)
}

/// The keys of `word` by `channel` into `out` (cleared first): a term of a name-matching field, or a
/// query word tokenized the same way. The phonetic encoders key words of ASCII letters only: they
/// slice by byte and panic inside a multi-byte character ("aɴ"), and a panic aborts the server (the
/// analyzer has already folded every letter it can); a word with digits ("3m") is no name to sound
/// out.
pub fn keys(word: &[u8], channel: Channel, out: &mut Vec<Vec<u8>>) {
    out.clear();
    let Ok(word) = std::str::from_utf8(word) else { return };
    let mut add = |code: String| {
        if !code.is_empty() && !out.iter().any(|c| c.as_slice() == code.as_bytes()) {
            out.push(code.into_bytes());
        }
    };
    match channel {
        Channel::Fold => add(fold(word)),
        _ if word.is_empty() || !word.bytes().all(|b| b.is_ascii_alphabetic()) => {}
        Channel::DoubleMetaphone => {
            let r = double_metaphone().double_metaphone(word);
            // a code of one sound (M for "m", "ma", "mae") would match every short word
            for code in [r.primary(), r.alternate()] {
                if code.len() >= 2 {
                    add(code);
                }
            }
        }
        Channel::DaitchMokotoff => {
            for code in daitch_mokotoff().inner_soundex(word, true) {
                // a code of fewer than two sounds (Lee 800000, Xi 500000) would match everything
                if code.bytes().filter(|&b| b != b'0').count() >= 2 {
                    add(code);
                }
            }
        }
    }
}

/// The channel of cost `cost` by which `word` and `term` share a key (None: none).
pub fn channel_of(word: &[u8], term: &[u8], cost: i32) -> Option<Channel> {
    let (mut a, mut b) = (Vec::new(), Vec::new());
    Channel::ALL.into_iter().filter(|c| c.cost() == cost).find(|&c| {
        keys(word, c, &mut a);
        keys(term, c, &mut b);
        a.iter().any(|k| b.contains(k))
    })
}

/// A KEYS section, read in place.
#[derive(Clone)]
pub struct Keys {
    bytes: Bytes,
    sets: Vec<(Channel, fst::Set<Bytes>)>,
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

impl Keys {
    /// The section for `terms`, a name-matching field's TERMS.
    pub fn encode<D: AsRef<[u8]>>(terms: &fst::Map<D>) -> io::Result<Vec<u8>> {
        let mut entries: Vec<Vec<Vec<u8>>> = vec![Vec::new(); Channel::ALL.len()];
        let mut buf = Vec::new();
        let mut stream = terms.stream();
        while let Some((term, _)) = stream.next() {
            for (c, &channel) in Channel::ALL.iter().enumerate() {
                keys(term, channel, &mut buf);
                for key in &buf {
                    let mut entry = Vec::with_capacity(key.len() + 1 + term.len());
                    entry.extend_from_slice(key);
                    entry.push(SEP);
                    entry.extend_from_slice(term);
                    entries[c].push(entry);
                }
            }
        }

        let n = Channel::ALL.len();
        let mut out = vec![0u8; 8 + 24 * n];
        out[..4].copy_from_slice(&(n as u32).to_le_bytes());
        for (c, mut list) in entries.into_iter().enumerate() {
            list.sort_unstable();
            list.dedup();
            let mut set = fst::SetBuilder::memory();
            for entry in &list {
                set.insert(entry).map_err(|e| invalid(e.to_string()))?;
            }
            let set = set.into_inner().map_err(|e| invalid(e.to_string()))?;
            out.resize(out.len().next_multiple_of(8), 0);
            let at = 8 + 24 * c;
            out[at..at + 4].copy_from_slice(&Channel::ALL[c].id().to_le_bytes());
            let offset = out.len() as u64;
            out[at + 8..at + 16].copy_from_slice(&offset.to_le_bytes());
            out[at + 16..at + 24].copy_from_slice(&(set.len() as u64).to_le_bytes());
            out.extend_from_slice(&set);
        }
        Ok(out)
    }

    pub fn open(bytes: Bytes) -> io::Result<Self> {
        if bytes.len() < 8 {
            return Err(invalid("KEYS shorter than its header"));
        }
        let n = bytes.u32_at(0) as usize;
        if 8 + 24 * n > bytes.len() {
            return Err(invalid("KEYS channel table past end"));
        }
        let mut sets = Vec::with_capacity(n);
        for c in 0..n {
            let at = 8 + 24 * c;
            let id = bytes.u32_at(at);
            let (offset, len) = (bytes.u64_at(at + 8) as usize, bytes.u64_at(at + 16) as usize);
            if offset.checked_add(len).is_none_or(|end| end > bytes.len()) {
                return Err(invalid(format!("KEYS channel {id} past end")));
            }
            // a channel this server does not know is skipped, not refused
            if let Some(channel) = Channel::ALL.into_iter().find(|ch| ch.id() == id) {
                let set = fst::Set::new(bytes.slice(offset, len)).map_err(|e| invalid(e.to_string()))?;
                sets.push((channel, set));
            }
        }
        Ok(Self { bytes, sets })
    }

    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    /// Each term keyed `key` by `channel`.
    pub fn terms_of(&self, channel: Channel, key: &[u8], mut f: impl FnMut(&[u8])) {
        let Some((_, set)) = self.sets.iter().find(|(c, _)| *c == channel) else { return };
        let mut from = key.to_vec();
        from.push(SEP);
        let mut to = key.to_vec();
        to.push(SEP + 1);
        let mut stream = set.range().ge(&from).lt(&to).into_stream();
        while let Some(entry) = stream.next() {
            f(&entry[from.len()..]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyed(word: &str, channel: Channel) -> Vec<String> {
        let mut out = Vec::new();
        keys(word.as_bytes(), channel, &mut out);
        out.into_iter().map(|k| String::from_utf8(k).unwrap()).collect()
    }

    fn shares(a: &str, b: &str, channel: Channel) -> bool {
        let kb = keyed(b, channel);
        keyed(a, channel).iter().any(|k| kb.contains(k))
    }

    #[test]
    fn fold_reads_passport_spellings() {
        assert_eq!(fold("oedegaard"), "odegard");
        assert_eq!(fold("mueller"), "muller");
        assert_eq!(fold("aake"), "ake");
        assert_eq!(fold("bjoern"), "bjorn");
        assert_eq!(fold("karlsson"), "karlsson");
        assert_eq!(fold("шипулин"), "шипулин");
    }

    #[test]
    fn phonetic_keys_join_spelling_variants() {
        for (a, b) in [("mohammed", "muhammad"), ("qaddafi", "gaddafi"), ("carlsson", "karlsson"), ("osama", "usama"), ("philip", "filip")] {
            assert!(
                shares(a, b, Channel::DoubleMetaphone) || shares(a, b, Channel::DaitchMokotoff),
                "{a} / {b}: {:?} {:?} / {:?} {:?}",
                keyed(a, Channel::DoubleMetaphone),
                keyed(a, Channel::DaitchMokotoff),
                keyed(b, Channel::DoubleMetaphone),
                keyed(b, Channel::DaitchMokotoff)
            );
        }
        // Latin letters only, and no near-empty Daitch-Mokotoff codes
        assert!(keyed("шипулин", Channel::DoubleMetaphone).is_empty());
        assert!(keyed("lee", Channel::DaitchMokotoff).is_empty());
        // nor one-sound Double Metaphone codes, nor words with digits (a UK entry's "3M" found every
        // short M word on the UN list instead of the entry)
        assert!(keyed("mae", Channel::DoubleMetaphone).is_empty());
        for channel in [Channel::DoubleMetaphone, Channel::DaitchMokotoff] {
            assert!(keyed("3m", channel).is_empty());
            assert!(keyed("m3ller", channel).is_empty());
        }
        // a letter the tokenizer cannot transliterate, among Latin ones (a sanctions list's "ɴ")
        for channel in Channel::ALL {
            keyed("aɴ", channel);
            keyed("ᴀʟɪ", channel);
        }
    }

    /// Any word a field or a query can hold: keying it must not panic (a panic aborts the server).
    #[test]
    fn keys_never_panic() {
        // half ASCII only, which reaches the phonetic encoders, half with letters that do not
        let ascii: Vec<char> = "abcdefghijklmnopqrstuvwxyzaeiouyhwcsz0123456789-_'.&".chars().collect();
        let mixed: Vec<char> = "abcdefghijklmnopqrstuvwxyz0123456789-_'.&ɴøæåßшλ\u{301}".chars().collect();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut out = Vec::new();
        for i in 0..200_000 {
            let alphabet = if i % 2 == 0 { &ascii } else { &mixed };
            let len = (next() % 24) as usize;
            let word: String = (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
            for channel in Channel::ALL {
                keys(word.as_bytes(), channel, &mut out);
            }
        }
        for word in ["", "a", "h", "w", "x", "ch", "sch", "tsch", "ghgh", "cia", "wicz", "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz", "aaaaaaaaaaaaaaaaaa", "'", "-"] {
            for channel in Channel::ALL {
                keys(word.as_bytes(), channel, &mut out);
            }
        }
        // a query of letter groups with two sounds each: Daitch-Mokotoff branches stay few (codes
        // stop at six digits), so keying it stays quick
        let start = std::time::Instant::now();
        for group in ["ch", "ck", "rz", "cz", "tch", "j"] {
            let word = group.repeat(2_000);
            keys(word.as_bytes(), Channel::DaitchMokotoff, &mut out);
            assert!(out.len() <= 64, "{group}: {} codes", out.len());
            keys(word.as_bytes(), Channel::DoubleMetaphone, &mut out);
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
    }

    #[test]
    fn section_round_trip() {
        let mut b = fst::MapBuilder::memory();
        for (i, t) in ["karlsson", "muhammad", "odegard", "zzz"].iter().enumerate() {
            b.insert(t, i as u64).unwrap();
        }
        let terms = fst::Map::new(b.into_inner().unwrap()).unwrap();
        let keys = Keys::open(Bytes::from_vec(Keys::encode(&terms).unwrap())).unwrap();
        let found = |word: &str, channel: Channel| {
            let mut ks = Vec::new();
            super::keys(word.as_bytes(), channel, &mut ks);
            let mut out = Vec::new();
            for k in &ks {
                keys.terms_of(channel, k, |t| out.push(String::from_utf8(t.to_vec()).unwrap()));
            }
            out
        };
        assert_eq!(found("oedegaard", Channel::Fold), ["odegard"]);
        assert!(found("carlsson", Channel::DoubleMetaphone).contains(&"karlsson".to_string()));
        assert!(found("mohammed", Channel::DaitchMokotoff).contains(&"muhammad".to_string()));
        assert!(found("nobody", Channel::Fold).is_empty());
        assert_eq!(channel_of(b"oedegaard", b"odegard", 1), Some(Channel::Fold));
        assert_eq!(channel_of(b"carlsson", b"karlsson", 2).map(Channel::cost), Some(2));
        assert_eq!(channel_of(b"carlsson", b"karlsson", 1), None);
    }
}
