use crate::index::geo::Polygon;
use crate::schema::{FieldDef, FieldId, FieldType, Schema};
use std::sync::Arc;

use super::{FilterOp, FilterValue, Match, TextFilter};

#[derive(Debug)]
pub struct FilterParseError {
    pub message: String,
}

impl std::fmt::Display for FilterParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub fn parse_filter(input: &str, schema: &Schema) -> Result<Option<FilterOp>, FilterParseError> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }

    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Ok(None);
    }

    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        schema,
        depth: 0,
    };
    let op = parser.parse_or()?;

    if parser.pos < parser.tokens.len() {
        return Err(FilterParseError {
            message: format!(
                "Unexpected token `{}` at position {}",
                parser.tokens[parser.pos].text, parser.pos
            ),
        });
    }

    Ok(Some(op))
}

/// Typesense's error on a filter field: "Error with filter field `x`: …".
fn field_error(def: &FieldDef, message: &str) -> FilterParseError {
    FilterParseError { message: format!("Error with filter field `{}`: {message}", def.name) }
}

/// Typesense's error for a malformed geo value, of the first form or of the bracketed one.
fn geo_format(def: &FieldDef, bracketed: bool) -> FilterParseError {
    let forms = if bracketed {
        "`([-44.50, 170.29], radius: 0.75 km, exact_filter_radius: 5 km)` or ([56.33, -65.97, 23.82, -127.82], exact_filter_radius: 7 km)"
    } else {
        "`(-44.50, 170.29, 0.75 km)` or (56.33, -65.97, 23.82, -127.82)"
    };
    FilterParseError { message: format!("Value of filter field `{}`: must be in the {forms} format.", def.name) }
}

/// One geo value, the text in its parentheses.
fn geo_value(def: &FieldDef, field_id: FieldId, text: &str) -> Result<FilterOp, FilterParseError> {
    let coordinate = |s: &str| s.trim().parse::<f64>().ok().filter(|f| f.is_finite());
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('[') {
        let format = || geo_format(def, true);
        let (coordinates, params) = rest.split_once(']').ok_or_else(format)?;
        let c = coordinates.split(',').map(coordinate).collect::<Option<Vec<f64>>>().ok_or_else(format)?;
        let (mut radius, mut exact) = (None, None);
        let params = params.trim();
        if !params.is_empty() {
            for param in params.strip_prefix(',').ok_or_else(format)?.split(',') {
                let (key, value) = param.split_once(':').ok_or_else(format)?;
                let slot = match key.trim() {
                    "radius" => &mut radius,
                    "exact_filter_radius" => &mut exact,
                    _ => return Err(format()),
                };
                if slot.replace(distance(value, format)?).is_some() {
                    return Err(format());
                }
            }
        }
        // Typesense matches coarse cells beyond `exact_filter_radius`; every point is tested here
        let _ = exact;
        return match (c.len(), radius) {
            (2, Some(r)) => Ok(FilterOp::GeoRadius(field_id, c[0], c[1], r)),
            (n, None) if n >= 6 && n % 2 == 0 => polygon(field_id, &c),
            _ => Err(format()),
        };
    }
    let format = || geo_format(def, false);
    let parts: Vec<&str> = text.split(',').collect();
    match parts.iter().map(|p| coordinate(p)).collect::<Option<Vec<f64>>>() {
        Some(c) if c.len() == 4 => Ok(FilterOp::GeoBoundingBox(field_id, c[0].min(c[2]), c[1].min(c[3]), c[0].max(c[2]), c[1].max(c[3]))),
        Some(c) if c.len() >= 6 && c.len() % 2 == 0 => polygon(field_id, &c),
        _ if parts.len() == 3 => {
            let (Some(lat), Some(lng)) = (coordinate(parts[0]), coordinate(parts[1])) else {
                return Err(format());
            };
            Ok(FilterOp::GeoRadius(field_id, lat, lng, distance(parts[2], format)?))
        }
        _ => Err(format()),
    }
}

/// A radius in metres, from `10 km` or `10mi`: the number read as a 32-bit float (Typesense's
/// `stof`), times 1000 for km or 1609.34 for mi.
fn distance(text: &str, format: impl Fn() -> FilterParseError) -> Result<f64, FilterParseError> {
    let text = text.trim();
    let metres = match text.get(text.len().saturating_sub(2)..) {
        Some("km") => 1000.0,
        Some("mi") => 1609.34,
        _ => return Err(FilterParseError { message: "Unit must be either `km` or `mi`.".into() }),
    };
    let r: f32 = text[..text.len() - 2].trim().parse().map_err(|_| format())?;
    Ok(f64::from(r) * metres)
}

/// A polygon of latitude, longitude pairs, or S2's complaint about it.
fn polygon(field_id: FieldId, c: &[f64]) -> Result<FilterOp, FilterParseError> {
    let vertices: Vec<(f64, f64)> = c.chunks(2).map(|p| (p[0], p[1])).collect();
    Polygon::new(&vertices).map(|p| FilterOp::GeoPolygon(field_id, Arc::new(p))).map_err(|message| FilterParseError { message })
}

// -- Tokenizer --
//
// Typesense's lexing: an expression is a field name up to its `:`, an operator, and a value that
// runs to the end of the expression (`&&`, `||`, a parenthesis, the end) or, in a list, to its `,`
// or `]`. The value is whatever it holds (`AT&T`, `a/b`, `10:30`, `O'Brien`). Backticks quote
// (`` \` `` is a backtick in them), and so does an opening double quote: on a string field a phrase
// when nothing follows the closing one. A value in parentheses (a geo value, nested conditions),
// one in a list too, and a join's filter are kept whole, as written.

#[derive(Debug, Clone, PartialEq)]
enum TokenKind {
    Field,      // field name
    Operator,   // :=, :!=, :!, :>, :>=, :<, :<=, :
    Value,      // a value as written, without its quotes
    Group,      // a value in parentheses, its text (`48.85, 2.35, 10 km`, `color:=Black && size:=M`)
    LParen,     // (
    RParen,     // )
    LBracket,   // [
    RBracket,   // ]
    And,        // &&
    Or,         // ||
    Comma,      // ,
    AllOf,      // ALL
    JoinRef,    // $CollectionName, its filter the Value after it
    Star,       // a value of `*` alone (`id:*`)
}

#[derive(Debug, Clone)]
struct Token {
    kind: TokenKind,
    text: String,
    /// A value written in double quotes (on a string field a phrase).
    phrase: bool,
}

fn tokenize(input: &str) -> Result<Vec<Token>, FilterParseError> {
    let chars: Vec<char> = input.chars().collect();
    let mut lexer = Lexer { chars: &chars, i: 0, tokens: Vec::new() };
    loop {
        lexer.skip_space();
        let Some(c) = lexer.at(0) else { break };
        match c {
            '(' => lexer.symbol(TokenKind::LParen, "("),
            ')' => lexer.symbol(TokenKind::RParen, ")"),
            '&' if lexer.at(1) == Some('&') => lexer.symbol(TokenKind::And, "&&"),
            '|' if lexer.at(1) == Some('|') => lexer.symbol(TokenKind::Or, "||"),
            '$' => lexer.join()?,
            _ => lexer.expression()?,
        }
    }
    Ok(lexer.tokens)
}

struct Lexer<'a> {
    chars: &'a [char],
    i: usize,
    tokens: Vec<Token>,
}

