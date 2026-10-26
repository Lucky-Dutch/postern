# Architecture

```text
egui views and PosternApp state
        |
        | typed Command / Event messages, tagged with a lane
        v
Tokio worker
        |
        +-- data lane: catalog, browsing, staged grid changes, DDL, import/export
        +-- SQL lane per document: user statements, explicit transactions
        +-- control lane: connect, switch database, disconnect, discovery
```

## Modules

- `src/models.rs`: connection profiles (SSL, SSH, session options, URL parsing),
  table metadata, filters, browse requests, staged row changes, structure drafts,
  activity and role rows, index advisor findings, statement statistics, and SQL
  documents.
- `src/sql.rs`: identifier and literal quoting; browse, count, and filter SQL;
  guarded UPDATE, INSERT, and DELETE statements; DDL for structure edits, table
  creation, object actions, and index advisor fixes; CSV, TSV, JSON, Markdown,
  and INSERT formats; and the guarded formatter.
  - `src/sql/lexer.rs`: a PostgreSQL-aware tokenizer for strings, dollar quotes,
    quoted identifiers, and nested comments; statement splitting with psql's
    `BEGIN ATOMIC` rule; the statement at the cursor; positional parameters; and
    token comparison.
  - `src/sql/complete.rs`: completion from the catalog, with alias resolution.
- `src/services/database.rs`: connection and TLS setup, catalog queries, generated
  DDL, browsing, per-statement script execution, guarded mutations, COPY import,
  streaming export, activity, roles, the index advisor, and `pg_stat_statements`.
  UI code does not build write SQL.
- `src/services/worker.rs`: lanes, cancellation, reconnection, and sessions.
- `src/services/server.rs`: detecting a server that runs on this computer and
  restarting it through its own `pg_ctl`.
- `src/services/ssh.rs`: local port forwarding through the system `ssh` client.
- `src/services/pgpass.rs`: libpq-compatible `.pgpass` lookup.
- `src/services/storage.rs`: atomic private JSON files, user files that keep their
  permissions and links, and Keychain credentials. It moves data and Keychain
  passwords saved under the former name, PG Manager.
- `src/ui/app/`: state and views: servers, navigator and object list, table
  browser, structure editor, SQL editor, results and plans, server views,
  diagnostics, and dialogs. `src/ui/grid.rs` is the shared virtualized grid; `src/ui/edits.rs`
  holds staged changes; `src/ui/display.rs` formats bytea and JSON for display;
  `src/ui/explain.rs` parses EXPLAIN JSON; `src/ui/menu.rs` is the macOS menu bar;
  `src/ui/theme.rs` holds fonts, the light and dark palettes, and the
  application icon from `assets/`.
- `src/ui/app/smoke.rs`: optional native framebuffer smoke harness, absent from
  default and release builds.

## Clipboard

The eframe `clipboard` feature must stay enabled when selecting features manually.
Copy buttons use `egui::Context::copy_text`, and focused text fields and grids
handle the clipboard events translated from native shortcuts. Eframe passes the
output to the system clipboard and reads native paste requests. Without this
feature, egui-winit falls back to a clipboard held only in application memory,
so copying appears to succeed but cannot be pasted into another application.

## Sessions

The data lane owns the table browser's session. Each SQL document has a lane and
a session that opens on the document's first query; `Command::Query` uses a shared
SQL session opened with every connection. Lanes run concurrently and each runs one
command at a time. Control commands lock the data session, then every SQL session,
then the connection context, so they wait for running work and never interleave
with it. Switching databases checks every SQL session's state through
`pg_stat_activity` from the data session, opens the new data and shared SQL
sessions before replacing the old ones, and closes document sessions so they
reopen on the new database.

A session found closed before or after a command is reconnected with the password
kept in worker memory and, for SSH profiles, a restarted tunnel. The password and
tunnel are dropped on disconnect. Cancellation sends a cancel request for the
lane's session. After each SQL command, the transaction state of that session is
read from the data session, which never changes the observed transaction.

