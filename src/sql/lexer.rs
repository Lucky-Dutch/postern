//! A PostgreSQL-aware SQL lexer.
//!
//! It follows the token rules of PostgreSQL's scanner closely enough to never
//! split inside a string, dollar-quoted body, quoted identifier, or comment. It
//! is used to split scripts into statements, find the statement at the cursor,
//! guard the formatter, and look for words outside literals. It is not a parser.

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Whitespace,
    LineComment,
    BlockComment,
    Word,
    QuotedIdentifier,
    String,
    DollarString,
    Number,
    Parameter,
    Operator,
    Punctuation,
    Semicolon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind,
    pub text: &'a str,
    /// Byte offset of the token in the tokenized text.
    pub start: usize,
}

impl Token<'_> {
    pub fn end(&self) -> usize {
        self.start + self.text.len()
    }

    pub fn is_trivia(&self) -> bool {
        matches!(
            self.kind,
            TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
        )
    }

    pub fn is_word(&self, word: &str) -> bool {
        self.kind == TokenKind::Word && self.text.eq_ignore_ascii_case(word)
    }
}

pub fn tokenize(sql: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < sql.len() {
        let rest = &sql[index..];
        let (kind, len) = next_token(rest);
        debug_assert!(len > 0 && rest.is_char_boundary(len));
        tokens.push(Token {
            kind,
            text: &rest[..len],
            start: index,
        });
        index += len;
    }
    tokens
}

fn next_token(rest: &str) -> (TokenKind, usize) {
    use TokenKind::*;
    let bytes = rest.as_bytes();
    let c = rest.chars().next().expect("non-empty input");
    let next = bytes.get(1).copied();
    if c.is_whitespace() {
        let len = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        return (Whitespace, len);
    }
    if rest.starts_with("--") {
        return (LineComment, rest.find('\n').unwrap_or(rest.len()));
    }
    if rest.starts_with("/*") {
        return (BlockComment, block_comment_len(rest));
    }
    match c {
        '\'' => return (String, quoted_len(rest, 0, b'\'', false)),
        '"' => return (QuotedIdentifier, quoted_len(rest, 0, b'"', false)),
        'E' | 'e' if next == Some(b'\'') => return (String, quoted_len(rest, 1, b'\'', true)),
        'B' | 'b' | 'X' | 'x' | 'N' | 'n' if next == Some(b'\'') => {
            return (String, quoted_len(rest, 1, b'\'', false));
        }
        'U' | 'u' if next == Some(b'&') => match bytes.get(2) {
            Some(b'\'') => return (String, quoted_len(rest, 2, b'\'', false)),
            Some(b'"') => return (QuotedIdentifier, quoted_len(rest, 2, b'"', false)),
            _ => {}
        },
        '$' => {
            if next.is_some_and(|b| b.is_ascii_digit()) {
                let digits = rest[1..]
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len() - 1);
                return (Parameter, 1 + digits);
            }
            if let Some(len) = dollar_quoted_len(rest) {
                return (DollarString, len);
            }
            return (Punctuation, 1);
        }
        _ => {}
    }
    if is_identifier_start(c) {
        return (Word, identifier_len(rest));
    }
    if c.is_ascii_digit() || (c == '.' && next.is_some_and(|b| b.is_ascii_digit())) {
        return (Number, number_len(rest));
    }
    if c == ';' {
        return (Semicolon, 1);
    }
    if rest.starts_with("::") {
        return (Punctuation, 2);
    }
    if matches!(c, '(' | ')' | '[' | ']' | ',' | ':' | '.') {
        return (Punctuation, 1);
    }
    if is_operator_char(c) {
        return (Operator, operator_len(rest));
    }
    (Punctuation, c.len_utf8())
}

fn block_comment_len(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut depth = 0usize;
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'/' && bytes[index + 1] == b'*' {
            depth += 1;
            index += 2;
        } else if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
        } else {
            index += 1;
        }
    }
    rest.len()
}

