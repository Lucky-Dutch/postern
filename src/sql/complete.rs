//! Schema-aware SQL completion.
//!
//! Completion looks at the statement around the cursor: after `alias.` it offers
//! that table's columns, after `schema.` the schema's tables, after FROM or JOIN
//! tables, and elsewhere the columns of tables used in the statement, keywords,
//! tables, and functions. Table aliases are resolved from FROM, JOIN, UPDATE,
//! and INTO clauses. Nothing is offered inside strings or comments.

use super::lexer::{self, TokenKind};
use std::{collections::HashMap, ops::Range};

/// Database objects available for completion.
#[derive(Clone, Debug, Default)]
pub struct CompletionCatalog {
    /// Tables and views as (schema, name).
    pub relations: Vec<(String, String)>,
    /// Column names by (schema, table).
    pub columns: HashMap<(String, String), Vec<String>>,
    /// Functions as (schema, name).
    pub functions: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Column,
    Table,
    Schema,
    Function,
    Keyword,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub insert: String,
    pub kind: ItemKind,
    /// Extra detail such as the table a column belongs to.
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// Byte range replaced by the chosen item: the word being typed.
    pub replace: Range<usize>,
    pub items: Vec<CompletionItem>,
}

const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS",
    "IN",
    "EXISTS",
    "BETWEEN",
    "LIKE",
    "ILIKE",
    "AS",
    "ON",
    "USING",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "OUTER",
    "CROSS",
    "LATERAL",
    "NATURAL",
    "GROUP",
    "BY",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "FETCH",
    "DISTINCT",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "ALL",
    "ANY",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "INSERT",
    "INTO",
    "VALUES",
    "DEFAULT",
    "UPDATE",
    "SET",
    "DELETE",
    "RETURNING",
    "MERGE",
    "MATCHED",
    "CONFLICT",
    "DO",
    "NOTHING",
    "WITH",
    "RECURSIVE",
    "CREATE",
    "ALTER",
    "DROP",
    "TABLE",
    "VIEW",
    "MATERIALIZED",
    "INDEX",
    "UNIQUE",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "CONSTRAINT",
    "CHECK",
    "COLUMN",
    "ADD",
    "RENAME",
    "TO",
    "TYPE",
    "SCHEMA",
    "SEQUENCE",
    "FUNCTION",
    "PROCEDURE",
    "TRIGGER",
    "RETURNS",
    "LANGUAGE",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "SAVEPOINT",
    "RELEASE",
    "TRANSACTION",
    "GRANT",
    "REVOKE",
    "TRUNCATE",
    "VACUUM",
    "ANALYZE",
    "EXPLAIN",
    "COPY",
    "CAST",
    "COALESCE",
    "NULLIF",
    "TRUE",
    "FALSE",
    "ASC",
    "DESC",
    "NULLS",
    "FIRST",
    "LAST",
    "OVER",
    "PARTITION",
    "WINDOW",
    "FILTER",
    "WITHIN",
    "ROWS",
    "RANGE",
    "PRECEDING",
    "FOLLOWING",
    "CURRENT",
    "ROW",
    "UNBOUNDED",
    "SIMILAR",
    "ESCAPE",
    "COLLATE",
    "INTERVAL",
    "CURRENT_DATE",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "IF",
    "CASCADE",
    "RESTRICT",
    "OWNER",
    "TABLESPACE",
    "TEMPORARY",
    "UNLOGGED",
    "REPLACE",
];

/// Words after which a table name is expected.
const TABLE_CONTEXT: &[&str] = &["from", "join", "update", "into", "table", "truncate"];

/// Words that end a table reference list.
const CLAUSE_WORDS: &[&str] = &[
    "where",
    "join",
    "inner",
    "left",
    "right",
    "full",
    "cross",
    "on",
    "using",
    "group",
    "order",
    "having",
    "limit",
    "offset",
    "set",
    "values",
    "returning",
    "union",
    "intersect",
    "except",
    "window",
    "for",
    "natural",
    "lateral",
    "default",
    "select",
    "fetch",
];

struct TableReference {
    schema: Option<String>,
    table: String,
    alias: Option<String>,
}