## Values and mutations

All cell values use `Option<String>`; `None` is SQL NULL. Browsing selects each
column cast to text. Scripts are split into statements and sent one at a time with
the simple query protocol, which keeps textual values exact and lets VACUUM and
similar commands run outside a transaction block. Execution stops at the first
error, and the error position is mapped back to the editor.

Grid writes parameterize every value as text and cast it to the column's type
without modifiers, so the column's own modifier is enforced on assignment. A row
is identified by its primary key, compared with the type's equality so the index
is used, and by every original value compared as text, which detects concurrent
changes. Staged changes are applied in one transaction: updates, inserts, then one
DELETE for all deleted rows. Each statement must affect exactly the expected
number of rows, and fresh column metadata must match the metadata the rows were
loaded with; otherwise everything rolls back. Generated columns are rejected in
the SQL builder even if a caller bypasses the UI. Triggers, permissions, row-level
security, and foreign key actions still apply.

## Diagnostics

The index advisor and the slow query list run read-only catalog and statistics
queries on the data lane, so they share the table browser's statement timeout and
never touch a SQL document's transaction. Findings carry fix SQL built in
`src/sql.rs` with quoted identifiers; the UI only opens it in a new SQL document.
Fixes use `CONCURRENTLY` where PostgreSQL allows it, so they must run outside a
transaction block; partitioned tables and indexes get the plain form.

The advisor compares `pg_index` rows by key columns, operator classes,
collations, and options. An index is reported once, under its most specific
finding: invalid, duplicate, redundant, then unused. Unique indexes, indexes
that enforce a constraint, and indexes that belong to a partitioned index are
never proposed for dropping. Indexes with a predicate or expressions are only
checked for use. An index that starts with a foreign key column is not reported
as unused; a duplicate of it still is, because the other copy remains. A foreign
key counts as indexed when an index without a predicate starts with all of its
columns in any order.

The slow query list finds the schema of the `pg_stat_statements` extension,
reads the PostgreSQL 13+ column names (`total_exec_time`) or the older ones, and
computes each statement's share with a window over every matching statement
before the 200-row limit. Without the extension it reports whether the library
is already preloaded, which decides whether `CREATE EXTENSION` is enough. Errors
from the view, such as a missing preload, are shown instead of failing the
command. EXPLAIN uses `GENERIC_PLAN` for statements with `$n` parameters.

When the library is not loaded, the report describes the setup instead: the
running and configured `shared_preload_libraries`, read from `pg_settings` and
`pg_file_settings`, whether the extension exists, and whether Postern can
restart the server. A restart is offered only when the data directory reported
by the server is readable, its `postmaster.pid` names the parent process of the
connected backend, launchd does not run the postmaster, `pg_ctl` sits next to the
running binary, and the library file exists. `ALTER SYSTEM` writes each library
as a separate literal, because `shared_preload_libraries` quotes list elements.

The setup command runs on the control lane, so it holds every session like a
database switch and refuses to continue while a SQL document has an open
transaction. It configures the library, closes the application's sessions,
stops the server with `pg_ctl stop -m fast`, and starts it with the arguments
from `postmaster.opts`. If those arguments set `shared_preload_libraries`, the
library is added to that value, because the command line overrides the
configuration files. If the server does not start, it is started with its
previous arguments and running library list, and the configuration is restored
once a session reconnects. The data session and the shared SQL session reconnect
at once; document sessions reconnect on their next query.

## Structure edits

Structure edits are compared with the loaded structure to produce ALTER TABLE
statements in a valid order. Identifiers are always quoted; types, defaults, and
constraint definitions are user-written SQL fragments that are rejected if they
contain a semicolon, a line comment, unbalanced parentheses, or an unterminated
quote. The statements are shown for review and run in one transaction.

The design follows the domain separation seen in SequelPG, adapted to Rust
ownership and a message-driven egui application. No Swift, webview, or
general-purpose application server is part of this implementation.