/// Length of a quoted token whose opening quote is at `quote_at`. A doubled quote
/// is an escaped quote; with `backslash`, a backslash escapes the next byte.
fn quoted_len(rest: &str, quote_at: usize, quote: u8, backslash: bool) -> usize {
    let bytes = rest.as_bytes();
    let mut index = quote_at + 1;
    while index < bytes.len() {
        let byte = bytes[index];
        if backslash && byte == b'\\' {
            index += 2;
        } else if byte == quote {
            if bytes.get(index + 1) == Some(&quote) {
                index += 2;
            } else {
                return index + 1;
            }
        } else {
            index += 1;
        }
    }
    rest.len()
}

fn dollar_quoted_len(rest: &str) -> Option<usize> {
    let tag_body = &rest[1..];
    let tag_len = tag_body.find('$')?;
    let tag = &tag_body[..tag_len];
    let mut chars = tag.chars();
    if let Some(first) = chars.next()
        && (!is_identifier_start(first) || !chars.all(is_dollar_tag_char))
    {
        return None;
    }
    let delimiter = &rest[..tag_len + 2];
    let body = &rest[delimiter.len()..];
    Some(match body.find(delimiter) {
        Some(position) => delimiter.len() + position + delimiter.len(),
        None => rest.len(),
    })
}

fn is_identifier_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || !c.is_ascii()
}

fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || !c.is_ascii()
}

fn is_dollar_tag_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || !c.is_ascii()
}

fn identifier_len(rest: &str) -> usize {
    rest.find(|c: char| !is_identifier_char(c))
        .unwrap_or(rest.len())
}

fn number_len(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let exponent_sign = matches!(byte, b'+' | b'-')
            && index > 0
            && matches!(bytes[index - 1], b'e' | b'E')
            && !rest[..index].starts_with("0x")
            && !rest[..index].starts_with("0X")
            && bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
        if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.' || exponent_sign {
            if byte == b'.' && bytes.get(index + 1) == Some(&b'.') {
                break;
            }
            index += 1;
        } else {
            break;
        }
    }
    index
}

fn is_operator_char(c: char) -> bool {
    matches!(
        c,
        '+' | '-'
            | '*'
            | '/'
            | '<'
            | '>'
            | '='
            | '~'
            | '!'
            | '@'
            | '#'
            | '%'
            | '^'
            | '&'
            | '|'
            | '`'
            | '?'
    )
}

fn operator_len(rest: &str) -> usize {
    let mut index = 0;
    for (position, c) in rest.char_indices() {
        if !is_operator_char(c)
            || (position > 0
                && (rest[position..].starts_with("--") || rest[position..].starts_with("/*")))
        {
            break;
        }
        index = position + c.len_utf8();
    }
    index
}

/// One statement of a script. `range` covers the statement text from its first
/// significant token to its last one, without the terminating semicolon. `end` is
/// the byte offset just after the terminator, or the end of the script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    pub range: Range<usize>,
    pub end: usize,
}

/// Splits a script at top-level semicolons. Statements made only of comments and
/// whitespace are skipped. Like psql, semicolons inside parentheses and inside
/// `BEGIN ... END` bodies of `CREATE FUNCTION`/`CREATE PROCEDURE` do not split.
pub fn split_statements(sql: &str) -> Vec<Statement> {
    let mut statements = Vec::new();
    let mut start = None;
    let mut last_end = 0;
    let mut parentheses = 0i32;
    let mut begin_depth = 0usize;
    let mut words = [0u8; 4];
    let mut word_count = 0usize;
    for token in tokenize(sql) {
        if token.is_trivia() {
            continue;
        }
        if token.kind == TokenKind::Semicolon && parentheses <= 0 && begin_depth == 0 {
            if let Some(start) = start.take() {
                statements.push(Statement {
                    range: start..last_end,
                    end: token.end(),
                });
            }
            parentheses = 0;
            words = [0; 4];
            word_count = 0;
            continue;
        }
        if start.is_none() {
            start = Some(token.start);
        }
        last_end = token.end();
        match token.text {
            "(" => parentheses += 1,
            ")" => parentheses -= 1,
            _ => {}
        }
        if token.kind == TokenKind::Word {
            let lower = token.text.to_ascii_lowercase();
            if word_count < words.len()
                && matches!(
                    lower.as_str(),
                    "create" | "function" | "procedure" | "or" | "replace"
                )
            {
                words[word_count] = lower.as_bytes()[0];
            }
            word_count += 1;
            let routine = words[0] == b'c'
                && (words[1] == b'f'
                    || words[1] == b'p'
                    || (words[1] == b'o' && words[2] == b'r' && matches!(words[3], b'f' | b'p')));
            if routine && parentheses == 0 {
                match lower.as_str() {
                    "begin" => begin_depth += 1,
                    "case" if begin_depth >= 1 => begin_depth += 1,
                    "end" => begin_depth = begin_depth.saturating_sub(1),
                    _ => {}
                }
            }
        }
    }
    if let Some(start) = start {
        statements.push(Statement {
            range: start..last_end,
            end: sql.len(),
        });
    }
    statements
}

