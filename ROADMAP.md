# Roadmap

Postern covers the daily Postico workflow: connect (including SSH and client
certificates), find a table, browse, filter, and edit rows with staged changes,
follow foreign keys, edit table structure, run SQL with completion and plans, and
import or export data. The remaining work is listed below.

## Connections

- SSH password and keyboard-interactive authentication. Tunnels currently use
  the SSH agent or an identity file through the system `ssh` client.
- Encrypted client keys and PKCS#1 (`BEGIN RSA PRIVATE KEY`) keys. Client keys
  must currently be unencrypted PKCS#8.
- Multiple hosts and failover in one profile.
- Server list sync through iCloud or Dropbox, and pre-connect shell scripts for
  IAM or SSO tools.

## Data grid

- Rectangular cell selection and pasting values into existing rows. Pasting adds
  new rows today.
- Editing rows of updatable views.
- A foreign key picker for composite keys; single-column keys are supported.
- Remembering filters and sort order per table between launches.

## SQL workspace

- Multiple windows, and opening the same file in more than one tab.
- Query parameters and automatic keyword uppercasing while typing.

## Database objects

- Form-based editors for views, triggers, sequences, types, and extensions.
- Editing privileges and ownership. Table grants are shown in Structure; generated
  DDL omits both.

## Diagnostics

- Index suggestions from recorded query plans, and hypothetical indexes.
- Table and index bloat estimates, and lock waits with blocking sessions.
- Explaining a recorded statement with sample parameter values, and query
  statistics on PostgreSQL versions before 13 tested end to end.

## macOS polish and distribution

- Accessibility audit and automated physical keyboard and mouse tests.
- Large-table performance measurements and soak tests.
- Universal binary, Developer ID signing, notarization, and releases.
- Validation against CockroachDB, Redshift, Greenplum, and other compatibility
  layers. Do not assume complete PostgreSQL catalog compatibility.

No ER diagrams, non-PostgreSQL database engines, or general-purpose IDE features
are planned for the core workflow.
