use crate::schema::Language;
use rust_stemmers::{Algorithm, Stemmer};
use std::cell::RefCell;

/// Number of supported languages (must match Language enum variant count).
const NUM_LANGUAGES: usize = 17;

/// Stem a token using the Snowball algorithm for the given language.
/// Stemmer instances are cached per-thread for reuse.
pub fn stem(token: &str, language: Language) -> String {
    STEMMERS.with(|stemmers| {
        let mut stemmers = stemmers.borrow_mut();
        let idx = language as usize;
        let stemmer =
            stemmers[idx].get_or_insert_with(|| Stemmer::create(language_algorithm(language)));
        stemmer.stem(token).into_owned()
    })
}

/// Entries `stem_remembered` keeps per thread before it starts over.
const REMEMBERED: usize = 4096;

thread_local! {
    static REMEMBER: RefCell<(usize, [rustc_hash::FxHashMap<Box<str>, Box<str>>; NUM_LANGUAGES])> = RefCell::new(Default::default());
}

/// `term` stemmed in place, the stems remembered per thread: a stemmed field's filters analyze
/// their value and the field's values in every language, the same ones query after query.
pub fn stem_remembered(term: &mut String, language: Language) {
    REMEMBER.with(|r| {
        let (count, maps) = &mut *r.borrow_mut();
        let map = &mut maps[language as usize];
        if let Some(s) = map.get(term.as_str()) {
            term.clear();
            term.push_str(s);
            return;
        }
        let s = stem(term, language);
        if *count >= REMEMBERED {
            maps.iter_mut().for_each(|m| m.clear());
            *count = 0;
        }
        maps[language as usize].insert(term.as_str().into(), s.as_str().into());
        *count += 1;
        *term = s;
    })
}

thread_local! {
    static STEMMERS: RefCell<[Option<Stemmer>; NUM_LANGUAGES]> =
        const { RefCell::new([
            None, None, None, None, None, None, None, None, None,
            None, None, None, None, None, None, None, None,
        ]) };
}

fn language_algorithm(lang: Language) -> Algorithm {
    match lang {
        Language::English => Algorithm::English,
        Language::Swedish => Algorithm::Swedish,
        Language::Norwegian => Algorithm::Norwegian,
        Language::Danish => Algorithm::Danish,
        Language::Finnish => Algorithm::Finnish,
        Language::German => Algorithm::German,
        Language::French => Algorithm::French,
        Language::Spanish => Algorithm::Spanish,
        Language::Portuguese => Algorithm::Portuguese,
        Language::Italian => Algorithm::Italian,
        Language::Dutch => Algorithm::Dutch,
        Language::Russian => Algorithm::Russian,
        Language::Arabic => Algorithm::Arabic,
        Language::Greek => Algorithm::Greek,
        Language::Hungarian => Algorithm::Hungarian,
        Language::Romanian => Algorithm::Romanian,
        Language::Turkish => Algorithm::Turkish,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_running() {
        assert_eq!(stem("running", Language::English), "run");
    }

    #[test]
    fn english_cats() {
        assert_eq!(stem("cats", Language::English), "cat");
    }

    #[test]
    fn swedish_bilar() {
        assert_eq!(stem("bilar", Language::Swedish), "bil");
    }

    #[test]
    fn norwegian_bilene() {
        assert_eq!(stem("bilene", Language::Norwegian), "bil");
    }

    #[test]
    fn danish_bilerne() {
        assert_eq!(stem("bilerne", Language::Danish), "bil");
    }

    #[test]
    fn finnish_taloissa() {
        assert_eq!(stem("taloissa", Language::Finnish), "talo");
    }

    #[test]
    fn german_hauser() {
        assert_eq!(stem("häuser", Language::German), "haus");
    }

    #[test]
    fn french_maisons() {
        assert_eq!(stem("maisons", Language::French), "maison");
    }

    #[test]
    fn spanish_corriendo() {
        assert_eq!(stem("corriendo", Language::Spanish), "corr");
    }

    #[test]
    fn portuguese_casas() {
        assert_eq!(stem("casas", Language::Portuguese), "cas");
    }

    #[test]
    fn italian_ragazzi() {
        assert_eq!(stem("ragazzi", Language::Italian), "ragazz");
    }

    #[test]
    fn dutch_huizen() {
        assert_eq!(stem("huizen", Language::Dutch), "huiz");
    }

    #[test]
    fn russian_domov() {
        assert_eq!(stem("домов", Language::Russian), "дом");
    }

    #[test]
    fn turkish_evlerin() {
        assert_eq!(stem("evlerin", Language::Turkish), "ev");
    }
}
