use crate::models::*;
use anyhow::{Result, ensure};

pub mod complete;
pub mod lexer;

pub fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Formats SQL and uppercases keywords. String and dollar-quoted literals are
/// replaced by placeholders while the formatter runs, because it does not know
/// PostgreSQL's dollar quoting or standard-conforming backslashes. The result is
/// rejected unless it has exactly the same tokens as the input, apart from
/// whitespace and keyword case.
pub fn format_sql(sql: &str) -> Result<String> {
    use lexer::TokenKind;
    let mut marker = String::from("pgm_literal_");
    while sql.contains(&marker) {
        marker.push('x');
    }
    let mut protected = String::with_capacity(sql.len());
    let mut literals = Vec::new();
    for token in lexer::tokenize(sql) {
        if matches!(token.kind, TokenKind::String | TokenKind::DollarString) {
            protected.push_str(&format!("'{marker}{}'", literals.len()));
            literals.push(token.text);
        } else {
            protected.push_str(token.text);
        }
    }
    let mut formatted = sqlformat::format(
        &protected,
        &sqlformat::QueryParams::None,
        &sqlformat::FormatOptions {
            uppercase: Some(true),
            ..Default::default()
        },
    );
    for (index, literal) in literals.iter().enumerate() {
        let placeholder = format!("'{marker}{index}'");
        ensure!(
            formatted.matches(&placeholder).count() == 1,
            "Formatting was not applied because the formatter changed a string literal."
        );
        formatted = formatted.replacen(&placeholder, literal, 1);
    }
    ensure!(
        lexer::same_tokens(sql, &formatted),
        "Formatting was not applied because the formatter would change more than whitespace and keyword case in this SQL."
    );
    Ok(formatted)
}

/// One page of a browse request. Rows are filtered, sorted, and limited first, and
/// only the page's rows are cast to text: casting every row before sorting would
/// convert the whole table. The outer ORDER BY keeps the page in order.
pub fn browse_sql(request: &BrowseRequest) -> Result<(String, Vec<String>)> {
    ensure!(
        (1..=500).contains(&request.page_size),
        "Page size must be between 1 and 500."
    );
    let offset = request
        .page
        .checked_mul(request.page_size)
        .ok_or_else(|| anyhow::anyhow!("Page offset overflow."))?;
    let query = BrowseQuery::new(request)?;
    let sql = format!(
        "SELECT {} FROM (SELECT * FROM {}{}{} LIMIT {} OFFSET {offset}) AS _pgm_row{}",
        query.projection,
        query.source,
        query.filter,
        query.order,
        request.page_size + 1,
        query.order
    );
    Ok((sql, query.params))
}

/// The filtered and sorted SELECT of a browse request without pagination, for
/// exporting every row. Every column is cast to text so values keep PostgreSQL's
/// exact representation.
pub fn select_sql(request: &BrowseRequest) -> Result<(String, Vec<String>)> {
    let query = BrowseQuery::new(request)?;
    let sql = format!(
        "SELECT {} FROM {}{}{}",
        query.projection, query.source, query.filter, query.order
    );
    Ok((sql, query.params))
}

/// The parts of a browse query. Expressions refer to the table as `_pgm_row`.
struct BrowseQuery {
    projection: String,
    source: String,
    filter: String,
    order: String,
    params: Vec<String>,
}