/// Computes completions at byte offset `cursor`. `explicit` requests completion
/// even when nothing has been typed yet, as with Control-Space.
pub fn complete(
    sql: &str,
    cursor: usize,
    catalog: &CompletionCatalog,
    explicit: bool,
) -> Option<Completion> {
    let cursor = cursor.min(sql.len());
    if !sql.is_char_boundary(cursor) {
        return None;
    }
    let statement = lexer::statement_at(sql, cursor)
        .filter(|range| range.start <= cursor)
        .map_or(0..sql.len(), |range| {
            range.start..sql.len().min(range.end.max(cursor))
        });
    let tokens = lexer::tokenize(&sql[..cursor]);
    if let Some(last) = tokens.last()
        && matches!(
            last.kind,
            TokenKind::String
                | TokenKind::DollarString
                | TokenKind::LineComment
                | TokenKind::BlockComment
        )
    {
        return None;
    }
    let word_start = sql[..cursor]
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '$' || !c.is_ascii())
        .last()
        .map_or(cursor, |(index, _)| index);
    let prefix = &sql[word_start..cursor];
    if prefix.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let qualifier = qualifier_before(sql, word_start);
    if prefix.is_empty() && qualifier.is_none() && !explicit {
        return None;
    }
    let references = table_references(&sql[statement.clone()]);
    let previous = previous_word(&sql[statement.start..word_start.max(statement.start)]);
    let lower = prefix.to_lowercase();
    let matches = |name: &str| name.to_lowercase().starts_with(&lower);
    let mut items = Vec::new();
    let mut add = |label: String, insert: String, kind: ItemKind, detail: String| {
        if matches(&label)
            && !items
                .iter()
                .any(|item: &CompletionItem| item.label == label && item.kind == kind)
        {
            items.push(CompletionItem {
                label,
                insert,
                kind,
                detail,
            });
        }
    };
    if let Some(qualifier) = qualifier {
        let qualifier_lower = qualifier.to_lowercase();
        let target = references.iter().find(|reference| {
            reference
                .alias
                .as_ref()
                .is_some_and(|alias| alias.to_lowercase() == qualifier_lower)
                || reference.table.to_lowercase() == qualifier_lower
        });
        let tables: Vec<(String, String)> = match target {
            Some(reference) => resolve(catalog, reference.schema.as_deref(), &reference.table)
                .into_iter()
                .collect(),
            None => catalog
                .relations
                .iter()
                .filter(|(_, name)| name.to_lowercase() == qualifier_lower)
                .cloned()
                .collect(),
        };
        for key in &tables {
            for column in catalog.columns.get(key).into_iter().flatten() {
                add(
                    column.clone(),
                    identifier(column),
                    ItemKind::Column,
                    key.1.clone(),
                );
            }
        }
        for (schema, name) in catalog
            .relations
            .iter()
            .filter(|(schema, _)| schema.to_lowercase() == qualifier_lower)
        {
            add(
                name.clone(),
                identifier(name),
                ItemKind::Table,
                schema.clone(),
            );
        }
        for (schema, name) in catalog
            .functions
            .iter()
            .filter(|(schema, _)| schema.to_lowercase() == qualifier_lower)
        {
            add(
                format!("{name}()"),
                format!("{}(", identifier(name)),
                ItemKind::Function,
                schema.clone(),
            );
        }
    } else {
        let wants_table = previous
            .as_deref()
            .is_some_and(|word| TABLE_CONTEXT.contains(&word));
        let add_tables = |add: &mut dyn FnMut(String, String, ItemKind, String)| {
            for (schema, name) in &catalog.relations {
                let insert = if schema == "public" {
                    identifier(name)
                } else {
                    format!("{}.{}", identifier(schema), identifier(name))
                };
                add(name.clone(), insert, ItemKind::Table, schema.clone());
            }
            let mut schemas: Vec<&String> =
                catalog.relations.iter().map(|(schema, _)| schema).collect();
            schemas.sort();
            schemas.dedup();
            for schema in schemas {
                add(
                    schema.clone(),
                    identifier(schema),
                    ItemKind::Schema,
                    String::new(),
                );
            }
        };
        if wants_table {
            add_tables(&mut add);
        } else {
            for reference in &references {
                if let Some(key) = resolve(catalog, reference.schema.as_deref(), &reference.table) {
                    for column in catalog.columns.get(&key).into_iter().flatten() {
                        add(
                            column.clone(),
                            identifier(column),
                            ItemKind::Column,
                            key.1.clone(),
                        );
                    }
                }
            }
            for keyword in KEYWORDS {
                let insert = if prefix.chars().next().is_some_and(char::is_lowercase) {
                    keyword.to_lowercase()
                } else {
                    (*keyword).to_owned()
                };
                add(
                    (*keyword).to_owned(),
                    insert,
                    ItemKind::Keyword,
                    String::new(),
                );
            }
            add_tables(&mut add);
            for (schema, name) in &catalog.functions {
                add(
                    format!("{name}()"),
                    format!("{}(", identifier(name)),
                    ItemKind::Function,
                    schema.clone(),
                );
            }
        }
    }
    if items.is_empty() {
        return None;
    }
    // Columns of the statement's tables first, then keywords, tables, and
    // functions; the sort is stable, so columns keep their table order.
    items.sort_by_key(|item| rank(item.kind, previous.as_deref()));
    items.truncate(60);
    Some(Completion {
        replace: word_start..cursor,
        items,
    })
}

