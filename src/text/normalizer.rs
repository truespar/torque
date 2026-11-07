/// Normalize a token: lowercase + unicode folding, without the characters that are not part of a
/// word.
///
/// ASCII tokens get a fast lowercase-only path. Non-ASCII letters are lowercased then folded via
/// `deunicode` (e.g. ä→a, ö→o, ø→o, ш→sh); a letter it cannot fold stays as it is.
pub fn normalize(token: &str) -> String {
    let (mut lowered, mut folded) = (String::new(), String::new());
    normalize_into(token, &[], &mut lowered, &mut folded);
    folded
}

/// Lowercase a token without accent folding.
/// Used for prefix search where accented characters must be preserved
/// (e.g. "Trä" → "trä", NOT "tra").
pub fn lowercase_only(token: &str) -> String {
    let (mut lowered, mut folded) = (String::new(), String::new());
    normalize_into(token, &[], &mut lowered, &mut folded);
    lowered
}

/// A token's lower-cased form with its accents, and its folded form, into reusable buffers. Both
/// keep letters, digits and `symbols` only.
pub fn normalize_into(token: &str, symbols: &[char], lowered: &mut String, folded: &mut String) {
    lowered.clear();
    folded.clear();
    let lower;
    // whole, for the mappings that depend on the context (final sigma)
    let text = if token.is_ascii() {
        token
    } else {
        lower = token.to_lowercase();
        &lower
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            let c = c.to_ascii_lowercase();
            lowered.push(c);
            folded.push(c);
        } else if symbols.contains(&c) {
            lowered.push(c);
            folded.push(c);
        } else if c.is_alphanumeric() {
            lowered.push(c);
            match deunicode::deunicode_char(c) {
                Some(ascii) => folded.extend(ascii.chars().filter(char::is_ascii_alphanumeric).map(|a| a.to_ascii_lowercase())),
                None => folded.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercase_ascii() {
        assert_eq!(normalize("Hello"), "hello");
    }

    #[test]
    fn all_caps() {
        assert_eq!(normalize("WORLD"), "world");
    }

    #[test]
    fn cafe_accent() {
        assert_eq!(normalize("café"), "cafe");
    }

    #[test]
    fn swedish_strom() {
        assert_eq!(normalize("Ström"), "strom");
    }

    #[test]
    fn naive_diaeresis() {
        assert_eq!(normalize("naïve"), "naive");
    }

    #[test]
    fn angstrom() {
        assert_eq!(normalize("Ångström"), "angstrom");
    }

    #[test]
    fn nordic_malmo() {
        assert_eq!(normalize("Malmö"), "malmo");
    }

    #[test]
    fn nordic_tromso() {
        assert_eq!(normalize("Tromsø"), "tromso");
    }

    #[test]
    fn already_ascii() {
        assert_eq!(normalize("hello"), "hello");
    }

    #[test]
    fn numbers_unchanged() {
        assert_eq!(normalize("42"), "42");
    }

    #[test]
    fn every_script_folds_its_case() {
        assert_eq!(normalize("ШИПУЛИН"), normalize("шипулин"));
        assert_eq!(normalize("ΛΑΣ"), normalize("Λας"));
    }

    #[test]
    fn punctuation_inside_a_word_is_dropped() {
        assert_eq!(normalize("non-stick"), "nonstick");
        assert_eq!(normalize("556000-1234"), "5560001234");
        assert_eq!(normalize("19.99"), "1999");
    }

    #[test]
    fn indexed_symbols_stay() {
        let (mut lowered, mut folded) = (String::new(), String::new());
        normalize_into("C++", &['+'], &mut lowered, &mut folded);
        assert_eq!((lowered.as_str(), folded.as_str()), ("c++", "c++"));
    }

    #[test]
    fn accents_stay_in_the_lowered_form() {
        let (mut lowered, mut folded) = (String::new(), String::new());
        normalize_into("Trä-dgård", &[], &mut lowered, &mut folded);
        assert_eq!((lowered.as_str(), folded.as_str()), ("trädgård", "tradgard"));
    }
}
