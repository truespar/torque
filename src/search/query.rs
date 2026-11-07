//! A search's `q`: words, `"exact phrases"` and `-exclusions` (of a word or a phrase).

use std::collections::HashSet;

/// `q` read: its tokens as written, stopwords left out.
#[derive(Debug, Default, PartialEq)]
pub struct ParsedQuery {
    pub words: Vec<String>,
    pub phrases: Vec<Vec<String>>,
    pub exclude: Vec<Vec<String>>,
    /// Every word was a stopword: nothing is left to find.
    pub only_stopwords: bool,
}

impl ParsedQuery {
    /// No word and no phrase to look for.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.phrases.is_empty()
    }
}

pub fn parse(q: &str, symbols: &[char], separators: &[char], stopwords: Option<&HashSet<String>>) -> ParsedQuery {
    let tokens = |text: &str| -> Vec<String> { crate::text::tokenize(text, symbols, separators).into_iter().map(str::to_string).collect() };
    let mut out = ParsedQuery::default();
    let mut rest = q.trim_start();
    while !rest.is_empty() {
        let negated = rest.starts_with('-') && !symbols.contains(&'-') && rest.len() > 1 && !rest[1..].starts_with(char::is_whitespace);
        if negated {
            rest = &rest[1..];
        }
        if let Some(after) = rest.strip_prefix('"') {
            let (phrase, tail) = after.split_once('"').unwrap_or((after, ""));
            let phrase = tokens(phrase);
            if !phrase.is_empty() {
                if negated { out.exclude.push(phrase) } else { out.phrases.push(phrase) }
            }
            rest = tail.trim_start();
            continue;
        }
        let end = rest.find(|c: char| c.is_whitespace() || c == '"').unwrap_or(rest.len());
        let (chunk, tail) = rest.split_at(end);
        for token in tokens(chunk) {
            if negated {
                out.exclude.push(vec![token]);
            } else {
                out.words.push(token);
            }
        }
        rest = tail.trim_start();
    }
    if let Some(sw) = stopwords {
        let had_words = !out.words.is_empty();
        out.words.retain(|t| !sw.contains(&crate::text::lowercase_only(t)) && !sw.contains(&crate::text::normalize(t)));
        out.only_stopwords = had_words && out.words.is_empty() && out.phrases.is_empty();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(q: &str) -> ParsedQuery {
        parse(q, &[], &[], None)
    }

    #[test]
    fn words_phrases_and_exclusions() {
        let q = p(r#"electric "sports car" -tesla -"model s" cheap"#);
        assert_eq!(q.words, ["electric", "cheap"]);
        assert_eq!(q.phrases, [vec!["sports", "car"]]);
        assert_eq!(q.exclude, [vec!["tesla".to_string()], vec!["model".to_string(), "s".to_string()]]);
    }

    #[test]
    fn a_hyphen_inside_a_word_is_part_of_it() {
        assert_eq!(p("non-stick pan").words, ["non-stick", "pan"]);
        assert_eq!(p("a - b").words, ["a", "b"]);
    }

    #[test]
    fn an_open_quote_runs_to_the_end() {
        assert_eq!(p("\"exact words").phrases, [vec!["exact", "words"]]);
    }

    #[test]
    fn a_query_without_words_is_empty() {
        for q in ["---", "???", "\"\"", "  "] {
            assert!(p(q).is_empty(), "{q}");
        }
        assert!(p("-anna").is_empty());
        assert_eq!(p("-anna").exclude.len(), 1);
    }

    #[test]
    fn stopwords_leave_the_words() {
        let sw: HashSet<String> = ["the".to_string()].into_iter().collect();
        assert_eq!(parse("The fox", &[], &[], Some(&sw)).words, ["fox"]);
        let q = parse("the", &[], &[], Some(&sw));
        assert!(q.only_stopwords && q.is_empty());
    }

    #[test]
    fn separators_split_a_word() {
        assert_eq!(parse("non-stick", &[], &['-'], None).words, ["non", "stick"]);
    }
}