impl BrowseQuery {
    fn new(request: &BrowseRequest) -> Result<Self> {
        ensure!(
            !request.columns.is_empty(),
            "This relation has no visible columns."
        );
        let projection = request
            .columns
            .iter()
            .map(|c| format!("_pgm_row.{}::text", quote_ident(&c.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let mut params = Vec::new();
        let filter = filter_predicate(&request.filter, &request.columns, &mut params)?
            .map(|predicate| format!(" WHERE {predicate}"))
            .unwrap_or_default();
        // Without an explicit choice, NULL placement follows PostgreSQL's default
        // (last when ascending, first when descending), which matches plain btree
        // indexes in both directions, so sorting an indexed column reads the index.
        let nulls = match request.nulls {
            NullsOrder::Default => "",
            NullsOrder::First => " NULLS FIRST",
            NullsOrder::Last => " NULLS LAST",
        };
        let mut order = Vec::new();
        for sort in &request.sort {
            let column = request
                .columns
                .iter()
                .find(|c| c.name == sort.name)
                .ok_or_else(|| anyhow::anyhow!("Unknown sort column."))?;
            let expression = if column.sortable {
                format!("_pgm_row.{}", quote_ident(&column.name))
            } else {
                format!("_pgm_row.{}::text", quote_ident(&column.name))
            };
            order.push(format!(
                "{expression} {}{nulls}",
                if sort.descending { "DESC" } else { "ASC" }
            ));
        }
        for column in request.columns.iter().filter(|c| c.primary_key) {
            if !request.sort.iter().any(|s| s.name == column.name) {
                order.push(format!("_pgm_row.{}", quote_ident(&column.name)));
            }
        }
        let order = if order.is_empty() {
            String::new()
        } else {
            format!(" ORDER BY {}", order.join(", "))
        };
        Ok(Self {
            projection,
            source: format!("{} AS _pgm_row", request.relation.qualified_name()),
            filter,
            order,
            params,
        })
    }
}

/// Builds the WHERE predicate for a filter set. Values are always parameters.
/// Equality, lists, and ordering comparisons cast the value to the column type,
/// so numbers and dates compare by value and indexes can be used. Types without
/// a btree ordering, such as json, compare their exact text instead. Text
/// operators such as contains always compare the textual representation.
pub fn filter_predicate(
    filter: &FilterSet,
    columns: &[Column],
    params: &mut Vec<String>,
) -> Result<Option<String>> {
    let mut predicates = Vec::new();
    for condition in &filter.conditions {
        let column = columns
            .iter()
            .find(|c| c.name == condition.column)
            .ok_or_else(|| anyhow::anyhow!("Unknown filter column."))?;
        let name = format!("_pgm_row.{}", quote_ident(&column.name));
        let mut value = |text: &str| {
            params.push(text.to_owned());
            format!("${}::text", params.len())
        };
        use FilterOperator::*;
        let predicate = match condition.operator {
            IsNull => format!("{name} IS NULL"),
            IsNotNull => format!("{name} IS NOT NULL"),
            Contains => format!(
                "strpos(lower({name}::text), lower({})) > 0",
                value(&condition.value)
            ),
            NotContains => format!(
                "strpos(lower({name}::text), lower({})) = 0",
                value(&condition.value)
            ),
            StartsWith => format!(
                "starts_with(lower({name}::text), lower({}))",
                value(&condition.value)
            ),
            EndsWith => {
                let slot = value(&condition.value);
                format!("right(lower({name}::text), length({slot})) = lower({slot})")
            }
            Equal if column.sortable => {
                format!("{name} = ({})::{}", value(&condition.value), column.cast())
            }
            Equal => format!("{name}::text = {}", value(&condition.value)),
            NotEqual if column.sortable => format!(
                "{name} IS DISTINCT FROM ({})::{}",
                value(&condition.value),
                column.cast()
            ),
            NotEqual => format!("{name}::text IS DISTINCT FROM {}", value(&condition.value)),
            Less | LessOrEqual | Greater | GreaterOrEqual => {
                let operator = match condition.operator {
                    Less => "<",
                    LessOrEqual => "<=",
                    Greater => ">",
                    _ => ">=",
                };
                format!(
                    "{name} {operator} ({})::{}",
                    value(&condition.value),
                    column.cast()
                )
            }
            Matches => format!("{name}::text ~* {}", value(&condition.value)),
            InList | NotInList => {
                let values: Vec<&str> = condition.value.lines().collect();
                ensure!(
                    !values.is_empty() && values.len() <= 500,
                    "Enter between 1 and 500 values, one per line."
                );
                let typed = column.sortable;
                let slots = values
                    .into_iter()
                    .map(|text| {
                        let slot = value(text);
                        if typed {
                            format!("({slot})::{}", column.cast())
                        } else {
                            slot
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let negation = if condition.operator == NotInList {
                    "NOT "
                } else {
                    ""
                };
                let target = if typed {
                    name.clone()
                } else {
                    format!("{name}::text")
                };
                format!("{target} {negation}IN ({slots})")
            }
        };
        predicates.push(predicate);
    }
    let mut clauses = Vec::new();
    if !predicates.is_empty() {
        let joiner = if filter.any { " OR " } else { " AND " };
        clauses.push(format!("({})", predicates.join(joiner)));
    }
    let custom = filter.custom_sql.trim();
    if !custom.is_empty() {
        validate_expression(custom)?;
        clauses.push(format!("({custom})"));
    }
    Ok((!clauses.is_empty()).then(|| clauses.join(" AND ")))
}

/// Accepts a user-written SQL expression for a WHERE clause only when it cannot
/// escape its parentheses or end the statement.
fn validate_expression(expression: &str) -> Result<()> {
    let mut depth = 0i32;
    for token in lexer::tokenize(expression) {
        match token.kind {
            lexer::TokenKind::Semicolon => {
                anyhow::bail!("A custom filter cannot contain a semicolon.")
            }
            lexer::TokenKind::LineComment => {
                anyhow::bail!("A custom filter cannot contain a -- comment.")
            }
            lexer::TokenKind::String | lexer::TokenKind::QuotedIdentifier
                if token.text.len() < 2 || !token.text.ends_with(['\'', '"']) =>
            {
                anyhow::bail!("A custom filter has an unterminated quote.")
            }
            lexer::TokenKind::BlockComment if !token.text.ends_with("*/") => {
                anyhow::bail!("A custom filter has an unterminated comment.")
            }
            _ => {}
        }
        match token.text {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {}
        }
        ensure!(depth >= 0, "A custom filter has unbalanced parentheses.");
    }
    ensure!(depth == 0, "A custom filter has unbalanced parentheses.");
    Ok(())
}

/// Rejects a SQL fragment written by the user, such as a type, default, or
/// constraint definition, when it could end the statement or is unterminated.
fn validate_fragment(label: &str, fragment: &str) -> Result<()> {
    ensure!(!fragment.trim().is_empty(), "{label} is required.");
    validate_expression(fragment).map_err(|error| {
        anyhow::anyhow!(
            "{label}: {}",
            error.to_string().replace("A custom filter", "It")
        )
    })
}

fn quote_name(label: &str, name: &str) -> Result<String> {
    ensure!(!name.trim().is_empty(), "{label} is required.");
    ensure!(!name.contains('\0'), "{label} contains a NUL character.");
    Ok(quote_ident(name.trim()))
}

pub fn relation_action_sql(relation: &Relation, action: &RelationAction) -> Result<String> {
    let name = relation.qualified_name();
    let keyword = relation.ddl_keyword();
    Ok(match action {
        RelationAction::Truncate {
            cascade,
            restart_identity,
        } => {
            ensure!(relation.is_table(), "Only tables can be truncated.");
            format!(
                "TRUNCATE TABLE {name}{}{}",
                if *restart_identity {
                    " RESTART IDENTITY"
                } else {
                    ""
                },
                if *cascade { " CASCADE" } else { "" }
            )
        }
        RelationAction::Drop { cascade } => format!(
            "DROP {keyword} {name}{}",
            if *cascade { " CASCADE" } else { "" }
        ),
        RelationAction::Rename(new_name) => format!(
            "ALTER {keyword} {name} RENAME TO {}",
            quote_name("The new name", new_name)?
        ),
        RelationAction::VacuumAnalyze => format!("VACUUM (ANALYZE) {name}"),
        RelationAction::Analyze => format!("ANALYZE {name}"),
        RelationAction::Reindex => format!("REINDEX TABLE {name}"),
        RelationAction::RefreshMaterializedView => {
            ensure!(
                relation.kind == "m",
                "Only materialized views can be refreshed."
            );
            format!("REFRESH MATERIALIZED VIEW {name}")
        }
    })
}

pub fn drop_routine_sql(routine: &Routine, cascade: bool) -> String {
    format!(
        "DROP {} {}{}",
        if routine.procedure {
            "PROCEDURE"
        } else {
            "FUNCTION"
        },
        routine.signature(),
        if cascade { " CASCADE" } else { "" }
    )
}

pub fn create_schema_sql(name: &str) -> Result<String> {
    Ok(format!(
        "CREATE SCHEMA {}",
        quote_name("Schema name", name)?
    ))
}

pub fn create_database_sql(name: &str) -> Result<String> {
    Ok(format!(
        "CREATE DATABASE {}",
        quote_name("Database name", name)?
    ))
}

pub fn drop_database_sql(name: &str) -> Result<String> {
    Ok(format!(
        "DROP DATABASE {}",
        quote_name("Database name", name)?
    ))
}

fn column_definition(column: &ColumnDraft) -> Result<String> {
    let mut definition = format!(
        "{} {}",
        quote_name("Column name", &column.name)?,
        column.data_type.trim()
    );
    validate_fragment(&format!("Type of {}", column.name), &column.data_type)?;
    if !column.nullable {
        definition.push_str(" NOT NULL");
    }
    if !column.default.trim().is_empty() {
        validate_fragment(&format!("Default of {}", column.name), &column.default)?;
        definition.push_str(&format!(" DEFAULT {}", column.default.trim()));
    }
    Ok(definition)
}

fn comment_sql(target: &str, comment: &str) -> String {
    format!(
        "COMMENT ON {target} IS {}",
        if comment.trim().is_empty() {
            "NULL".to_owned()
        } else {
            quote_literal(comment)
        }
    )
}

fn constraint_sql(constraint: &ConstraintDraft) -> Result<String> {
    validate_fragment("Constraint definition", &constraint.definition)?;
    Ok(if constraint.name.trim().is_empty() {
        constraint.definition.trim().to_owned()
    } else {
        format!(
            "CONSTRAINT {} {}",
            quote_ident(constraint.name.trim()),
            constraint.definition.trim()
        )
    })
}

fn index_sql(table: &str, index: &IndexDraft) -> Result<String> {
    validate_fragment("Index columns", &index.columns)?;
    let method = if index.method.trim().is_empty() {
        "btree"
    } else {
        index.method.trim()
    };
    ensure!(
        method
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "Index method must be a plain name such as btree or gin."
    );
    let columns = index.columns.trim();
    let columns = if columns.starts_with('(') {
        columns.to_owned()
    } else {
        format!("({columns})")
    };
    Ok(format!(
        "CREATE {}INDEX {}ON {table} USING {method} {columns}",
        if index.unique { "UNIQUE " } else { "" },
        if index.name.trim().is_empty() {
            String::new()
        } else {
            format!("{} ", quote_ident(index.name.trim()))
        }
    ))
}

/// CREATE TABLE with columns, primary key, constraints, then indexes and comments.
pub fn create_table_sql(draft: &TableDraft) -> Result<Vec<String>> {
    let table = format!(
        "{}.{}",
        quote_name("Schema", &draft.schema)?,
        quote_name("Table name", &draft.name)?
    );
    let columns: Vec<&ColumnDraft> = draft.columns.iter().filter(|c| !c.dropped).collect();
    ensure!(!columns.is_empty(), "Add at least one column.");
    let mut lines = columns
        .iter()
        .map(|c| column_definition(c).map(|d| format!("    {d}")))
        .collect::<Result<Vec<_>>>()?;
    let key: Vec<String> = columns
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| quote_ident(c.name.trim()))
        .collect();
    if !key.is_empty() {
        lines.push(format!("    PRIMARY KEY ({})", key.join(", ")));
    }
    for constraint in draft.constraints.iter().filter(|c| !c.dropped) {
        lines.push(format!("    {}", constraint_sql(constraint)?));
    }
    let mut statements = vec![format!("CREATE TABLE {table} (\n{}\n)", lines.join(",\n"))];
    for index in draft.indexes.iter().filter(|i| !i.dropped) {
        statements.push(index_sql(&table, index)?);
    }
    if !draft.comment.trim().is_empty() {
        statements.push(comment_sql(&format!("TABLE {table}"), &draft.comment));
    }
    for column in columns.iter().filter(|c| !c.comment.trim().is_empty()) {
        statements.push(comment_sql(
            &format!("COLUMN {table}.{}", quote_ident(column.name.trim())),
            &column.comment,
        ));
    }
    Ok(statements)
}

/// The statements that turn `original` into `edited`, in an order that is valid
/// in one transaction: drops first, then column changes, additions, comments,
/// and finally the table rename.
pub fn alter_table_sql(original: &TableDraft, edited: &TableDraft) -> Result<Vec<String>> {
    let table = format!(
        "{}.{}",
        quote_ident(&original.schema),
        quote_ident(&original.name)
    );
    let mut statements = Vec::new();
    for constraint in &edited.constraints {
        if let (Some(name), true) = (&constraint.original, constraint.dropped) {
            statements.push(format!(
                "ALTER TABLE {table} DROP CONSTRAINT {}",
                quote_ident(name)
            ));
        }
    }
    for index in &edited.indexes {
        if let (Some(name), true) = (&index.original, index.dropped) {
            statements.push(format!(
                "DROP INDEX {}.{}",
                quote_ident(&original.schema),
                quote_ident(name)
            ));
        }
    }
    for column in &edited.columns {
        if let (Some(name), true) = (&column.original, column.dropped) {
            statements.push(format!(
                "ALTER TABLE {table} DROP COLUMN {}",
                quote_ident(name)
            ));
        }
    }
    for column in edited.columns.iter().filter(|c| !c.dropped) {
        let Some(name) = &column.original else {
            continue;
        };
        let before = original
            .columns
            .iter()
            .find(|c| c.original.as_ref() == Some(name))
            .ok_or_else(|| {
                anyhow::anyhow!("Column {name} no longer exists. Reload the structure.")
            })?;
        let current = quote_name("Column name", &column.name)?;
        if column.name.trim() != name {
            statements.push(format!(
                "ALTER TABLE {table} RENAME COLUMN {} TO {current}",
                quote_ident(name)
            ));
        }
        if column.data_type.trim() != before.data_type.trim() {
            validate_fragment(&format!("Type of {}", column.name), &column.data_type)?;
            let data_type = column.data_type.trim();
            statements.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {current} TYPE {data_type} USING {current}::{data_type}"
            ));
        }
        if column.nullable != before.nullable {
            statements.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {current} {} NOT NULL",
                if column.nullable { "DROP" } else { "SET" }
            ));
        }
        if !column.generated && column.default.trim() != before.default.trim() {
            if column.default.trim().is_empty() {
                statements.push(format!(
                    "ALTER TABLE {table} ALTER COLUMN {current} DROP DEFAULT"
                ));
            } else {
                validate_fragment(&format!("Default of {}", column.name), &column.default)?;
                statements.push(format!(
                    "ALTER TABLE {table} ALTER COLUMN {current} SET DEFAULT {}",
                    column.default.trim()
                ));
            }
        }
        if column.comment.trim() != before.comment.trim() {
            statements.push(comment_sql(
                &format!("COLUMN {table}.{current}"),
                &column.comment,
            ));
        }
    }
    for column in edited
        .columns
        .iter()
        .filter(|c| c.original.is_none() && !c.dropped)
    {
        statements.push(format!(
            "ALTER TABLE {table} ADD COLUMN {}",
            column_definition(column)?
        ));
        if !column.comment.trim().is_empty() {
            statements.push(comment_sql(
                &format!("COLUMN {table}.{}", quote_ident(column.name.trim())),
                &column.comment,
            ));
        }
    }
    for constraint in edited
        .constraints
        .iter()
        .filter(|c| c.original.is_none() && !c.dropped)
    {
        statements.push(format!(
            "ALTER TABLE {table} ADD {}",
            constraint_sql(constraint)?
        ));
    }
    for index in edited
        .indexes
        .iter()
        .filter(|i| i.original.is_none() && !i.dropped)
    {
        statements.push(index_sql(&table, index)?);
    }
    if edited.comment.trim() != original.comment.trim() {
        statements.push(comment_sql(&format!("TABLE {table}"), &edited.comment));
    }
    if edited.name.trim() != original.name {
        statements.push(format!(
            "ALTER TABLE {table} RENAME TO {}",
            quote_name("Table name", &edited.name)?
        ));
    }
    Ok(statements)
}

/// `DROP INDEX CONCURRENTLY`, which does not block reads or writes. It cannot run
/// inside a transaction block. Partitioned indexes cannot be dropped
/// concurrently, so they get a plain `DROP INDEX`.
pub fn drop_index_sql(schema: &str, index: &str, partitioned: bool) -> String {
    format!(
        "DROP INDEX {}{}.{}",
        if partitioned { "" } else { "CONCURRENTLY " },
        quote_ident(schema),
        quote_ident(index)
    )
}

/// An index on a foreign key's columns. Partitioned tables cannot build indexes
/// concurrently.
pub fn foreign_key_index_sql(
    schema: &str,
    table: &str,
    columns: &[String],
    partitioned: bool,
) -> String {
    format!(
        "CREATE INDEX {}ON {}.{} ({})",
        if partitioned { "" } else { "CONCURRENTLY " },
        quote_ident(schema),
        quote_ident(table),
        columns
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Splits a library list setting such as `shared_preload_libraries` the way
/// PostgreSQL does: commas separate elements, surrounding whitespace is ignored,
/// and double quotes protect commas and spaces, with `""` for a quote.
pub fn parse_library_list(value: &str) -> Vec<String> {
    let mut libraries = Vec::new();
    let mut chars = value.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            break;
        }
        let mut item = String::new();
        if chars.next_if_eq(&'"').is_some() {
            while let Some(c) = chars.next() {
                if c == '"' {
                    if chars.next_if_eq(&'"').is_some() {
                        item.push('"');
                    } else {
                        break;
                    }
                } else {
                    item.push(c);
                }
            }
            while chars.next_if(|c| *c != ',').is_some() {}
        } else {
            while let Some(c) = chars.next_if(|c| *c != ',') {
                item.push(c);
            }
            item.truncate(item.trim_end().len());
        }
        if !item.is_empty() {
            libraries.push(item);
        }
        if chars.next().is_none() {
            break;
        }
    }
    libraries
}

/// Formats libraries as a setting value, quoting elements that need it.
pub fn format_library_list(libraries: &[String]) -> String {
    libraries
        .iter()
        .map(|library| {
            if library.contains([',', '"']) || library.contains(char::is_whitespace) {
                format!("\"{}\"", library.replace('"', "\"\""))
            } else {
                library.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// True when the list loads the named library, written as a name, a
/// `$libdir/` path, or a file with its extension.
pub fn loads_library(libraries: &[String], name: &str) -> bool {
    libraries.iter().any(|library| {
        let file = library.rsplit('/').next().unwrap_or(library);
        let stem = file
            .strip_suffix(".so")
            .or_else(|| file.strip_suffix(".dylib"))
            .unwrap_or(file);
        stem == name
    })
}

/// The list with `name` appended unless it is already loaded.
pub fn with_library(libraries: &[String], name: &str) -> Vec<String> {
    let mut result = libraries.to_vec();
    if !loads_library(libraries, name) {
        result.push(name.to_owned());
    }
    result
}

/// Writes `shared_preload_libraries` to `postgresql.auto.conf`. Each library is a
/// separate literal, because PostgreSQL would quote a single comma-separated
/// literal as one library name. An empty list removes the setting.
pub fn alter_preload_sql(libraries: &[String]) -> String {
    if libraries.is_empty() {
        "ALTER SYSTEM RESET shared_preload_libraries".to_owned()
    } else {
        format!(
            "ALTER SYSTEM SET shared_preload_libraries = {}",
            libraries
                .iter()
                .map(|library| quote_literal(library))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// A guarded data change: the statement, its text parameters, and the number of
/// rows it must affect. Any other count rolls the change back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mutation {
    pub sql: String,
    pub params: Vec<Option<String>>,
    pub expected: u64,
}

/// UPDATE of one row identified by its complete original values.
pub fn update_row_sql(
    target: &MutationTarget,
    original: &[Option<String>],
    changes: &[(usize, Option<String>)],
) -> Result<Mutation> {
    ensure!(
        target.relation.is_table(),
        "Only base tables can be edited."
    );
    ensure!(!changes.is_empty(), "There are no changes to save.");
    let mut params = Vec::new();
    let mut assignments = Vec::new();
    for (position, (index, value)) in changes.iter().enumerate() {
        ensure!(
            !changes[..position].iter().any(|(other, _)| other == index),
            "A column is changed twice in one row."
        );
        let column = target
            .columns
            .get(*index)
            .ok_or_else(|| anyhow::anyhow!("Column no longer exists. Refresh the table."))?;
        ensure!(!column.generated, "Generated columns cannot be edited.");
        ensure!(
            column.nullable || value.is_some(),
            "{} does not allow NULL.",
            column.name
        );
        params.push(value.clone());
        assignments.push(format!(
            "{} = (${}::text)::{}",
            quote_ident(&column.name),
            params.len(),
            column.cast()
        ));
    }
    let predicate = mutation_predicate(&target.columns, original, &mut params)?;
    Ok(Mutation {
        sql: format!(
            "UPDATE {} SET {} WHERE {predicate}",
            target.relation.qualified_name(),
            assignments.join(", ")
        ),
        params,
        expected: 1,
    })
}

/// One DELETE for all selected rows, each identified by its complete original values.
pub fn delete_rows_sql(
    target: &MutationTarget,
    originals: &[Vec<Option<String>>],
) -> Result<Mutation> {
    ensure!(!originals.is_empty(), "Select at least one row to delete.");
    ensure!(
        target.relation.is_table(),
        "Only base tables can be edited."
    );
    let mut params = vec![];
    let predicates = originals
        .iter()
        .map(|original| {
            mutation_predicate(&target.columns, original, &mut params)
                .map(|predicate| format!("({predicate})"))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        params.len() <= u16::MAX as usize,
        "Selection has too many values. Select fewer rows and try again."
    );
    Ok(Mutation {
        sql: format!(
            "DELETE FROM {} WHERE {}",
            target.relation.qualified_name(),
            predicates.join(" OR ")
        ),
        params,
        expected: originals.len() as u64,
    })
}

pub fn insert_row_sql(target: &MutationTarget, values: &[InsertValue]) -> Result<Mutation> {
    ensure!(
        target.relation.is_table(),
        "Only base tables can be edited."
    );
    ensure!(
        values.len() == target.columns.len(),
        "Table structure changed. Refresh before inserting."
    );
    let mut names = Vec::new();
    let mut expressions = Vec::new();
    let mut params = Vec::new();
    for (column, value) in target.columns.iter().zip(values) {
        if *value == InsertValue::Default {
            continue;
        }
        ensure!(!column.generated, "Generated columns cannot be assigned.");
        ensure!(
            column.nullable || *value != InsertValue::Null,
            "{} does not allow NULL.",
            column.name
        );
        names.push(quote_ident(&column.name));
        params.push(match value {
            InsertValue::Text(s) => Some(s.clone()),
            _ => None,
        });
        expressions.push(format!("(${}::text)::{}", params.len(), column.cast()));
    }
    let sql = if names.is_empty() {
        format!(
            "INSERT INTO {} DEFAULT VALUES",
            target.relation.qualified_name()
        )
    } else {
        format!(
            "INSERT INTO {} ({}) VALUES ({})",
            target.relation.qualified_name(),
            names.join(", "),
            expressions.join(", ")
        )
    };
    Ok(Mutation {
        sql,
        params,
        expected: 1,
    })
}

/// Identifies a row by its primary key, compared with the column's own equality so
/// the key index is used, and by every original value, compared as text, so a
/// concurrent change to any column is detected.
fn mutation_predicate(
    columns: &[Column],
    original: &[Option<String>],
    params: &mut Vec<Option<String>>,
) -> Result<String> {
    ensure!(
        columns.iter().any(|c| c.primary_key),
        "Editing requires a primary key."
    );
    ensure!(
        columns.len() == original.len(),
        "Table structure changed. Refresh before editing."
    );
    let mut parts = Vec::new();
    for (column, value) in columns.iter().zip(original) {
        if column.primary_key
            && let Some(value) = value
        {
            params.push(Some(value.clone()));
            parts.push(format!(
                "{} = (${}::text)::{}",
                quote_ident(&column.name),
                params.len(),
                column.cast()
            ));
        }
    }
    for (column, value) in columns.iter().zip(original) {
        params.push(value.clone());
        parts.push(format!(
            "{}::text IS NOT DISTINCT FROM ${}::text",
            quote_ident(&column.name),
            params.len()
        ));
    }
    Ok(parts.join(" AND "))
}

pub fn markdown(result: &QueryResult) -> String {
    markdown_rows(&result.columns, &result.rows.iter().collect::<Vec<_>>())
}

pub fn markdown_rows(columns: &[String], rows: &[&Row]) -> String {
    fn escape(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('|', "\\|")
            .replace('\n', "<br>")
            .replace('\r', "")
    }
    let line = |values: Vec<String>| format!("| {} |\n", values.join(" | "));
    let mut out = line(columns.iter().map(|c| escape(c)).collect());
    out += &line(columns.iter().map(|_| "---".into()).collect());
    for row in rows {
        out += &line(
            row.iter()
                .map(|v| escape(v.as_deref().unwrap_or("NULL")))
                .collect(),
        );
    }
    out
}

pub fn csv(result: &QueryResult) -> String {
    let mut out = csv_line(result.columns.iter().map(|c| Some(c.as_str())));
    for row in &result.rows {
        out += &csv_line(row.iter().map(Option::as_deref));
    }
    out
}

/// One CSV record. NULL is an unquoted empty field and an empty string is a
/// quoted empty field, matching PostgreSQL's CSV format.
pub fn csv_line<'a>(values: impl Iterator<Item = Option<&'a str>>) -> String {
    let fields: Vec<String> = values
        .map(|value| {
            value
                .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
                .unwrap_or_default()
        })
        .collect();
    fields.join(",") + "\r\n"
}

pub fn csv_rows(columns: &[String], rows: &[&Row]) -> String {
    let mut out = csv_line(columns.iter().map(|c| Some(c.as_str())));
    for row in rows {
        out += &csv_line(row.iter().map(Option::as_deref));
    }
    out
}

/// Splits one CSV record (a header line) into fields, honoring double quotes.
pub fn parse_csv_record(line: &str, delimiter: char) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut quoted = false;
    let mut chars = line.trim_end_matches(['\r', '\n']).chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                fields.last_mut().expect("one field").push('"');
            }
            '"' => quoted = !quoted,
            c if c == delimiter && !quoted => fields.push(String::new()),
            c => fields.last_mut().expect("one field").push(c),
        }
    }
    fields
}

/// The delimiter that splits a header line into the most fields.
pub fn guess_delimiter(line: &str) -> char {
    [',', ';', '\t', '|']
        .into_iter()
        .max_by_key(|d| parse_csv_record(line, *d).len())
        .unwrap_or(',')
}

/// `COPY ... FROM STDIN` for CSV data in the given column order. NULL is an
/// unquoted empty field, as in PostgreSQL's CSV format.
pub fn copy_from_csv_sql(
    relation: &Relation,
    columns: &[String],
    header: bool,
    delimiter: char,
) -> String {
    format!(
        "COPY {} ({}) FROM STDIN WITH (FORMAT csv, HEADER {}, DELIMITER {})",
        relation.qualified_name(),
        columns
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", "),
        if header { "true" } else { "false" },
        quote_literal(&delimiter.to_string())
    )
}

/// Tab-separated rows in PostgreSQL's COPY text format: `\N` is NULL and
/// backslash, tab, newline, and carriage return are escaped. Pasting this text
/// back into a grid restores the exact values.
pub fn tsv_rows(columns: Option<&[String]>, rows: &[&Row]) -> String {
    fn escape(value: Option<&str>) -> String {
        match value {
            None => "\\N".into(),
            Some(text) => text
                .replace('\\', "\\\\")
                .replace('\t', "\\t")
                .replace('\n', "\\n")
                .replace('\r', "\\r"),
        }
    }
    let mut out = String::new();
    if let Some(columns) = columns {
        out += &columns
            .iter()
            .map(|c| escape(Some(c)))
            .collect::<Vec<_>>()
            .join("\t");
        out.push('\n');
    }
    for row in rows {
        out += &row
            .iter()
            .map(|v| escape(v.as_deref()))
            .collect::<Vec<_>>()
            .join("\t");
        out.push('\n');
    }
    out
}

/// Parses COPY text format, as produced by [`tsv_rows`] or spreadsheet copies.
pub fn parse_tsv(text: &str) -> Vec<Row> {
    text.trim_end_matches(['\n', '\r'])
        .split('\n')
        .map(|line| {
            line.trim_end_matches('\r')
                .split('\t')
                .map(|field| {
                    if field == "\\N" {
                        return None;
                    }
                    let mut value = String::with_capacity(field.len());
                    let mut chars = field.chars();
                    while let Some(c) = chars.next() {
                        if c == '\\' {
                            match chars.next() {
                                Some('t') => value.push('\t'),
                                Some('n') => value.push('\n'),
                                Some('r') => value.push('\r'),
                                Some(other) => value.push(other),
                                None => value.push('\\'),
                            }
                        } else {
                            value.push(c);
                        }
                    }
                    Some(value)
                })
                .collect()
        })
        .collect()
}

/// A JSON array of objects in column order. Values stay strings so that numbers
/// and timestamps keep their exact PostgreSQL text; NULL becomes `null`.
pub fn json_rows(columns: &[String], rows: &[&Row]) -> String {
    let quote = |s: &str| serde_json::to_string(s).expect("strings serialize");
    let objects: Vec<String> = rows
        .iter()
        .map(|row| {
            let fields: Vec<String> = columns
                .iter()
                .zip(row.iter())
                .map(|(name, value)| {
                    format!(
                        "{}: {}",
                        quote(name),
                        value.as_deref().map_or_else(|| "null".into(), quote)
                    )
                })
                .collect();
            format!("  {{{}}}", fields.join(", "))
        })
        .collect();
    format!("[\n{}\n]\n", objects.join(",\n"))
}

/// A SQL string literal for `standard_conforming_strings = on`, the default
/// since PostgreSQL 9.1.
pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// One INSERT statement per row for the given columns.
pub fn insert_statements(
    relation: &Relation,
    columns: &[String],
    rows: &[Vec<Option<&str>>],
) -> String {
    let names = columns
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    rows.iter()
        .map(|row| {
            let values = row
                .iter()
                .map(|v| v.map_or_else(|| "NULL".into(), quote_literal))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "INSERT INTO {} ({names}) VALUES ({values});\n",
                relation.qualified_name()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id_column() -> Column {
        Column {
            name: "id".into(),
            data_type: "integer".into(),
            primary_key: true,
            cast_type: "pg_catalog.int4".into(),
            sortable: true,
            ..Default::default()
        }
    }

    fn request() -> BrowseRequest {
        BrowseRequest {
            relation: Relation {
                schema: "odd\"schema".into(),
                name: "users".into(),
                kind: "r".into(),
                ..Default::default()
            },
            columns: vec![id_column()],
            page: 2,
            page_size: 100,
            ..Default::default()
        }
    }

    fn target() -> MutationTarget {
        MutationTarget {
            relation: request().relation,
            columns: vec![
                id_column(),
                Column {
                    name: "code".into(),
                    data_type: "character varying(3)".into(),
                    nullable: true,
                    cast_type: "pg_catalog.\"varchar\"".into(),
                    sortable: true,
                    ..Default::default()
                },
                Column {
                    name: "doubled".into(),
                    data_type: "integer".into(),
                    generated: true,
                    cast_type: "pg_catalog.int4".into(),
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn quotes_identifiers_and_paginates_with_primary_key_tiebreaker() {
        let (sql, _) = browse_sql(&request()).unwrap();
        assert_eq!(
            sql,
            "SELECT _pgm_row.\"id\"::text FROM (SELECT * FROM \"odd\"\"schema\".\"users\" AS _pgm_row ORDER BY _pgm_row.\"id\" LIMIT 101 OFFSET 200) AS _pgm_row ORDER BY _pgm_row.\"id\""
        );
    }

    #[test]
    fn filter_values_remain_parameters_including_quotes_and_wildcards() {
        let mut req = request();
        let value = "%' OR true; DROP TABLE users; --";
        req.filter = FilterSet::single(Filter {
            column: "id".into(),
            operator: FilterOperator::Contains,
            value: value.into(),
        });
        let (sql, params) = browse_sql(&req).unwrap();
        assert!(!sql.contains(value));
        assert_eq!(params, [value]);
        req.filter.conditions[0].column = "missing".into();
        assert!(browse_sql(&req).is_err());
    }

    #[test]
    fn combines_conditions_with_typed_comparisons_and_custom_sql() {
        let mut req = request();
        req.filter = FilterSet {
            conditions: vec![
                Filter {
                    column: "id".into(),
                    operator: FilterOperator::GreaterOrEqual,
                    value: "10".into(),
                },
                Filter {
                    column: "id".into(),
                    operator: FilterOperator::NotInList,
                    value: "11\n12".into(),
                },
                Filter {
                    column: "id".into(),
                    operator: FilterOperator::IsNull,
                    value: String::new(),
                },
            ],
            any: true,
            custom_sql: "id % 2 = 0".into(),
        };
        let (sql, params) = select_sql(&req).unwrap();
        assert!(sql.contains(
            "WHERE (_pgm_row.\"id\" >= ($1::text)::pg_catalog.int4 OR _pgm_row.\"id\" NOT IN (($2::text)::pg_catalog.int4, ($3::text)::pg_catalog.int4) OR _pgm_row.\"id\" IS NULL) AND (id % 2 = 0)"
        ));
        assert_eq!(params, ["10", "11", "12"]);
        for custom in [
            "1 = 1); DROP TABLE x; --",
            "true) OR (true",
            "a = 'open",
            "x -- y",
        ] {
            req.filter.custom_sql = custom.into();
            assert!(select_sql(&req).is_err(), "{custom} must be rejected");
        }
    }

    #[test]
    fn unsortable_types_sort_by_text_and_null_placement_is_optional() {
        let mut req = request();
        req.columns.push(Column {
            name: "doc".into(),
            data_type: "json".into(),
            ..Default::default()
        });
        req.sort = vec![SortColumn {
            name: "doc".into(),
            descending: true,
        }];
        req.nulls = NullsOrder::First;
        let (sql, _) = select_sql(&req).unwrap();
        assert!(sql.ends_with("ORDER BY _pgm_row.\"doc\"::text DESC NULLS FIRST, _pgm_row.\"id\""));
        req.nulls = NullsOrder::Default;
        let (sql, _) = browse_sql(&req).unwrap();
        assert!(
            !sql.contains("NULLS"),
            "default NULL placement leaves indexes usable: {sql}"
        );
        assert!(
            sql.starts_with(
                "SELECT _pgm_row.\"id\"::text, _pgm_row.\"doc\"::text FROM (SELECT * FROM"
            ),
            "only the page is cast to text: {sql}"
        );
    }

    #[test]
    fn mutations_cast_without_modifiers_and_use_the_primary_key() {
        let target = target();
        let original = vec![Some("7".into()), Some("abc".into()), Some("14".into())];
        let update = update_row_sql(&target, &original, &[(1, Some("toolong".into()))]).unwrap();
        assert_eq!(
            update.sql,
            "UPDATE \"odd\"\"schema\".\"users\" SET \"code\" = ($1::text)::pg_catalog.\"varchar\" WHERE \"id\" = ($2::text)::pg_catalog.int4 AND \"id\"::text IS NOT DISTINCT FROM $3::text AND \"code\"::text IS NOT DISTINCT FROM $4::text AND \"doubled\"::text IS NOT DISTINCT FROM $5::text"
        );
        assert_eq!(update.params[0].as_deref(), Some("toolong"));
        assert!(update_row_sql(&target, &original, &[(2, Some("1".into()))]).is_err());
        assert!(update_row_sql(&target, &original, &[(1, None), (1, None)]).is_err());
        assert!(update_row_sql(&target, &original[..2], &[(1, None)]).is_err());
        let delete = delete_rows_sql(&target, &[original.clone(), original.clone()]).unwrap();
        assert_eq!(delete.expected, 2);
        assert_eq!(delete.params.len(), 8);
        let insert = insert_row_sql(
            &target,
            &[
                InsertValue::Default,
                InsertValue::Text("x".into()),
                InsertValue::Default,
            ],
        )
        .unwrap();
        assert_eq!(
            insert.sql,
            "INSERT INTO \"odd\"\"schema\".\"users\" (\"code\") VALUES (($1::text)::pg_catalog.\"varchar\")"
        );
    }

    #[test]
    fn formatting_preserves_literals_and_bodies() {
        let formatted =
            format_sql("select a,b from t where c = 'select x' and d = 'C:\\' and e = 1").unwrap();
        assert!(formatted.starts_with("SELECT"));
        assert!(formatted.contains("'select x'"));
        assert!(formatted.contains("'C:\\'"));
        let function = "create function f() returns text language sql as $body$ select  'a;b' $body$;\nselect 1";
        let formatted = format_sql(function).unwrap();
        assert!(formatted.contains("$body$ select  'a;b' $body$"));
        assert!(lexer::same_tokens(function, &formatted));
        let unchanged = "select x from t";
        assert!(lexer::same_tokens(
            unchanged,
            &format_sql(unchanged).unwrap()
        ));
    }

    #[test]
    fn copy_formats_round_trip_and_escape_values() {
        let rows: Vec<Row> = vec![
            vec![Some("a\tb\nc\\d".into()), None, Some(String::new())],
            vec![Some("it's".into()), Some("\\N".into()), Some("x".into())],
        ];
        let refs: Vec<&Row> = rows.iter().collect();
        let columns = vec!["one".to_string(), "two".into(), "three".into()];
        let tsv = tsv_rows(None, &refs);
        assert_eq!(parse_tsv(&tsv), rows);
        assert!(tsv_rows(Some(&columns), &refs).starts_with("one\ttwo\tthree\n"));
        let json = json_rows(&columns, &refs);
        assert!(json.contains("\"two\": null") && json.contains("\"one\": \"it's\""));
        let relation = Relation {
            schema: "s".into(),
            name: "t".into(),
            ..Default::default()
        };
        let inserts = insert_statements(&relation, &columns[..2], &[vec![Some("it's"), None]]);
        assert_eq!(
            inserts,
            "INSERT INTO \"s\".\"t\" (\"one\", \"two\") VALUES ('it''s', NULL);\n"
        );
        assert_eq!(
            parse_tsv("1\t\\N\r\n2\tx\n"),
            vec![
                vec![Some("1".into()), None],
                vec![Some("2".into()), Some("x".into())]
            ]
        );
    }

    #[test]
    fn csv_headers_parse_with_quotes_and_guessed_delimiters() {
        assert_eq!(
            parse_csv_record("a,\"b,\"\"c\"\"\",d\r\n", ','),
            ["a", "b,\"c\"", "d"]
        );
        assert_eq!(guess_delimiter("id;name;price"), ';');
        assert_eq!(guess_delimiter("id\tname"), '\t');
        assert_eq!(guess_delimiter("id,name"), ',');
        let relation = Relation {
            schema: "s".into(),
            name: "t".into(),
            ..Default::default()
        };
        assert_eq!(
            copy_from_csv_sql(&relation, &["a b".into()], true, ';'),
            "COPY \"s\".\"t\" (\"a b\") FROM STDIN WITH (FORMAT csv, HEADER true, DELIMITER ';')"
        );
    }

    #[test]
    fn structure_edits_produce_ordered_quoted_ddl() {
        let relation = Relation {
            schema: "app".into(),
            name: "items".into(),
            kind: "r".into(),
            ..Default::default()
        };
        let structure = TableStructure {
            columns: vec![
                Column {
                    name: "id".into(),
                    data_type: "integer".into(),
                    primary_key: true,
                    ..Default::default()
                },
                Column {
                    name: "title".into(),
                    data_type: "text".into(),
                    nullable: true,
                    default: Some("'x'::text".into()),
                    ..Default::default()
                },
                Column {
                    name: "old".into(),
                    data_type: "text".into(),
                    nullable: true,
                    ..Default::default()
                },
            ],
            constraints: vec![
                ("items_pkey".into(), "PRIMARY KEY (id)".into()),
                ("positive".into(), "CHECK (id > 0)".into()),
            ],
            indexes: vec![
                IndexInfo {
                    name: "items_pkey".into(),
                    definition: "...".into(),
                    constraint: true,
                    ..Default::default()
                },
                IndexInfo {
                    name: "items_title".into(),
                    definition: "CREATE INDEX ...".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let original = TableDraft::from_structure(&relation, &structure);
        assert_eq!(
            original.indexes.len(),
            1,
            "constraint indexes are edited through constraints"
        );
        let mut edited = original.clone();
        edited.columns[1].name = "Title".into();
        edited.columns[1].data_type = "varchar(200)".into();
        edited.columns[1].nullable = false;
        edited.columns[1].default.clear();
        edited.columns[1].comment = "It's shown".into();
        edited.columns[2].dropped = true;
        edited.columns.push(ColumnDraft {
            name: "price".into(),
            data_type: "numeric(8,2)".into(),
            nullable: true,
            default: "0".into(),
            ..Default::default()
        });
        edited.constraints[1].dropped = true;
        edited.constraints.push(ConstraintDraft {
            name: "price_ok".into(),
            definition: "CHECK (price >= 0)".into(),
            ..Default::default()
        });
        edited.indexes[0].dropped = true;
        edited.indexes.push(IndexDraft {
            name: "items_price".into(),
            columns: "price DESC".into(),
            method: "btree".into(),
            ..Default::default()
        });
        edited.name = "goods".into();
        let sql = alter_table_sql(&original, &edited).unwrap();
        assert_eq!(
            sql,
            vec![
                "ALTER TABLE \"app\".\"items\" DROP CONSTRAINT \"positive\"",
                "DROP INDEX \"app\".\"items_title\"",
                "ALTER TABLE \"app\".\"items\" DROP COLUMN \"old\"",
                "ALTER TABLE \"app\".\"items\" RENAME COLUMN \"title\" TO \"Title\"",
                "ALTER TABLE \"app\".\"items\" ALTER COLUMN \"Title\" TYPE varchar(200) USING \"Title\"::varchar(200)",
                "ALTER TABLE \"app\".\"items\" ALTER COLUMN \"Title\" SET NOT NULL",
                "ALTER TABLE \"app\".\"items\" ALTER COLUMN \"Title\" DROP DEFAULT",
                "COMMENT ON COLUMN \"app\".\"items\".\"Title\" IS 'It''s shown'",
                "ALTER TABLE \"app\".\"items\" ADD COLUMN \"price\" numeric(8,2) DEFAULT 0",
                "ALTER TABLE \"app\".\"items\" ADD CONSTRAINT \"price_ok\" CHECK (price >= 0)",
                "CREATE INDEX \"items_price\" ON \"app\".\"items\" USING btree (price DESC)",
                "ALTER TABLE \"app\".\"items\" RENAME TO \"goods\"",
            ]
        );
        assert!(alter_table_sql(&original, &original).unwrap().is_empty());
        let mut bad = original.clone();
        bad.columns[1].default = "1); DROP TABLE x; --".into();
        assert!(alter_table_sql(&original, &bad).is_err());
        let created = create_table_sql(&TableDraft {
            schema: "app".into(),
            name: "new table".into(),
            comment: "Fresh".into(),
            columns: vec![
                ColumnDraft {
                    name: "id".into(),
                    data_type: "bigint".into(),
                    primary_key: true,
                    default: String::new(),
                    ..Default::default()
                },
                ColumnDraft {
                    name: "note".into(),
                    data_type: "text".into(),
                    nullable: true,
                    comment: "free text".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            created[0],
            "CREATE TABLE \"app\".\"new table\" (\n    \"id\" bigint NOT NULL,\n    \"note\" text,\n    PRIMARY KEY (\"id\")\n)"
        );
        assert_eq!(created.len(), 3);
        let view = Relation {
            schema: "app".into(),
            name: "v".into(),
            kind: "m".into(),
            ..Default::default()
        };
        assert_eq!(
            relation_action_sql(&view, &RelationAction::Drop { cascade: true }).unwrap(),
            "DROP MATERIALIZED VIEW \"app\".\"v\" CASCADE"
        );
        assert!(
            relation_action_sql(
                &view,
                &RelationAction::Truncate {
                    cascade: false,
                    restart_identity: false
                }
            )
            .is_err()
        );
        assert_eq!(
            relation_action_sql(&relation, &RelationAction::Rename("a\"b".into())).unwrap(),
            "ALTER TABLE \"app\".\"items\" RENAME TO \"a\"\"b\""
        );
        assert_eq!(
            create_database_sql("my db").unwrap(),
            "CREATE DATABASE \"my db\""
        );
        assert!(create_schema_sql(" ").is_err());
    }

    #[test]
    fn advisor_fixes_quote_names() {
        assert_eq!(
            drop_index_sql("app", "Old idx", false),
            "DROP INDEX CONCURRENTLY \"app\".\"Old idx\""
        );
        assert_eq!(
            drop_index_sql("app", "events_at", true),
            "DROP INDEX \"app\".\"events_at\""
        );
        assert_eq!(
            foreign_key_index_sql(
                "app",
                "items",
                &["tenant".into(), "parent id".into()],
                false
            ),
            "CREATE INDEX CONCURRENTLY ON \"app\".\"items\" (\"tenant\", \"parent id\")"
        );
        assert!(
            foreign_key_index_sql("app", "events", &["x".into()], true)
                .starts_with("CREATE INDEX ON")
        );
    }

    #[test]
    fn library_lists_follow_postgresql_syntax() {
        assert!(parse_library_list("").is_empty());
        assert!(parse_library_list("  ").is_empty());
        assert_eq!(
            parse_library_list(" auto_explain ,pg_cron,, \"$libdir/my, lib\" ,\"a\"\"b\""),
            ["auto_explain", "pg_cron", "$libdir/my, lib", "a\"b"]
        );
        let libraries =
            parse_library_list("auth_permission_dialog,$libdir/pg_stat_statements.dylib");
        assert!(loads_library(&libraries, "pg_stat_statements"));
        assert!(!loads_library(&libraries, "pg_stat"));
        assert_eq!(with_library(&libraries, "pg_stat_statements"), libraries);
        let added = with_library(&["auto_explain".into()], "pg_stat_statements");
        assert_eq!(added, ["auto_explain", "pg_stat_statements"]);
        assert_eq!(
            format_library_list(&["auto_explain".into(), "my, lib".into(), "a\"b".into()]),
            "auto_explain,\"my, lib\",\"a\"\"b\""
        );
        let round_trip = vec!["x".to_owned(), "my, lib".to_owned(), "a\"b".to_owned()];
        assert_eq!(
            parse_library_list(&format_library_list(&round_trip)),
            round_trip
        );
        assert_eq!(
            alter_preload_sql(&added),
            "ALTER SYSTEM SET shared_preload_libraries = 'auto_explain', 'pg_stat_statements'"
        );
        assert_eq!(
            alter_preload_sql(&["it's".into()]),
            "ALTER SYSTEM SET shared_preload_libraries = 'it''s'"
        );
        assert_eq!(
            alter_preload_sql(&[]),
            "ALTER SYSTEM RESET shared_preload_libraries"
        );
    }

    #[test]
    fn csv_distinguishes_null_and_empty_and_escapes_quotes() {
        let result = QueryResult {
            columns: vec!["a".into(), "b".into()],
            rows: vec![
                vec![None, Some(String::new())],
                vec![Some("a,\"b\n".into()), Some("|x|".into())],
            ],
            ..Default::default()
        };
        assert!(csv(&result).contains(",\"\"\r\n"));
        assert!(csv(&result).contains("\"a,\"\"b\n\""));
        assert!(markdown(&result).contains("\\|x\\|"));
    }
}
