# Verification

## Clipboard regression, 2026-10-26

Verified on macOS 26.4.1, Apple Silicon, Rust 1.99.1, and disposable PostgreSQL
19rc1 clusters. The new `clipboard` scenario in
[`src/ui/app/smoke.rs`](../src/ui/app/smoke.rs) failed before enabling eframe's
`clipboard` feature: `pbpaste` could not see the text sent through `copy_text`.
The same scenario passed after the feature and its lockfile dependencies were
enabled. It checks the output path used by Copy buttons, a selected SQL fragment
with Polish and Chinese characters and a newline, and a native paste into the
editor. It checks actual system clipboard contents without printing them on
failure.

- `cargo fmt --check`: passed.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed.
- `cargo test --locked`: all 54 unit tests passed.
- `scripts/test-postgres.sh`: all 13 database integration tests passed.
- Native smoke scenarios `clipboard`, `slow_queries`, `query`, and `selection`:
  passed. The clipboard capture was visually inspected.

The copy event is injected into egui, and paste is requested through eframe.
Physical Command-C/Command-V key presses and clicking the Slow queries Copy
button were not automated. The Copy button uses the same tested `copy_text`
output path. The test replaces the system clipboard with fixture text.

## Full application verification, 2026-10-25

Verified on 2026-10-25 on macOS 26.4.1, Apple Silicon (arm64), Rust 1.99.1, with
PostgreSQL 19rc1, 18.6, 16.15, and 14 from Postgres.app.

- `cargo fmt --check`: passed.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed.
- `cargo test --locked`: 54 unit tests passed. They cover the SQL lexer, statement
  splitting and the statement at the cursor, positional parameters, the formatter
  guard, completion with aliases, filters, mutation and DDL builders, index
  advisor fixes, `shared_preload_libraries` lists and `ALTER SYSTEM`,
  `postmaster.opts` parsing and shell quoting, copy and paste formats,
  connection string parsing, `.pgpass` lookup, moving the former data
  directory after the rename, staged edits, bytea and JSON
  display, EXPLAIN parsing, find matching, file permissions, duration
  formatting, and grid keyboard, inline editing, selection, and deletion
  behavior. The thirteen database tests are ignored by this command and were run
  separately below.
- `scripts/test-postgres.sh`: all thirteen database integration tests passed on
  PostgreSQL 19rc1, 18.6, 16.15, and 14. Twelve run against a disposable cluster
  with `pg_stat_statements` preloaded. The setup test runs against a second
  disposable cluster started like Postgres.app starts its servers, with
  `shared_preload_libraries=auto_explain` on the command line. In addition to the
  earlier coverage of metadata, pagination, ordering, exact values, conflict
  protection, batch deletion, discovery, and database switching, they cover:
  - over-long `varchar(n)`, `char(n)`, and `bit(n)` values rejected instead of
    truncated, and UPDATE predicates that use the primary key index;
  - scripts run per statement, error locations, NOTICE capture, VACUUM in a
    script, and rejected COPY STDIN;
  - concurrent data and SQL lanes, transaction state, a terminated SQL session
    reconnecting with its startup SQL, and read-only profiles;
  - one SQL session per document, with independent transactions, concurrent
    queries, and blocked database switching while any transaction is open;
  - staged updates, inserts, and deletes saved in one transaction and rolled back
    together on conflict; CSV export and import round trips; failing imports
    rolled back; query export through COPY; foreign key metadata and lookup;
  - generated DDL that recreates a table identically after it is dropped,
    partitions, materialized views, structure edits, table creation, object
    actions, functions and procedures;
  - session activity, cancelling another session's query, roles, table grants,
    and EXPLAIN and EXPLAIN ANALYZE plans that leave data unchanged.
  - descending sorts served by an index scan with default NULL placement,
    explicit NULL placement, subquery pages matching one ordered query, and row
    counts on the separate count session while the browser keeps loading pages;
  - the index advisor in a new database: an invalid index from a failed
    concurrent build, unindexed single and composite foreign keys, keys covered
    in another column order, exact duplicates (including of a unique
    constraint), redundant prefixes, operator classes and partial indexes kept
    apart, keys and foreign key indexes never proposed for dropping, sequential
    scans, dead rows, and the suggested SQL run as written;
  - slow queries: the missing extension reported as preloaded, installation,
    normalized statements with call counts, current database and catalog
    filters, ordering by calls, shares, generic plans on PostgreSQL 16 and later,
    and reset;
  - pg_stat_statements setup on the second cluster: the setup state with a
    restartable local server, a restart with a library that cannot load followed
    by an automatic start with the previous command line, an open SQL
    transaction blocking the restart, and the full setup through the worker. The
    full setup configures `ALTER SYSTEM` without the command-line library,
    restarts with `auto_explain,pg_stat_statements`, reconnects the data, shared
    SQL, and document sessions, and creates the extension. The server log
    confirmed the failed start, the restored start, and the restart.
