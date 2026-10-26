# Postern

Rust desktop PostgreSQL client using eframe/egui, Tokio, and tokio-postgres.

- Read README.md and docs/architecture.md before changing behavior.
- Keep this project independent from Medibiom and SequelPG working trees.
- Keep database operations in services, SQL construction in services/sql, and
  UI state/rendering in ui. Database work must never run on the UI thread.
- Keep NULL distinct from an empty string and preserve exact PostgreSQL values.
- Parameterize values and quote every dynamic identifier. Require a primary key,
  current metadata, and optimistic conflict checks for grid updates/deletes.
- Do not share transactions between the table browser and SQL editor.
- Never save passwords in profile JSON, SQL history, logs, or command arguments.
- Use a disposable PostgreSQL cluster for integration tests, never an existing
  developer or production database.
- Follow local analogs. Update tests and documentation for changed contracts.
- Run cargo fmt --check, cargo clippy --locked --all-targets --all-features -- -D warnings,
  cargo test --locked, and relevant database/native smoke tests.
- Do not create branches, stage, commit, push, or configure remotes unless requested.
- Do not add Co-Authored-By or other AI attribution lines to commits or pull requests.
- Code and repository documentation are English. Respond in the user's language.