/// Returns the statement containing the byte offset `cursor`. Between two
/// statements, the previous one is chosen while the cursor stays on the line of
/// its terminator; otherwise the next one.
pub fn statement_at(sql: &str, cursor: usize) -> Option<Range<usize>> {
    let statements = split_statements(sql);
    for (index, statement) in statements.iter().enumerate() {
        if cursor <= statement.end {
            if cursor >= statement.range.start || index == 0 {
                return Some(statement.range.clone());
            }
            let previous = &statements[index - 1];
            let gap = sql.get(previous.end..cursor).unwrap_or_default();
            return Some(if gap.contains('\n') {
                statement.range.clone()
            } else {
                previous.range.clone()
            });
        }
    }
    statements.last().map(|statement| statement.range.clone())
}

/// True when `word` appears as an unquoted word outside strings and comments.
pub fn contains_word(sql: &str, word: &str) -> bool {
    tokenize(sql).iter().any(|token| token.is_word(word))
}

/// True when the text has positional parameters such as `$1` outside strings
/// and comments, as in statements normalized by `pg_stat_statements`.
pub fn has_parameters(sql: &str) -> bool {
    tokenize(sql)
        .iter()
        .any(|token| token.kind == TokenKind::Parameter)
}

/// The first unquoted word of a statement, uppercased.
pub fn first_word(sql: &str) -> Option<String> {
    tokenize(sql)
        .into_iter()
        .find(|token| !token.is_trivia())
        .filter(|token| token.kind == TokenKind::Word)
        .map(|token| token.text.to_ascii_uppercase())
}

/// True when both texts contain the same tokens apart from whitespace and the
/// case of unquoted words, which PostgreSQL folds. Literals, identifiers,
/// operators, and comments must match exactly.
pub fn same_tokens(left: &str, right: &str) -> bool {
    fn significant(sql: &str) -> impl Iterator<Item = (TokenKind, String)> + '_ {
        tokenize(sql)
            .into_iter()
            .filter(|token| token.kind != TokenKind::Whitespace)
            .map(|token| {
                let text = match token.kind {
                    TokenKind::Word => token.text.to_ascii_lowercase(),
                    TokenKind::LineComment | TokenKind::BlockComment => {
                        token.text.split_whitespace().collect::<Vec<_>>().join(" ")
                    }
                    _ => token.text.to_owned(),
                };
                (token.kind, text)
            })
    }
    significant(left).eq(significant(right))
}

pub fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