fn rank(kind: ItemKind, previous: Option<&str>) -> u8 {
    let table_context = previous.is_some_and(|word| TABLE_CONTEXT.contains(&word));
    match (kind, table_context) {
        (ItemKind::Table, true) => 0,
        (ItemKind::Schema, true) => 1,
        (ItemKind::Column, _) => 0,
        (ItemKind::Keyword, _) => 1,
        (ItemKind::Table, false) => 2,
        (ItemKind::Function, _) => 3,
        (ItemKind::Schema, false) => 4,
    }
}

/// An identifier as it must be written: unquoted when PostgreSQL would fold it to
/// the same name, quoted otherwise.
pub fn identifier(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(name));
    if plain {
        name.to_owned()
    } else {
        super::quote_ident(name)
    }
}

/// The identifier before a `.` that immediately precedes the word being typed.
fn qualifier_before(sql: &str, word_start: usize) -> Option<String> {
    let before = sql[..word_start].strip_suffix('.')?;
    let tokens = lexer::tokenize(before);
    let token = tokens.last()?;
    match token.kind {
        TokenKind::Word => Some(token.text.to_owned()),
        TokenKind::QuotedIdentifier => Some(unquote(token.text)),
        _ => None,
    }
}

fn unquote(text: &str) -> String {
    text.trim_start_matches("U&")
        .trim_matches('"')
        .replace("\"\"", "\"")
}

fn previous_word(text: &str) -> Option<String> {
    lexer::tokenize(text)
        .into_iter()
        .rev()
        .find(|token| !token.is_trivia())
        .filter(|token| token.kind == TokenKind::Word)
        .map(|token| token.text.to_lowercase())
}

fn resolve(
    catalog: &CompletionCatalog,
    schema: Option<&str>,
    table: &str,
) -> Option<(String, String)> {
    let table_lower = table.to_lowercase();
    catalog
        .relations
        .iter()
        .filter(|(s, name)| {
            name.to_lowercase() == table_lower
                && schema.is_none_or(|schema| s.to_lowercase() == schema.to_lowercase())
        })
        .min_by_key(|(s, _)| (s != "public", s.clone()))
        .cloned()
}

/// Tables referenced in a statement, with their aliases.
fn table_references(statement: &str) -> Vec<TableReference> {
    let tokens: Vec<_> = lexer::tokenize(statement)
        .into_iter()
        .filter(|token| !token.is_trivia())
        .collect();
    let name_of = |index: usize| -> Option<String> {
        let token = tokens.get(index)?;
        match token.kind {
            TokenKind::Word => Some(token.text.to_owned()),
            TokenKind::QuotedIdentifier => Some(unquote(token.text)),
            _ => None,
        }
    };
    let is_word = |index: usize, words: &[&str]| {
        tokens.get(index).is_some_and(|token| {
            token.kind == TokenKind::Word && words.contains(&token.text.to_lowercase().as_str())
        })
    };
    let mut references = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if !is_word(index, TABLE_CONTEXT) {
            index += 1;
            continue;
        }
        index += 1;
        loop {
            if is_word(index, &["only", "lateral"]) {
                index += 1;
            }
            let Some(first) = name_of(index) else {
                break;
            };
            if is_word(index, CLAUSE_WORDS) {
                break;
            }
            let (schema, table) = if tokens.get(index + 1).is_some_and(|t| t.text == ".")
                && let Some(second) = name_of(index + 2)
            {
                index += 3;
                (Some(first), second)
            } else {
                index += 1;
                (None, first)
            };
            if is_word(index, &["as"]) {
                index += 1;
            }
            let alias = if !is_word(index, CLAUSE_WORDS) && !is_word(index, KEYWORDS_LOWER) {
                name_of(index).inspect(|_| index += 1)
            } else {
                None
            };
            references.push(TableReference {
                schema,
                table,
                alias,
            });
            if tokens.get(index).is_some_and(|t| t.text == ",") {
                index += 1;
                continue;
            }
            break;
        }
    }
    references
}