- Sorting benchmark on a 56,000-row table (local server, median end to end
  through `Database::browse`): an unindexed text column took 29.6 ms before and
  17.7 ms after, the same as a plain `SELECT * ... ORDER BY ... LIMIT` measured
  in the same run; an indexed column sorted descending took 20.8 ms before and
  0.3 ms after; counting the table took 2.5 to 5 ms.
- Native smoke harness: 30 views captured from the real application against the
  disposable servers on PostgreSQL 19rc1. It checks staged edits, filters, foreign
  key navigation, the structure editor, function definitions, generated DDL,
  completion, find, light appearance, activity, roles, an analyzed plan, the
  index advisor, slow queries, a generic plan for a recorded statement, the
  pg_stat_statements setup confirmation, a setup that restarts a fresh local
  cluster and ends with recorded statements, Unicode glyph coverage, and that
  the native menu bar is installed. The captures were visually inspected. Two
  full runs stopped at the completion screen. The harness waited a fixed 300 ms
  for the editor to apply the cursor, which the first frames sometimes exceeded;
  it now waits until the editor has applied it, and the following full run
  passed.
- Rename to Postern: all of the above ran after the rename. The data directory
  move is covered by a unit test. The Keychain move was not exercised against
  the real Keychain.
- Release build: `scripts/bundle-macos.sh` built and ad-hoc signed the arm64
  `Postern.app` with the mammoth icon from `assets/Postern.icns`.
- `./install.sh --no-open` checked the command line tools and the pinned Rust,
  built and installed `/Applications/Postern.app`, left no copy in `dist/`, and
  noticed that Postern was running instead of opening it. The branches that
  install the command line tools or Rust on a Mac without them were not run.
- `scripts/make-dmg.sh` built a 7.5 MB `Postern-0.1.0-arm64.dmg`. Mounted
  read-only, it held the signed app, an Applications shortcut, and the Postern
  volume icon, and `hdiutil verify` reported a valid checksum. Opening a
  downloaded, quarantined copy through Gatekeeper was not tried, and the release
  workflow has not run on GitHub.

The smoke harness drives application state and database commands, not physical
mouse and keyboard input, and captures only the application's framebuffer, so
the menu bar itself is not pictured. Grid tests inject egui input events without a
native window. SSH tunnels, client certificates, live Keychain round trips, remote
TLS endpoints, compatibility databases, Intel Macs, older macOS versions, and
notarized distribution were not tested. Diagnostics were not tested by a role
without `pg_read_all_stats` or superuser rights, or on PostgreSQL 12 and older,
which use the pre-13 column names. The setup restart was tested on disposable
clusters started with `pg_ctl`, not on a server managed by the Postgres.app user
interface, Homebrew services, or launchd, and no existing server was restarted.
The GitHub Actions workflow is supplied but has not been run on GitHub. No
existing development databases were used.

Dependencies added in this round: `ron` (with `enumn` and `typeid`) for eframe's
saved window and column state, and `muda` (with `keyboard-types` and
`crossbeam-channel`) for the macOS menu bar. The diagnostics, the rename, and the
icon added no dependencies; the icon and README artwork are drawn by Swift
scripts with the system's CoreGraphics and stored in `assets/`.

Screenshots contain only synthetic fixture data:

- [Saved servers](screenshots/servers.png)
- [Table content](screenshots/table.png)
- [Table content, light appearance](screenshots/table_light.png)
- [Staged edits with row details](screenshots/staged.png)
- [Multi-condition filter](screenshots/filter.png)
- [Foreign key navigation](screenshots/related.png)
- [Multiple selected rows](screenshots/selection.png)
- [Rows marked for deletion](screenshots/delete.png)
- [Cell editor](screenshots/edit.png)
- [Table structure and statistics](screenshots/structure.png)
- [Structure editor](screenshots/structure_edit.png)
- [Generated DDL](screenshots/ddl.png)
- [Function definition](screenshots/routine.png)
- [SQL editor and results](screenshots/query.png)
- [SQL editor, light appearance](screenshots/query_light.png)
- [Completion through a table alias](screenshots/completion.png)
- [Find in the SQL editor](screenshots/find.png)
- [EXPLAIN ANALYZE plan](screenshots/explain.png)
- [Index advisor](screenshots/advisor.png)
- [Slow queries](screenshots/slow_queries.png)
- [Generic plan for a recorded statement](screenshots/generic_plan.png)
- [pg_stat_statements setup confirmation](screenshots/statements_setup.png)
- [Slow queries after the setup restart](screenshots/statements_restart.png)
- [Server activity](screenshots/activity.png)
- [Roles](screenshots/roles.png)
- [Discover databases before connecting](screenshots/discovery.png)
- [Switch databases from the toolbar](screenshots/databases.png)
- [Unicode table cells](screenshots/unicode.png)
- [Unicode SQL text and results](screenshots/unicode_query.png)
- [Unicode cell editor](screenshots/unicode_edit.png)