impl Lexer<'_> {
    fn at(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).copied()
    }

    fn push(&mut self, kind: TokenKind, text: String, phrase: bool) {
        self.tokens.push(Token { kind, text, phrase });
    }

    /// A token of these characters, here.
    fn symbol(&mut self, kind: TokenKind, text: &str) {
        self.i += text.len();
        self.push(kind, text.into(), false);
    }

    fn skip_space(&mut self) {
        while self.at(0).is_some_and(char::is_whitespace) {
            self.i += 1;
        }
    }

    /// At `&&`, `||` or a parenthesis: where an expression's value ends.
    fn at_boundary(&self) -> bool {
        match self.at(0) {
            Some('(' | ')') => true,
            Some(c @ ('&' | '|')) => self.at(1) == Some(c),
            _ => false,
        }
    }

    /// A field, its operator and its value.
    fn expression(&mut self) -> Result<(), FilterParseError> {
        let start = self.i;
        while self.at(0).is_some_and(|c| c != ':') && !self.at_boundary() {
            self.i += 1;
        }
        let field: String = self.chars[start..self.i].iter().collect();
        self.push(TokenKind::Field, field.trim().to_string(), false);
        if self.at(0) != Some(':') {
            return Ok(()); // no operator: the parser says so
        }
        self.operator();
        self.value()
    }

    /// The operator a `:` starts: `:=`, `:!=`, `:!`, `:>`, `:>=`, `:<`, `:<=`, or the `:` alone;
    /// spaces after the `:` are allowed (Typesense trims the value it reads them from).
    fn operator(&mut self) {
        self.i += 1;
        let mut j = self.i;
        while self.chars.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        let op = match (self.chars.get(j).copied(), self.chars.get(j + 1).copied()) {
            (Some('!'), Some('=')) => ":!=",
            (Some('>'), Some('=')) => ":>=",
            (Some('<'), Some('=')) => ":<=",
            (Some('='), _) => ":=",
            (Some('!'), _) => ":!",
            (Some('>'), _) => ":>",
            (Some('<'), _) => ":<",
            _ => ":",
        };
        if op != ":" {
            self.i = j + op.len() - 1;
        }
        self.push(TokenKind::Operator, op.into(), false);
    }

    /// An operator's value: a list (an `ALL` one too), a group, or text.
    fn value(&mut self) -> Result<(), FilterParseError> {
        self.skip_space();
        if self.chars[self.i..].starts_with(&['A', 'L', 'L']) {
            let mut j = self.i + 3;
            while self.chars.get(j).is_some_and(|c| c.is_whitespace()) {
                j += 1;
            }
            if self.chars.get(j) == Some(&'[') {
                self.i = j;
                self.push(TokenKind::AllOf, "ALL".into(), false);
            }
        }
        match self.at(0) {
            Some('[') => self.list(),
            Some('(') => self.group(),
            _ => {
                self.text_value(false);
                Ok(())
            }
        }
    }

    /// A list's values, each to its `,` or the `]` (one in parentheses whole).
    fn list(&mut self) -> Result<(), FilterParseError> {
        self.symbol(TokenKind::LBracket, "[");
        loop {
            self.skip_space();
            match self.at(0) {
                Some(']') => {
                    self.symbol(TokenKind::RBracket, "]");
                    break;
                }
                Some(',') => self.symbol(TokenKind::Comma, ","),
                Some('(') => {
                    let inner = self.balanced().ok_or_else(|| FilterParseError {
                        message: "Could not parse the filter query: unbalanced parentheses.".into(),
                    })?;
                    self.push(TokenKind::Group, inner, false);
                }
                Some(_) if !self.at_boundary() => self.text_value(true),
                _ => break,
            }
        }
        self.end_of_value()
    }

    /// A value in parentheses, kept whole.
    fn group(&mut self) -> Result<(), FilterParseError> {
        let inner = self.balanced().ok_or_else(|| FilterParseError {
            message: "Could not parse the filter query: unbalanced parentheses.".into(),
        })?;
        self.push(TokenKind::Group, inner, false);
        self.end_of_value()
    }

    /// A join, `$Collection(filter)`: the collection's name, then its filter as a value.
    fn join(&mut self) -> Result<(), FilterParseError> {
        let start = self.i;
        let error = |lexer: &Self| FilterParseError {
            message: format!("Could not parse the reference filter: `{}`.", lexer.chars[start..].iter().collect::<String>()),
        };
        self.i += 1;
        while self.at(0).is_some() && !self.at_boundary() {
            self.i += 1;
        }
        let name: String = self.chars[start + 1..self.i].iter().collect();
        let name = name.trim().to_string();
        if name.is_empty() || self.at(0) != Some('(') {
            return Err(error(self));
        }
        let Some(filter) = self.balanced() else {
            return Err(error(self));
        };
        self.push(TokenKind::JoinRef, name, false);
        self.push(TokenKind::Value, filter, false);
        self.end_of_value()
    }

    /// The text between the `(` here and its `)`, after which it moves on (backticks quote).
    fn balanced(&mut self) -> Option<String> {
        let start = self.i + 1;
        let (mut depth, mut quoted) = (0usize, false);
        for j in self.i..self.chars.len() {
            match self.chars[j] {
                '`' if !(quoted && self.chars[j - 1] == '\\') => quoted = !quoted,
                '(' if !quoted => depth += 1,
                ')' if !quoted => {
                    depth -= 1;
                    if depth == 0 {
                        self.i = j + 1;
                        return Some(self.chars[start..j].iter().collect());
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// After a list or a group, nothing but the expression's end.
    fn end_of_value(&mut self) -> Result<(), FilterParseError> {
        self.skip_space();
        match self.at(0) {
            Some(c) if !self.at_boundary() => Err(FilterParseError {
                message: format!("Unexpected character `{c}` at position {}", self.i),
            }),
            _ => Ok(()),
        }
    }

    /// A value's text as a token (none when it is empty), `*` alone a `Star`.
    fn text_value(&mut self, list: bool) {
        let (text, phrase) = self.text(list);
        match text.as_str() {
            "" => {}
            "*" => self.push(TokenKind::Star, text, false),
            _ => self.push(TokenKind::Value, text, phrase),
        }
    }

    /// A value's text, to the end of its expression (in a list to its `,` or `]`) and trimmed,
    /// and whether it is a phrase: one in double quotes, nothing after them.
    fn text(&mut self, list: bool) -> (String, bool) {
        let mut text = String::new();
        let mut phrase = false;
        if self.at(0) == Some('"') {
            if let Some(n) = self.chars[self.i + 1..].iter().position(|&c| c == '"') {
                text.extend(&self.chars[self.i + 1..self.i + 1 + n]);
                self.i += n + 2;
                phrase = true;
            }
        }
        while let Some(c) = self.at(0) {
            if self.at_boundary() || (list && matches!(c, ',' | ']')) {
                break;
            }
            self.i += 1;
            phrase &= c.is_whitespace();
            if c != '`' {
                text.push(c);
                continue;
            }
            while let Some(q) = self.at(0) {
                self.i += 1;
                match q {
                    '`' => break,
                    '\\' if self.at(0) == Some('`') => {
                        text.push('`');
                        self.i += 1;
                    }
                    _ => text.push(q),
                }
            }
        }
        (text.trim().to_string(), phrase)
    }
}

// -- Recursive descent parser --
// Precedence: OR < AND < NOT < atom

const MAX_PARSE_DEPTH: usize = 32;

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    schema: &'a Schema,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> &Token {
        let t = &self.tokens[self.pos];
        self.pos += 1;
        t
    }

    fn expect(&mut self, kind: TokenKind) -> Result<&Token, FilterParseError> {
        if let Some(t) = self.peek() {
            if t.kind == kind {
                return Ok(self.advance());
            }
            Err(FilterParseError {
                message: format!("Expected {:?}, got `{}`", kind, t.text),
            })
        } else {
            Err(FilterParseError {
                message: format!("Expected {:?}, got end of input", kind),
            })
        }
    }

    // OR: and (|| and)*
    fn parse_or(&mut self) -> Result<FilterOp, FilterParseError> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(t) if t.kind == TokenKind::Or) {
            self.advance();
            let right = self.parse_and()?;
            left = match left {
                FilterOp::Or(mut ops) => {
                    ops.push(right);
                    FilterOp::Or(ops)
                }
                _ => FilterOp::Or(vec![left, right]),
            };
        }
        Ok(left)
    }

    // AND: atom (&& atom)*
    fn parse_and(&mut self) -> Result<FilterOp, FilterParseError> {
        let mut left = self.parse_atom()?;
        while matches!(self.peek(), Some(t) if t.kind == TokenKind::And) {
            self.advance();
            let right = self.parse_atom()?;
            left = match left {
                FilterOp::And(mut ops) => {
                    ops.push(right);
                    FilterOp::And(ops)
                }
                _ => FilterOp::And(vec![left, right]),
            };
        }
        Ok(left)
    }

    // atom: '(' or ')' | '$Collection(...)' | field_expr
    fn parse_atom(&mut self) -> Result<FilterOp, FilterParseError> {
        let Some(t) = self.peek() else {
            return Err(FilterParseError {
                message: "Unexpected end of filter expression".into(),
            });
        };

        // JOIN filter: $CollectionName(inner_filter)
        if t.kind == TokenKind::JoinRef {
            return self.parse_join_filter();
        }

        if t.kind == TokenKind::LParen {
            self.depth += 1;
            if self.depth > MAX_PARSE_DEPTH {
                return Err(FilterParseError {
                    message: format!(
                        "Filter nesting depth exceeds maximum of {MAX_PARSE_DEPTH}."
                    ),
                });
            }
            self.advance();
            let op = self.parse_or()?;
            self.expect(TokenKind::RParen)?;
            self.depth -= 1;
            return Ok(op);
        }

        self.parse_field_expr()
    }

    // field_expr: field operator value
    //   OR: prefix:(inner_field:=val && inner_field:=val)  (nested AND)
    fn parse_field_expr(&mut self) -> Result<FilterOp, FilterParseError> {
        let field_token = self.expect(TokenKind::Field)?;
        let field_name = field_token.text.clone();

        // No `:`, before the field is looked up: Typesense's 400 for `x'||sleep(5)` too, not a 404 for a field `sleep`
        if !matches!(self.peek(), Some(t) if t.kind == TokenKind::Operator) {
            return Err(FilterParseError { message: "Could not parse the filter query.".into() });
        }

        // `id` is the documents' id, never a schema field (Typesense)
        if field_name == "id" {
            return self.parse_id_filter();
        }

        let Some(field_id) = self.schema.name_to_id.get(&field_name).copied() else {
            // conditions on one element of an object array: `variants:(color:=Black && size:=M)`
            if matches!(self.peek(), Some(t) if t.kind == TokenKind::Operator && t.text == ":")
                && matches!(self.tokens.get(self.pos + 1), Some(t) if t.kind == TokenKind::Group)
            {
                let inner = self.tokens[self.pos + 1].text.clone();
                self.pos += 2;
                return self.parse_nested_and(&field_name, &inner);
            }
            return Err(FilterParseError {
                message: format!("Could not find a filter field named `{field_name}` in the schema."),
            });
        };

        let operator = self.advance().text.clone();
        let schema: &Schema = self.schema;
        let def = &schema.fields[field_id as usize];
        match def.field_type {
            _ if def.field_type.is_searchable() => self.parse_string_filter(field_id, &operator),
            FieldType::GeoPoint | FieldType::GeoPointArray => self.parse_geo_filter(field_id, &operator),
            FieldType::FloatArray { .. } if def.num_dim.is_some() => Err(FilterParseError {
                message: format!("Cannot filter on vector field `{}`.", def.name),
            }),
            FieldType::Int32
            | FieldType::Int64
            | FieldType::Float64
            | FieldType::Int32Array
            | FieldType::Int64Array
            | FieldType::FloatArray { .. } => self.parse_number_filter(field_id, &operator),
            FieldType::Bool | FieldType::BoolArray => self.parse_bool_filter(field_id, &operator),
            _ => Err(field_error(def, "Unidentified field data type, see docs for supported data types.")),
        }
    }

    /// A filter on a string field, Typesense's: `:=` the value exactly, `:` its words (in double
    /// quotes a phrase: one after the other), `:!=` and `:!` their negations; a list `[a, b]` any
    /// of its values, `ALL[a, b]` every one. Each value is matched by its tokens (`TextFilter`).
    fn parse_string_filter(&mut self, field_id: u16, operator: &str) -> Result<FilterOp, FilterParseError> {
        let schema: &Schema = self.schema;
        let def = &schema.fields[field_id as usize];
        let empty = || field_error(def, "Filter value cannot be empty.");
        let (how, negate) = match operator {
            ":=" => (Match::Exact, false),
            ":!=" => (Match::Exact, true),
            ":" => (Match::Words, false),
            ":!" => (Match::Words, true),
            other => return Err(field_error(def, &format!("`{}` is not supported on a string field.", &other[1..]))),
        };
        let all = matches!(self.peek(), Some(t) if t.kind == TokenKind::AllOf);
        if all {
            self.advance();
        }
        let mut values = Vec::new();
        if all || matches!(self.peek(), Some(t) if t.kind == TokenKind::LBracket) {
            self.expect(TokenKind::LBracket)?;
            loop {
                values.push(self.parse_string_value(empty)?);
                if !matches!(self.peek(), Some(t) if t.kind == TokenKind::Comma) {
                    break;
                }
                self.advance();
            }
            self.expect(TokenKind::RBracket)?;
        } else {
            values.push(self.parse_string_value(empty)?);
        }
        let mut ops = Vec::with_capacity(values.len());
        for (text, phrase) in values {
            let how = if phrase && how == Match::Words { Match::Phrase } else { how };
            ops.push(FilterOp::Text(field_id, TextFilter::new(&text, how, schema, def).ok_or_else(empty)?));
        }
        let op = match ops.len() {
            1 => ops.pop().unwrap(),
            _ if all => FilterOp::And(ops),
            _ => FilterOp::Or(ops),
        };
        Ok(if negate { FilterOp::Not(Box::new(op)) } else { op })
    }

    /// A string filter value, and whether it is a phrase (in double quotes). A value in
    /// parentheses is its words, as Typesense reads it.
    fn parse_string_value(&mut self, empty: impl Fn() -> FilterParseError) -> Result<(String, bool), FilterParseError> {
        if !matches!(self.peek(), Some(t) if matches!(t.kind, TokenKind::Value | TokenKind::Group)) {
            return Err(empty());
        }
        let t = self.advance();
        Ok((t.text.clone(), t.phrase))
    }

    /// A filter on a numeric field, Typesense's: `:` and `:=` alike, `:!=` and `:!` their
    /// negation; a value is a number, a comparison (`>5`, `>=5`, `<5`, `<=5`, `!=5`) or a range
    /// (`1..5`, both ends included); a list `[…]` any of its values, `ALL[…]` every one.
    fn parse_number_filter(&mut self, field_id: u16, operator: &str) -> Result<FilterOp, FilterParseError> {
        let schema: &Schema = self.schema;
        let def = &schema.fields[field_id as usize];
        let (comparison, negate) = match operator {
            ":" | ":=" => ("", false),
            ":!=" | ":!" => ("", true),
            other => (&other[1..], false),
        };
        let list = comparison.is_empty() && matches!(self.peek(), Some(t) if matches!(t.kind, TokenKind::AllOf | TokenKind::LBracket));
        let op = if list {
            let all = matches!(self.peek(), Some(t) if t.kind == TokenKind::AllOf);
            if all {
                self.advance();
            }
            self.expect(TokenKind::LBracket)?;
            let mut ops = Vec::new();
            loop {
                ops.push(number_op(def, field_id, &self.value_text(def)?)?);
                if !matches!(self.peek(), Some(t) if t.kind == TokenKind::Comma) {
                    break;
                }
                self.advance();
            }
            self.expect(TokenKind::RBracket)?;
            let (values, mut rest) = equalities(ops);
            let numbers = match values.len() {
                0 => None,
                1 if !all => Some(FilterOp::Eq(field_id, values.into_iter().next().unwrap())),
                _ if all => Some(FilterOp::AllOf(field_id, values)),
                _ => Some(FilterOp::In(field_id, values)),
            };
            match (numbers, rest.len()) {
                (Some(op), 0) => op,
                (None, 1) => rest.pop().unwrap(),
                (numbers, _) => {
                    let ops = numbers.into_iter().chain(rest).collect();
                    if all { FilterOp::And(ops) } else { FilterOp::Or(ops) }
                }
            }
        } else {
            number_op(def, field_id, &format!("{comparison}{}", self.value_text(def)?))?
        };
        Ok(match (negate, op) {
            (false, op) => op,
            (true, FilterOp::Eq(f, v)) => FilterOp::Neq(f, v),
            (true, op) => FilterOp::Not(Box::new(op)),
        })
    }

    /// A filter on a bool field: `true` or `false`, a list any of them; `:!=` and `:!` negate.
    fn parse_bool_filter(&mut self, field_id: u16, operator: &str) -> Result<FilterOp, FilterParseError> {
        let schema: &Schema = self.schema;
        let def = &schema.fields[field_id as usize];
        let single = || FilterParseError { message: format!("Value of filter field `{}` must be `true` or `false`.", def.name) };
        let boolean = |text: &str, list: bool| match text {
            "true" => Ok(FilterValue::Bool(true)),
            "false" => Ok(FilterValue::Bool(false)),
            _ if list => Err(FilterParseError { message: format!("Values of filter field `{}`: must be `true` or `false`.", def.name) }),
            _ => Err(single()),
        };
        let negate = match operator {
            ":" | ":=" => false,
            ":!=" | ":!" => true,
            _ => return Err(single()),
        };
        let op = if matches!(self.peek(), Some(t) if t.kind == TokenKind::LBracket) {
            self.advance();
            let mut values = Vec::new();
            loop {
                values.push(boolean(&self.value_text(def)?, true)?);
                if !matches!(self.peek(), Some(t) if t.kind == TokenKind::Comma) {
                    break;
                }
                self.advance();
            }
            self.expect(TokenKind::RBracket)?;
            match values.len() {
                1 => FilterOp::Eq(field_id, values.pop().unwrap()),
                _ => FilterOp::In(field_id, values),
            }
        } else {
            FilterOp::Eq(field_id, boolean(&self.value_text(def)?, false)?)
        };
        Ok(match (negate, op) {
            (false, op) => op,
            (true, FilterOp::Eq(f, v)) => FilterOp::Neq(f, v),
            (true, op) => FilterOp::Not(Box::new(op)),
        })
    }

    /// Typesense's geo filters, one value or a list of them (any one holding a point), in both of
    /// its forms:
    /// - `(lat, lng, 10 km)` (`mi` the other unit, the space optional), the polygon `(lat1, lng1,
    ///   lat2, lng2, lat3, lng3, ...)`, and Torque's box `(lat1, lng1, lat2, lng2)`;
    /// - `([lat, lng], radius: 10 km)` and the polygon `([lat1, lng1, lat2, lng2, lat3, lng3, ...])`,
    ///   either with `exact_filter_radius: 5 km`, read and checked: Typesense matches coarse cells
    ///   beyond that radius, and here every point is tested exactly.
    fn parse_geo_filter(&mut self, field_id: u16, operator: &str) -> Result<FilterOp, FilterParseError> {
        let schema: &Schema = self.schema;
        let def = &schema.fields[field_id as usize];
        let format = || geo_format(def, false);
        let group = |p: &Self| matches!(p.peek(), Some(t) if t.kind == TokenKind::Group);
        if !matches!(operator, ":" | ":=") {
            return Err(format());
        }
        let mut ops = Vec::new();
        if group(self) {
            let text = self.advance().text.clone();
            ops.push(geo_value(def, field_id, &text)?);
        } else if matches!(self.peek(), Some(t) if t.kind == TokenKind::LBracket) {
            self.advance();
            loop {
                if !group(self) {
                    return Err(format());
                }
                let text = self.advance().text.clone();
                ops.push(geo_value(def, field_id, &text)?);
                match self.peek().map(|t| t.kind.clone()) {
                    Some(TokenKind::Comma) => {
                        self.advance();
                    }
                    Some(TokenKind::RBracket) => {
                        self.advance();
                        break;
                    }
                    _ => return Err(format()),
                }
            }
        } else {
            return Err(format());
        }
        Ok(if ops.len() == 1 { ops.pop().unwrap() } else { FilterOp::Or(ops) })
    }

    /// The text of the value here (`*` and a group as written), or Typesense's error when there
    /// is none.
    fn value_text(&mut self, def: &FieldDef) -> Result<String, FilterParseError> {
        match self.peek().map(|t| (t.kind.clone(), t.text.clone())) {
            Some((TokenKind::Value | TokenKind::Star, text)) => {
                self.advance();
                Ok(text)
            }
            Some((TokenKind::Group, text)) => {
                self.advance();
                Ok(format!("({text})"))
            }
            _ => Err(field_error(def, "Filter value cannot be empty.")),
        }
    }

    /// Typesense's `id` filter: `id:x` or `id:=x`, a list `id:[a, b]`, `id:!=x` (or `id:!x`) for
    /// every other document, `*` (alone or in a list) for every document. An id is looked up, never
    /// compared, so the other operators are refused (Typesense would look up the id `>5`).
    fn parse_id_filter(&mut self) -> Result<FilterOp, FilterParseError> {
        let error = |m: String| FilterParseError { message: format!("Error with filter field `id`: {m}") };
        let empty = || error("Filter value cannot be empty.".into());
        let negate = match self.expect(TokenKind::Operator)?.text.as_str() {
            ":" | ":=" => false,
            ":!=" | ":!" => true,
            other => return Err(error(format!("`{}` is not supported, only `=` and `!=`.", &other[1..]))),
        };
        let (mut ids, mut all) = (Vec::new(), false);
        match self.peek().map(|t| t.kind.clone()) {
            Some(TokenKind::LBracket) => {
                self.advance();
                if !matches!(self.peek(), Some(t) if t.kind == TokenKind::RBracket) {
                    loop {
                        if matches!(self.peek(), Some(t) if t.kind == TokenKind::Star) {
                            self.advance();
                            all = true;
                        } else {
                            ids.push(self.parse_text()?);
                        }
                        if !matches!(self.peek(), Some(t) if t.kind == TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                    }
                }
                self.expect(TokenKind::RBracket)?;
            }
            Some(TokenKind::Star) => {
                self.advance();
                all = true;
            }
            Some(TokenKind::Value) => ids.push(self.parse_text()?),
            _ => return Err(empty()),
        }
        let none = || FilterOp::Ids(Vec::new());
        Ok(match (all, negate) {
            (false, false) => FilterOp::Ids(ids),
            (false, true) => FilterOp::Not(Box::new(FilterOp::Ids(ids))),
            (true, false) => FilterOp::Not(Box::new(none())),
            (true, true) => none(),
        })
    }

    /// A value as written.
    fn parse_text(&mut self) -> Result<String, FilterParseError> {
        Ok(self.expect(TokenKind::Value)?.text.clone())
    }

    /// A join, `$Collection(filter)`: the filter is the target collection's, parsed against its
    /// schema when the join runs.
    fn parse_join_filter(&mut self) -> Result<FilterOp, FilterParseError> {
        let target_collection = self.expect(TokenKind::JoinRef)?.text.clone();
        let inner_filter = self.expect(TokenKind::Value)?.text.trim().to_string();
        if inner_filter.is_empty() {
            return Err(FilterParseError {
                message: format!(
                    "Empty filter in JOIN expression `${target_collection}()`"
                ),
            });
        }
        Ok(FilterOp::JoinFilter {
            target_collection,
            inner_filter,
        })
    }

    /// Nested AND, `prefix:(field:=value && field:=value)`: conditions on one element of an
    /// object array, each field `prefix.field`, each condition `:=`.
    fn parse_nested_and(&self, prefix: &str, inner: &str) -> Result<FilterOp, FilterParseError> {
        let tokens = tokenize(inner)?;
        let schema: &Schema = self.schema;
        let mut sub = Parser { tokens: &tokens, pos: 0, schema, depth: self.depth + 1 };
        let mut conditions = Vec::new();

        loop {
            // Parse inner field name
            let inner_name = sub.expect(TokenKind::Field)?.text.clone();
            let full_name = format!("{prefix}.{inner_name}");
            if !matches!(sub.peek(), Some(t) if t.kind == TokenKind::Operator) {
                return Err(FilterParseError { message: "Could not parse the filter query.".into() });
            }

            let field_id = schema
                .name_to_id
                .get(&full_name)
                .copied()
                .ok_or_else(|| FilterParseError {
                    message: format!("Could not find a filter field named `{full_name}` in the schema."),
                })?;

            // Only `:=` is supported inside nested AND
            let op_token = sub.expect(TokenKind::Operator)?;
            if op_token.text != ":=" {
                return Err(FilterParseError {
                    message: format!(
                        "Only `:=` operator is supported inside nested AND, got `{}`",
                        op_token.text
                    ),
                });
            }

            let def = &schema.fields[field_id as usize];
            let text = sub.value_text(def)?;
            let value = match def.field_type {
                _ if def.field_type.is_searchable() => FilterValue::String(text),
                FieldType::Bool | FieldType::BoolArray => match text.as_str() {
                    "true" => FilterValue::Bool(true),
                    "false" => FilterValue::Bool(false),
                    _ => {
                        return Err(FilterParseError {
                            message: format!("Value of filter field `{}` must be `true` or `false`.", def.name),
                        })
                    }
                },
                _ => number(def, &text)?,
            };
            conditions.push((field_id, value));

            // `&&` (more conditions) or the end
            match sub.peek() {
                Some(t) if t.kind == TokenKind::And => {
                    sub.advance();
                }
                None => break,
                Some(t) => {
                    return Err(FilterParseError {
                        message: format!(
                            "Expected `&&` or `)` in nested AND, got `{}`",
                            t.text
                        ),
                    });
                }
            }
        }

        Ok(FilterOp::NestedAnd(conditions))
    }
}

/// One numeric value as Typesense reads it: a number, a comparison (`>5`, `>=5`, `<5`, `<=5`,
/// `!=5`) or a range (`1..5`, both ends included).
fn number_op(def: &FieldDef, field_id: u16, text: &str) -> Result<FilterOp, FilterParseError> {
    let text = text.trim();
    if text.parse::<f64>().is_ok() {
        return Ok(FilterOp::Eq(field_id, number(def, text)?));
    }
    let comparisons: [(&str, fn(u16, FilterValue) -> FilterOp); 5] = [
        (">=", FilterOp::Gte),
        ("<=", FilterOp::Lte),
        ("!=", FilterOp::Neq),
        (">", FilterOp::Gt),
        ("<", FilterOp::Lt),
    ];
    for (prefix, op) in comparisons {
        if let Some(rest) = text.strip_prefix(prefix) {
            return Ok(op(field_id, number(def, rest.trim())?));
        }
    }
    match text.split_once("..") {
        Some((min, max)) => Ok(FilterOp::Range(field_id, number(def, min.trim())?, number(def, max.trim())?)),
        None => Err(field_error(def, "Numerical field has an invalid comparator.")),
    }
}

/// A number of the field's type, or Typesense's error.
fn number(def: &FieldDef, text: &str) -> Result<FilterValue, FilterParseError> {
    match def.field_type {
        FieldType::Int32 | FieldType::Int32Array => {
            let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(field_error(def, "Not an int32."));
            }
            text.parse::<i32>()
                .map(|n| FilterValue::Int(i64::from(n)))
                .map_err(|_| field_error(def, &format!("`{text}` exceeds the range of an int32.")))
        }
        FieldType::Int64 | FieldType::Int64Array => {
            text.parse::<i64>().map(FilterValue::Int).map_err(|_| field_error(def, "Not an int64."))
        }
        _ => text.parse::<f64>().map(FilterValue::Float).map_err(|_| field_error(def, "Not a float.")),
    }
}

