/// Tokenize text into words, respecting collection-level config.
///
/// A token is a slice of the text, for highlighting. Its term comes from `normalizer`.
///
/// Rules:
/// - Split on whitespace and on `token_separators`, nothing else: other punctuation stays inside
///   the slice and is dropped when the term is made (`non-stick` is the term `nonstick` unless `-`
///   is a separator)
/// - Characters that are not part of a word are trimmed from a token's ends
/// - `symbols_to_index` are part of a word
/// - Discard tokens without a letter or digit, and tokens longer than 256 bytes
pub fn tokenize<'a>(
    text: &'a str,
    symbols_to_index: &[char],
    token_separators: &[char],
) -> Vec<&'a str> {
    let mut tokens = Vec::new();
    tokenize_each(text, symbols_to_index, token_separators, |t| tokens.push(t));
    tokens
}

/// `tokenize` without collecting: `f` gets each token in order.
pub fn tokenize_each<'a>(
    text: &'a str,
    symbols_to_index: &[char],
    token_separators: &[char],
    mut f: impl FnMut(&'a str),
) {
    let is_word = |c: char| c.is_alphanumeric() || symbols_to_index.contains(&c);
    let mut emit = |raw: &'a str| {
        let token = raw.trim_matches(|c| !is_word(c));
        if !token.is_empty() && token.len() <= MAX_TOKEN_BYTES && token.chars().any(char::is_alphanumeric) {
            f(token);
        }
    };
    if token_separators.is_empty() && text.is_ascii() {
        for raw in text.split(|c: char| c.is_ascii_whitespace()) {
            emit(raw);
        }
    } else {
        for raw in text.split(|c: char| c.is_whitespace() || token_separators.contains(&c)) {
            emit(raw);
        }
    }
}

const MAX_TOKEN_BYTES: usize = 256;

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(text: &str) -> Vec<&str> {
        tokenize(text, &[], &[])
    }

    #[test]
    fn simple_words() {
        assert_eq!(tok("hello world"), vec!["hello", "world"]);
    }

    #[test]
    fn punctuation_stripped() {
        assert_eq!(tok("hello, world!"), vec!["hello", "world"]);
    }

    #[test]
    fn decimal_number_preserved() {
        assert_eq!(tok("price: $19.99"), vec!["price", "19.99"]);
    }

    #[test]
    fn punctuation_inside_a_word_does_not_split_it() {
        assert_eq!(tok("non-stick 556000-1234 a/b"), vec!["non-stick", "556000-1234", "a/b"]);
    }

    #[test]
    fn various_whitespace() {
        assert_eq!(tok("one\ttwo\nthree"), vec!["one", "two", "three"]);
    }

    #[test]
    fn unicode_spaces_split() {
        assert_eq!(tok("yas\u{00A0}air anna\u{2007}karin"), vec!["yas", "air", "anna", "karin"]);
    }

    #[test]
    fn empty_string() {
        assert_eq!(tok(""), Vec::<&str>::new());
    }

    #[test]
    fn unicode_accented() {
        assert_eq!(tok("café naïve"), vec!["café", "naïve"]);
    }

    #[test]
    fn long_token_discarded() {
        let long = "a".repeat(257);
        assert_eq!(tok(&long), Vec::<&str>::new());
    }

    #[test]
    fn max_length_token_kept() {
        let exact = "a".repeat(256);
        assert_eq!(tok(&exact), vec![exact.as_str()]);
    }

    #[test]
    fn symbols_to_index_respected() {
        // $ stays attached to 100; standalone + has no alphanumeric so is discarded
        let result = tokenize("$100 + tax", &['$', '+'], &[]);
        assert_eq!(result, vec!["$100", "tax"]);
    }

    #[test]
    fn symbols_to_index_in_word() {
        // + inside a word with alphanumeric chars is kept
        let result = tokenize("C++ rocks", &['+'], &[]);
        assert_eq!(result, vec!["C++", "rocks"]);
    }

    #[test]
    fn token_separators_respected() {
        let result = tokenize("hello|world non-stick", &[], &['|', '-']);
        assert_eq!(result, vec!["hello", "world", "non", "stick"]);
    }

    #[test]
    fn pure_punctuation_discarded() {
        assert_eq!(tok("..."), Vec::<&str>::new());
    }

    #[test]
    fn numbers_preserved() {
        assert_eq!(tok("item 42"), vec!["item", "42"]);
    }

    #[test]
    fn whitespace_only() {
        assert_eq!(tok("   \t\n  "), Vec::<&str>::new());
    }
}