pub fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(byte, _)| byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(sql: &str) -> Vec<(TokenKind, &str)> {
        tokenize(sql)
            .into_iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| (t.kind, t.text))
            .collect()
    }

    fn statements(sql: &str) -> Vec<&str> {
        split_statements(sql)
            .into_iter()
            .map(|s| &sql[s.range])
            .collect()
    }

    #[test]
    fn literals_comments_and_identifiers_are_single_tokens() {
        use TokenKind::*;
        assert_eq!(
            kinds(
                "SELECT 'a;''b', E'c\\';d', $$x;y$$, $f$ $$ ; $f$, \"q;\"\"r\" -- c;\n/* a /* b; */ c */ x"
            ),
            [
                (Word, "SELECT"),
                (String, "'a;''b'"),
                (Punctuation, ","),
                (String, "E'c\\';d'"),
                (Punctuation, ","),
                (DollarString, "$$x;y$$"),
                (Punctuation, ","),
                (DollarString, "$f$ $$ ; $f$"),
                (Punctuation, ","),
                (QuotedIdentifier, "\"q;\"\"r\""),
                (LineComment, "-- c;"),
                (BlockComment, "/* a /* b; */ c */"),
                (Word, "x"),
            ]
        );
        assert_eq!(
            kinds("a$b $1 1.5e-3 x::int a->>'k' b--c"),
            [
                (Word, "a$b"),
                (Parameter, "$1"),
                (Number, "1.5e-3"),
                (Word, "x"),
                (Punctuation, "::"),
                (Word, "int"),
                (Word, "a"),
                (Operator, "->>"),
                (String, "'k'"),
                (Word, "b"),
                (LineComment, "--c"),
            ]
        );
        assert_eq!(
            kinds("'C:\\' AS p"),
            [(String, "'C:\\'"), (Word, "AS"), (Word, "p")]
        );
        assert_eq!(kinds("zażółć_1"), [(Word, "zażółć_1")]);
        assert_eq!(kinds("'open"), [(String, "'open")]);
        assert_eq!(kinds("$$open"), [(DollarString, "$$open")]);
    }

    #[test]
    fn splits_only_at_top_level_semicolons() {
        assert_eq!(
            statements(
                "SELECT ';'; -- only a comment;\n;\n SELECT $$;$$ ;SELECT (1;2);  /* tail */ "
            ),
            ["SELECT ';'", "SELECT $$;$$", "SELECT (1;2)"]
        );
        assert_eq!(
            statements(
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT CASE WHEN true THEN 1 END; SELECT 2; END; SELECT 3"
            ),
            [
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT CASE WHEN true THEN 1 END; SELECT 2; END",
                "SELECT 3"
            ]
        );
        assert_eq!(
            statements("BEGIN; SELECT 1; END;"),
            ["BEGIN", "SELECT 1", "END"]
        );
        assert!(statements("  -- nothing\n").is_empty());
    }

    #[test]
    fn finds_the_statement_at_the_cursor() {
        let sql = "SELECT 1;  \n\nSELECT 2;\nSELECT 3";
        let at = |cursor| statement_at(sql, cursor).map(|r| &sql[r]);
        assert_eq!(at(0), Some("SELECT 1"));
        assert_eq!(at(9), Some("SELECT 1"));
        assert_eq!(at(11), Some("SELECT 1"));
        assert_eq!(at(12), Some("SELECT 2"));
        assert_eq!(at(sql.len()), Some("SELECT 3"));
        assert_eq!(statement_at("", 0), None);
    }

    #[test]
    fn compares_tokens_ignoring_whitespace_and_keyword_case() {
        assert!(same_tokens(
            "select a,b from t -- c\n",
            "SELECT\n  a,\n  b\nFROM\n  t -- c"
        ));
        assert!(!same_tokens("select 'select x'", "SELECT 'SELECT x'"));
        assert!(!same_tokens("select a->>'k'", "SELECT a - > > 'k'"));
        assert!(!same_tokens("select \"Mixed\"", "SELECT \"MIXED\""));
        assert!(!same_tokens("select 1 - -1", "SELECT 1 --1"));
    }

    #[test]
    fn finds_words_outside_literals() {
        assert!(contains_word("ALTER ROLE app PASSWORD 'x'", "password"));
        assert!(!contains_word(
            "SELECT 'password' -- password\n",
            "password"
        ));
        assert_eq!(first_word("  -- c\n select 1").as_deref(), Some("SELECT"));
        assert_eq!(first_word("(SELECT 1)"), None);
        assert!(has_parameters(
            "SELECT * FROM t WHERE id = $1 AND x IN ($2, $3)"
        ));
        assert!(!has_parameters(
            "SELECT '$1', $$ $1 $$, \"$1\" -- $1\nFROM t"
        ));
    }

    #[test]
    fn converts_between_byte_and_char_offsets() {
        let text = "ąb€c";
        assert_eq!(char_to_byte(text, 2), 3);
        assert_eq!(byte_to_char(text, 3), 2);
        assert_eq!(char_to_byte(text, 99), text.len());
    }
}