/// Keywords that cannot be table aliases.
const KEYWORDS_LOWER: &[&str] = &[
    "where",
    "join",
    "on",
    "using",
    "group",
    "order",
    "having",
    "limit",
    "offset",
    "set",
    "values",
    "returning",
    "union",
    "intersect",
    "except",
    "window",
    "inner",
    "left",
    "right",
    "full",
    "cross",
    "natural",
    "select",
    "default",
    "for",
    "fetch",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> CompletionCatalog {
        let mut columns = HashMap::new();
        columns.insert(
            ("public".into(), "customers".into()),
            vec!["id".into(), "name".into(), "email".into()],
        );
        columns.insert(
            ("public".into(), "orders".into()),
            vec!["id".into(), "customer_id".into(), "total".into()],
        );
        columns.insert(
            ("sales".into(), "Leads".into()),
            vec!["Lead Id".into(), "status".into()],
        );
        CompletionCatalog {
            relations: vec![
                ("public".into(), "customers".into()),
                ("public".into(), "orders".into()),
                ("sales".into(), "Leads".into()),
            ],
            columns,
            functions: vec![("public".into(), "order_total".into())],
        }
    }

    fn labels(sql: &str) -> Vec<String> {
        let cursor = sql.find('|').expect("cursor marker");
        let text = sql.replacen('|', "", 1);
        complete(&text, cursor, &catalog(), false)
            .map(|c| c.items.into_iter().map(|i| i.label).collect())
            .unwrap_or_default()
    }

    #[test]
    fn completes_columns_of_aliased_tables() {
        assert_eq!(
            labels("SELECT o.| FROM orders o"),
            ["id", "customer_id", "total"]
        );
        assert_eq!(
            labels("SELECT c.na| FROM customers AS c JOIN orders o ON o.customer_id = c.id"),
            ["name"]
        );
        assert_eq!(
            labels("SELECT l.| FROM sales.\"Leads\" l"),
            ["Lead Id", "status"]
        );
        let completion =
            complete("SELECT l. FROM sales.\"Leads\" l", 9, &catalog(), false).unwrap();
        assert_eq!(completion.items[0].insert, "\"Lead Id\"");
    }

    #[test]
    fn completes_tables_after_from_and_schema_qualifiers() {
        assert_eq!(labels("SELECT * FROM cu|"), ["customers"]);
        assert_eq!(labels("SELECT * FROM sales.|"), ["Leads"]);
        let completion = complete("SELECT * FROM Le", 16, &catalog(), false).unwrap();
        assert_eq!(completion.items[0].insert, "sales.\"Leads\"");
    }

    #[test]
    fn mixes_columns_keywords_and_functions_elsewhere() {
        let items = labels("SELECT * FROM orders WHERE to|");
        assert_eq!(items, ["total", "TO"]);
        let items = labels("SELECT ord|");
        assert!(
            items.contains(&"ORDER".to_string()) && items.contains(&"order_total()".to_string())
        );
        let completion = complete("select fr", 9, &catalog(), false).unwrap();
        assert_eq!(completion.items[0].insert, "from");
        assert_eq!(completion.replace, 7..9);
    }

    #[test]
    fn stays_quiet_in_literals_and_without_a_prefix() {
        assert!(labels("SELECT 'cu|").is_empty());
        assert!(labels("SELECT 1 -- cu|").is_empty());
        assert!(labels("SELECT |").is_empty());
        assert!(complete("SELECT ", 7, &catalog(), true).is_some());
        assert!(labels("SELECT 12|").is_empty());
    }

    #[test]
    fn quotes_identifiers_only_when_needed() {
        assert_eq!(identifier("total"), "total");
        assert_eq!(identifier("Total"), "\"Total\"");
        assert_eq!(identifier("order"), "\"order\"");
        assert_eq!(identifier("a b"), "\"a b\"");
    }
}