/// A list's plain values, and its other conditions.
fn equalities(ops: Vec<FilterOp>) -> (Vec<FilterValue>, Vec<FilterOp>) {
    let (mut values, mut rest) = (Vec::new(), Vec::new());
    for op in ops {
        match op {
            FilterOp::Eq(_, v) => values.push(v),
            op => rest.push(op),
        }
    }
    (values, rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{FieldDef, FieldType, Language};

    fn test_schema() -> Schema {
        let mut schema = Schema {
            name: "test".into(),
            fields: vec![
                FieldDef {
                    name: "color".into(),
                    field_type: FieldType::String,
                    facet: true,
                    index: true,
                    optional: false,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "price".into(),
                    field_type: FieldType::Int32,
                    facet: false,
                    index: true,
                    optional: false,
                    sort: true,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "tags".into(),
                    field_type: FieldType::StringArray,
                    facet: true,
                    index: true,
                    optional: false,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "in_stock".into(),
                    field_type: FieldType::Bool,
                    facet: false,
                    index: true,
                    optional: false,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "rating".into(),
                    field_type: FieldType::Float64,
                    facet: false,
                    index: true,
                    optional: false,
                    sort: true,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "location".into(),
                    field_type: FieldType::GeoPoint,
                    facet: false,
                    index: true,
                    optional: false,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
            ],
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: Language::default(),
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        schema.assign_field_ids();
        schema
    }

    /// A string field's filter of these terms.
    fn text(fid: u16, terms: &[&str], how: Match) -> FilterOp {
        FilterOp::Text(fid, TextFilter { terms: terms.iter().map(|t| t.as_bytes().to_vec()).collect(), others: Vec::new(), how, prefix: false })
    }

    fn exact(fid: u16, terms: &[&str]) -> FilterOp {
        text(fid, terms, Match::Exact)
    }

    fn not(op: FilterOp) -> FilterOp {
        FilterOp::Not(Box::new(op))
    }

    #[test]
    fn parse_eq_string() {
        let schema = test_schema();
        let op = parse_filter("color:=red", &schema).unwrap().unwrap();
        assert_eq!(op, exact(0, &["red"]));
    }

    #[test]
    fn parse_string_filters() {
        let schema = test_schema();
        let parse = |s: &str| parse_filter(s, &schema).unwrap().unwrap();
        // tokens as the field indexes them: quotes, case and punctuation go
        assert_eq!(parse("color:='E'"), exact(0, &["e"]));
        assert_eq!(parse("color:=`Dark-Red!`"), exact(0, &["darkred"]));
        // `:` every word, in double quotes a phrase; `:=` exact, quoted or not
        assert_eq!(parse("color:dark red"), text(0, &["dark", "red"], Match::Words));
        assert_eq!(parse("color:\"dark red\""), text(0, &["dark", "red"], Match::Phrase));
        assert_eq!(parse("color:\"red\""), text(0, &["red"], Match::Words));
        assert_eq!(parse("color:=\"dark red\""), exact(0, &["dark", "red"]));
        assert_eq!(parse("color:!dark"), not(text(0, &["dark"], Match::Words)));
        assert_eq!(parse("color:![dark, \"light blue\"]"), not(FilterOp::Or(vec![text(0, &["dark"], Match::Words), text(0, &["light", "blue"], Match::Phrase)])));
        // a trailing `*` is a prefix
        let prefixed = |how| FilterOp::Text(0, TextFilter { terms: vec![b"chris".to_vec(), b"p".to_vec()], others: Vec::new(), how, prefix: true });
        assert_eq!(parse("color:=Chris P*"), prefixed(Match::Exact));
        assert_eq!(parse("color:[Chris P*]"), prefixed(Match::Words));
        // (a following `&&` joins the same AND)
        assert_eq!(
            parse("color:=ALL[red, blue] && price:>5"),
            FilterOp::And(vec![exact(0, &["red"]), exact(0, &["blue"]), FilterOp::Gt(1, FilterValue::Int(5))])
        );
        let err = |s: &str| parse_filter(s, &schema).unwrap_err().message;
        for empty in ["color:=", "color:=-", "color:=[]", "color:=[red, ]", "color: && price:>5"] {
            assert_eq!(err(empty), "Error with filter field `color`: Filter value cannot be empty.", "{empty}");
        }
        assert_eq!(err("color:>5"), "Error with filter field `color`: `>` is not supported on a string field.");
    }

    #[test]
    fn parse_eq_number() {
        let schema = test_schema();
        let op = parse_filter("price:=100", &schema).unwrap().unwrap();
        assert_eq!(op, FilterOp::Eq(1, FilterValue::Int(100)));
    }

    #[test]
    fn parse_gt() {
        let schema = test_schema();
        let op = parse_filter("price:>100", &schema).unwrap().unwrap();
        assert_eq!(op, FilterOp::Gt(1, FilterValue::Int(100)));
    }

    #[test]
    fn parse_and() {
        let schema = test_schema();
        let op = parse_filter("price:>=100 && price:<=200", &schema).unwrap().unwrap();
        assert_eq!(
            op,
            FilterOp::And(vec![
                FilterOp::Gte(1, FilterValue::Int(100)),
                FilterOp::Lte(1, FilterValue::Int(200)),
            ])
        );
    }

    #[test]
    fn parse_or() {
        let schema = test_schema();
        let op = parse_filter("color:=red || color:=blue", &schema).unwrap().unwrap();
        assert_eq!(
            op,
            FilterOp::Or(vec![exact(0, &["red"]), exact(0, &["blue"])])
        );
    }

    #[test]
    fn parse_in_array() {
        let schema = test_schema();
        let op = parse_filter("tags:=[red, blue]", &schema).unwrap().unwrap();
        assert_eq!(op, FilterOp::Or(vec![exact(2, &["red"]), exact(2, &["blue"])]));
    }

    #[test]
    fn parse_allof() {
        let schema = test_schema();
        let op = parse_filter("tags:=ALL[red, blue]", &schema).unwrap().unwrap();
        assert_eq!(op, FilterOp::And(vec![exact(2, &["red"]), exact(2, &["blue"])]));
    }

    #[test]
    fn parse_parenthesized_precedence() {
        let schema = test_schema();
        let op = parse_filter("(color:=red || color:=blue) && price:>100", &schema).unwrap().unwrap();
        assert_eq!(
            op,
            FilterOp::And(vec![
                FilterOp::Or(vec![exact(0, &["red"]), exact(0, &["blue"])]),
                FilterOp::Gt(1, FilterValue::Int(100)),
            ])
        );
    }

    #[test]
    fn parse_range() {
        let schema = test_schema();
        let op = parse_filter("price:[50..200]", &schema).unwrap().unwrap();
        assert_eq!(
            op,
            FilterOp::Range(1, FilterValue::Int(50), FilterValue::Int(200))
        );
    }

    #[test]
    fn parse_neq() {
        let schema = test_schema();
        let op = parse_filter("color:!=red", &schema).unwrap().unwrap();
        assert_eq!(op, not(exact(0, &["red"])));
    }

    #[test]
    fn parse_bool() {
        let schema = test_schema();
        let op = parse_filter("in_stock:=true", &schema).unwrap().unwrap();
        assert_eq!(op, FilterOp::Eq(3, FilterValue::Bool(true)));
    }

    #[test]
    fn parse_float() {
        let schema = test_schema();
        let op = parse_filter("rating:>4.5", &schema).unwrap().unwrap();
        assert_eq!(op, FilterOp::Gt(4, FilterValue::Float(4.5)));
    }

    #[test]
    fn parse_quoted_string() {
        let schema = test_schema();
        let op = parse_filter("color:=`light blue`", &schema).unwrap().unwrap();
        assert_eq!(op, exact(0, &["light", "blue"]));
    }

    #[test]
    fn parse_empty_returns_none() {
        let schema = test_schema();
        let op = parse_filter("", &schema).unwrap();
        assert!(op.is_none());
    }

    #[test]
    fn parse_unknown_field_error() {
        let schema = test_schema();
        let err = parse_filter("unknown:=value", &schema).unwrap_err();
        assert_eq!(err.message, "Could not find a filter field named `unknown` in the schema.");
    }

    #[test]
    fn parse_id_filters() {
        let schema = test_schema();
        let ids = |v: &[&str]| FilterOp::Ids(v.iter().map(|s| s.to_string()).collect());
        let parse = |s: &str| parse_filter(s, &schema).unwrap().unwrap();
        assert_eq!(parse("id:=p1"), ids(&["p1"]));
        assert_eq!(parse("id: p1"), ids(&["p1"]));
        assert_eq!(parse("id:[p1, `p 2`, 7]"), ids(&["p1", "p 2", "7"]));
        assert_eq!(parse("id:=[p1,p2]"), ids(&["p1", "p2"]));
        assert_eq!(parse("id:!=p1"), not(ids(&["p1"])));
        assert_eq!(parse("id:!p1"), not(ids(&["p1"])));
        assert_eq!(parse("id:!=[p1, p2]"), not(ids(&["p1", "p2"])));
        // `*` is every document, alone or in a list; `!=*` none
        assert_eq!(parse("id:*"), not(ids(&[])));
        assert_eq!(parse("id:[p1, *]"), not(ids(&[])));
        assert_eq!(parse("id:!=*"), ids(&[]));
        assert_eq!(parse("id:[]"), ids(&[]));
        assert_eq!(parse("id:=p1 && price:>5"), FilterOp::And(vec![ids(&["p1"]), FilterOp::Gt(1, FilterValue::Int(5))]));
        let err = |s: &str| parse_filter(s, &schema).unwrap_err().message;
        assert_eq!(err("id:="), "Error with filter field `id`: Filter value cannot be empty.");
        assert_eq!(err("id:= && price:>5"), "Error with filter field `id`: Filter value cannot be empty.");
        assert_eq!(err("id:>5"), "Error with filter field `id`: `>` is not supported, only `=` and `!=`.");
        // a `*` is no value of other fields
        assert!(parse_filter("color:=*", &schema).is_err());
    }

    #[test]
    fn parse_and_higher_precedence_than_or() {
        let schema = test_schema();
        // a || b && c  should parse as  a || (b && c)
        let op = parse_filter("color:=red || price:>100 && in_stock:=true", &schema).unwrap().unwrap();
        assert_eq!(
            op,
            FilterOp::Or(vec![
                exact(0, &["red"]),
                FilterOp::And(vec![
                    FilterOp::Gt(1, FilterValue::Int(100)),
                    FilterOp::Eq(3, FilterValue::Bool(true)),
                ]),
            ])
        );
    }

    #[test]
    fn parse_neq_array() {
        let schema = test_schema();
        let op = parse_filter("color:!=[red, blue]", &schema).unwrap().unwrap();
        assert_eq!(op, not(FilterOp::Or(vec![exact(0, &["red"]), exact(0, &["blue"])])));
    }

    #[test]
    fn parse_negative_number() {
        let schema = test_schema();
        let op = parse_filter("price:>-50", &schema).unwrap().unwrap();
        assert_eq!(op, FilterOp::Gt(1, FilterValue::Int(-50)));
    }

    #[test]
    fn parse_geo_radius_km() {
        let schema = test_schema();
        let op = parse_filter("location:(48.85, 2.35, 10 km)", &schema).unwrap().unwrap();
        match op {
            FilterOp::GeoRadius(fid, lat, lng, radius) => {
                assert_eq!(fid, 5);
                assert!((lat - 48.85).abs() < 0.001);
                assert!((lng - 2.35).abs() < 0.001);
                assert_eq!(radius, 10_000.0);
            }
            other => panic!("Expected GeoRadius, got {other:?}"),
        }
    }

    #[test]
    fn parse_geo_radius_mi() {
        let schema = test_schema();
        let op = parse_filter("location:(40.71, -74.01, 5 mi)", &schema).unwrap().unwrap();
        match op {
            FilterOp::GeoRadius(_, _, _, radius_m) => {
                assert_eq!(radius_m, 5.0 * 1609.34);
            }
            other => panic!("Expected GeoRadius, got {other:?}"),
        }
    }

    #[test]
    fn parse_geo_combined_with_text_filter() {
        let schema = test_schema();
        let op = parse_filter("location:(48.85, 2.35, 10 km) && color:=red", &schema).unwrap().unwrap();
        match op {
            FilterOp::And(ops) => {
                assert_eq!(ops.len(), 2);
                assert!(matches!(ops[0], FilterOp::GeoRadius(5, _, _, _)));
                assert_eq!(ops[1], exact(0, &["red"]));
            }
            other => panic!("Expected And, got {other:?}"),
        }
    }

    fn nested_test_schema() -> Schema {
        let mut schema = Schema {
            name: "test".into(),
            fields: vec![
                FieldDef {
                    name: "title".into(),
                    field_type: FieldType::String,
                    facet: false,
                    index: true,
                    optional: false,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "variants.color".into(),
                    field_type: FieldType::StringArray,
                    facet: true,
                    index: true,
                    optional: false,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "variants.size".into(),
                    field_type: FieldType::StringArray,
                    facet: true,
                    index: true,
                    optional: false,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "metadata.brand".into(),
                    field_type: FieldType::String,
                    facet: true,
                    index: true,
                    optional: false,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
            ],
            default_sorting_field: None,
            enable_nested_fields: true,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: Language::default(),
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        schema.assign_field_ids();
        schema
    }

    #[test]
    fn parse_nested_and_basic() {
        let schema = nested_test_schema();
        let op = parse_filter("variants:(color:=Black && size:=M)", &schema).unwrap().unwrap();
        assert_eq!(
            op,
            FilterOp::NestedAnd(vec![
                (1, FilterValue::String("Black".into())),
                (2, FilterValue::String("M".into())),
            ])
        );
    }

    #[test]
    fn parse_nested_and_combined_with_regular_filter() {
        let schema = nested_test_schema();
        let op = parse_filter("variants:(color:=Black && size:=M) && metadata.brand:=Acme", &schema).unwrap().unwrap();
        match op {
            FilterOp::And(ops) => {
                assert_eq!(ops.len(), 2);
                assert_eq!(
                    ops[0],
                    FilterOp::NestedAnd(vec![
                        (1, FilterValue::String("Black".into())),
                        (2, FilterValue::String("M".into())),
                    ])
                );
                assert_eq!(ops[1], exact(3, &["acme"]));
            }
            other => panic!("Expected And, got {other:?}"),
        }
    }

    #[test]
    fn parse_nested_and_unknown_inner_field() {
        let schema = nested_test_schema();
        let err = parse_filter("variants:(color:=Black && weight:=100)", &schema).unwrap_err();
        assert_eq!(err.message, "Could not find a filter field named `variants.weight` in the schema.");
        let err = parse_filter("variants:(color:=Black && weight)", &schema).unwrap_err();
        assert_eq!(err.message, "Could not parse the filter query.");
    }

    #[test]
    fn parse_dotted_field_eq() {
        let schema = nested_test_schema();
        let op = parse_filter("metadata.brand:=Acme", &schema).unwrap().unwrap();
        assert_eq!(op, exact(3, &["acme"]));
    }

    #[test]
    fn reject_excessive_nesting_depth() {
        let schema = test_schema();
        // 33 levels of nesting — exceeds MAX_PARSE_DEPTH (32)
        let open = "(".repeat(33);
        let close = ")".repeat(33);
        let filter = format!("{open}color:=red{close}");
        let err = parse_filter(&filter, &schema).unwrap_err();
        assert!(err.message.contains("nesting depth"), "{}", err.message);
    }

    #[test]
    fn parse_unquoted_multiword_value() {
        let schema = test_schema();
        let op = parse_filter("color:=light blue", &schema).unwrap().unwrap();
        assert_eq!(op, exact(0, &["light", "blue"]));
    }

    #[test]
    fn parse_unquoted_multiword_value_with_and() {
        let schema = test_schema();
        let op = parse_filter("color:=light blue && price:>100", &schema).unwrap().unwrap();
        assert_eq!(
            op,
            FilterOp::And(vec![
                exact(0, &["light", "blue"]),
                FilterOp::Gt(1, FilterValue::Int(100)),
            ])
        );
    }

    #[test]
    fn parse_unquoted_multiword_neq() {
        let schema = test_schema();
        let op = parse_filter("color:!=light blue", &schema).unwrap().unwrap();
        assert_eq!(op, not(exact(0, &["light", "blue"])));
    }

    #[test]
    fn accept_max_nesting_depth() {
        let schema = test_schema();
        // 32 levels — exactly at MAX_PARSE_DEPTH
        let open = "(".repeat(32);
        let close = ")".repeat(32);
        let filter = format!("{open}color:=red{close}");
        assert!(parse_filter(&filter, &schema).is_ok());
    }

    #[test]
    fn bare_colon_filters() {
        let schema = test_schema();
        // String field: Typesense's words.
        assert_eq!(parse_filter("color:red", &schema).unwrap().unwrap(), text(0, &["red"], Match::Words));
        assert_eq!(parse_filter("color:dark red", &schema).unwrap().unwrap(), text(0, &["dark", "red"], Match::Words));
        // Numeric field: plain equality.
        match parse_filter("price:5", &schema).unwrap().unwrap() {
            FilterOp::Eq(_, FilterValue::Int(v)) => assert_eq!(v, 5),
            other => panic!("Expected Eq, got {other:?}"),
        }
        // Range syntax still works.
        match parse_filter("price:[1..10]", &schema).unwrap().unwrap() {
            FilterOp::Range(..) => {}
            other => panic!("Expected Range, got {other:?}"),
        }
        // Bare-colon OR list: the words of each value on strings.
        assert_eq!(
            parse_filter("color:[red, dark blue]", &schema).unwrap().unwrap(),
            FilterOp::Or(vec![text(0, &["red"], Match::Words), text(0, &["dark", "blue"], Match::Words)])
        );
        // Bare-colon OR list on numerics: IN.
        match parse_filter("price:[5, 10]", &schema).unwrap().unwrap() {
            FilterOp::In(_, vals) => assert_eq!(vals.len(), 2),
            other => panic!("Expected In, got {other:?}"),
        }
    }
    #[test]
    fn values_hold_what_typesense_reads() {
        let schema = test_schema();
        let parse = |s: &str| parse_filter(s, &schema).unwrap().unwrap();
        let exact_of = |raw: &str| FilterOp::Text(0, TextFilter::new(raw, Match::Exact, &schema, &schema.fields[0]).unwrap());
        let gt5 = || FilterOp::Gt(1, FilterValue::Int(5));
        // a value runs to `&&`, `||`, a parenthesis or the end, whatever it holds
        for raw in ["AT&T", "a/b", "10:30", "O'Brien", "'s-Hertogenbosch", "50%", "foo, bar", "#1 = one!", "a|b", "Dark-Red!"] {
            assert_eq!(parse(&format!("color:={raw} && price:>5")), FilterOp::And(vec![exact_of(raw), gt5()]), "{raw}");
        }
        assert_eq!(parse("color:=O'Brien"), exact(0, &["obrien"]));
        // backticks quote, `\`` is a backtick in them; an operator may stand apart from its `:`
        assert_eq!(parse("color:=`a && (b)` || color: != `c\\`d`"), FilterOp::Or(vec![exact_of("a && (b)"), not(exact_of("c`d"))]));
        assert_eq!(parse("id:=`p\\`1`"), FilterOp::Ids(vec!["p`1".into()]));
        // a list's values run to their `,` or the `]`
        assert_eq!(parse("color:=[AT&T, `x, y`]"), FilterOp::Or(vec![exact_of("AT&T"), exact_of("x, y")]));
        // a join's filter is kept as written
        assert_eq!(
            parse("$other(name:=`a && b` || n:>1) && price:>5"),
            FilterOp::And(vec![
                FilterOp::JoinFilter { target_collection: "other".into(), inner_filter: "name:=`a && b` || n:>1".into() },
                gt5(),
            ])
        );
    }

    #[test]
    fn numbers_bools_and_geo_as_typesense() {
        let schema = test_schema();
        let parse = |s: &str| parse_filter(s, &schema).unwrap().unwrap();
        let int = FilterValue::Int;
        // `:` and `:=` alike, a comparison apart from its `:`, a list of numbers, comparisons and ranges
        assert_eq!(parse("price: >= 5"), FilterOp::Gte(1, int(5)));
        assert_eq!(parse("price:=>5"), FilterOp::Gt(1, int(5)));
        assert_eq!(
            parse("price:[1..5, 10, >20, 30]"),
            FilterOp::Or(vec![FilterOp::In(1, vec![int(10), int(30)]), FilterOp::Range(1, int(1), int(5)), FilterOp::Gt(1, int(20))])
        );
        assert_eq!(parse("price:!=[5, 10]"), not(FilterOp::In(1, vec![int(5), int(10)])));
        assert_eq!(parse("price:!5"), FilterOp::Neq(1, int(5)));
        assert_eq!(parse("price:[-10..-5]"), FilterOp::Range(1, int(-10), int(-5)));
        assert_eq!(parse("rating:[1.5..2]"), FilterOp::Range(4, FilterValue::Float(1.5), FilterValue::Float(2.0)));
        assert_eq!(parse("in_stock:[true, false]"), FilterOp::In(3, vec![FilterValue::Bool(true), FilterValue::Bool(false)]));
        assert_eq!(parse("in_stock:!true"), FilterOp::Neq(3, FilterValue::Bool(true)));
        assert!(matches!(parse("location:(48.85, 2.35, 10km)"), FilterOp::GeoRadius(5, _, _, r) if r == 10_000.0));
        let err = |s: &str| parse_filter(s, &schema).unwrap_err().message;
        assert_eq!(err("price:=abc"), "Error with filter field `price`: Numerical field has an invalid comparator.");
        assert_eq!(err("price:>abc"), "Error with filter field `price`: Not an int32.");
        assert_eq!(err("price:=4.5"), "Error with filter field `price`: Not an int32.");
        assert_eq!(err("price:=3000000000"), "Error with filter field `price`: `3000000000` exceeds the range of an int32.");
        assert_eq!(err("price:[1..x]"), "Error with filter field `price`: Not an int32.");
        assert_eq!(err("rating:<x"), "Error with filter field `rating`: Not a float.");
        assert_eq!(err("price:="), "Error with filter field `price`: Filter value cannot be empty.");
        assert_eq!(err("in_stock:=yes"), "Value of filter field `in_stock` must be `true` or `false`.");
        assert_eq!(err("in_stock:[true, no]"), "Values of filter field `in_stock`: must be `true` or `false`.");
        assert_eq!(err("location:(48.85, 2.35, 10 m)"), "Unit must be either `km` or `mi`.");
        assert_eq!(
            err("location:=near"),
            "Value of filter field `location`: must be in the `(-44.50, 170.29, 0.75 km)` or (56.33, -65.97, 23.82, -127.82) format."
        );
        assert_eq!(err("price"), "Could not parse the filter query.");
        // no `:` is a 400 before any field is looked up: an unknown name and `id` too
        assert_eq!(err("nope"), "Could not parse the filter query.");
        assert_eq!(err("id"), "Could not parse the filter query.");
        assert_eq!(err("color:='x'||sleep(5)||''"), "Could not parse the filter query.");
        assert_eq!(err("$other(x:=1"), "Could not parse the reference filter: `$other(x:=1`.");
    }

    #[test]
    fn geo_polygons_lists_and_the_bracketed_form() {
        let schema = test_schema();
        let parse = |s: &str| parse_filter(s, &schema).unwrap().unwrap();
        let err = |s: &str| parse_filter(s, &schema).unwrap_err().message;
        let square = Polygon::new(&[(48.85, 2.32), (48.85, 2.36), (48.87, 2.36), (48.87, 2.32)]).unwrap();
        let polygon = FilterOp::GeoPolygon(5, Arc::new(square));
        assert_eq!(parse("location:(48.85, 2.32, 48.85, 2.36, 48.87, 2.36, 48.87, 2.32)"), polygon);
        assert_eq!(parse("location:([48.85, 2.32, 48.85, 2.36, 48.87, 2.36, 48.87, 2.32])"), polygon);
        assert_eq!(parse("location:([48.85, 2.32, 48.85, 2.36, 48.87, 2.36, 48.87, 2.32], exact_filter_radius: 7 km)"), polygon);
        // a closed ring is the same polygon
        assert_eq!(parse("location:(48.85, 2.32, 48.85, 2.36, 48.87, 2.36, 48.87, 2.32, 48.85, 2.32)"), polygon);
        assert_eq!(parse("location:([48.85, 2.35], radius: 1.5 km)"), FilterOp::GeoRadius(5, 48.85, 2.35, 1500.0));
        assert_eq!(
            parse("location:([48.85, 2.35], radius: 2mi, exact_filter_radius: 10 km)"),
            FilterOp::GeoRadius(5, 48.85, 2.35, 2.0 * 1609.34)
        );
        // a list holds a point when any of its values does
        assert_eq!(
            parse("location:[(48.85, 2.35, 1 km), ([51.5, -0.12], radius: 2 km), (48.85, 2.32, 48.85, 2.36, 48.87, 2.36, 48.87, 2.32)]"),
            FilterOp::Or(vec![FilterOp::GeoRadius(5, 48.85, 2.35, 1000.0), FilterOp::GeoRadius(5, 51.5, -0.12, 2000.0), polygon.clone()])
        );
        let first = "Value of filter field `location`: must be in the `(-44.50, 170.29, 0.75 km)` or (56.33, -65.97, 23.82, -127.82) format.";
        let bracketed = "Value of filter field `location`: must be in the `([-44.50, 170.29], radius: 0.75 km, exact_filter_radius: 5 km)` or ([56.33, -65.97, 23.82, -127.82], exact_filter_radius: 7 km) format.";
        assert_eq!(err("location:(1, 2, 3, 4, 5)"), first);
        assert_eq!(err("location:(1, 2, 3 km, 4)"), first);
        assert_eq!(err("location:[(1, 2, 3 km), x]"), first);
        assert_eq!(err("location:!=(48.85, 2.35, 1 km)"), first);
        assert_eq!(err("location:([1, 2])"), bracketed);
        assert_eq!(err("location:([1, 2, 3, 4])"), bracketed);
        assert_eq!(err("location:([1, 2], radius: 1 km, radius: 2 km)"), bracketed);
        assert_eq!(err("location:([1, 2], size: 1 km)"), bracketed);
        assert_eq!(err("location:([48.85, 2.32, 48.85, 2.36, 48.87, 2.36], radius: 1 km)"), bracketed);
        assert_eq!(err("location:([1, 2], radius: 1 m)"), "Unit must be either `km` or `mi`.");
        assert_eq!(err("location:(0, 0, 1, 1, 1, 0, 0, 1)"), "Polygon is invalid: Edge 0 crosses edge 2");
    }
}
