# Development

## Checks

```sh
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
sh scripts/test-postgres.sh
```

Plain `cargo test` skips the database tests.

## Database tests

The database tests never connect to an existing database.
`scripts/test-postgres.sh` creates an isolated cluster on port 55439, with
`pg_stat_statements` preloaded, and a second one on the next port, started the
way Postgres.app starts its servers, which the setup test restarts. Both stop on
exit. Temporary cluster files are kept for inspection.

The default tool path is Postgres.app. Set `PG_BIN` for another installation and
`POSTERN_TEST_PORT` if the port is taken. `POSTERN_DATA_DIR` selects a separate
application data directory.

## Screenshots

Capture the application's real views against synthetic data:

```sh
POSTERN_CAPTURE_DIR=/tmp/postern-captures sh scripts/test-postgres.sh
```

This builds the optional `smoke` feature and needs a macOS GUI session with the
display awake (`caffeinate -d` helps). It drives the connection worker and the
table, SQL, structure, server, and diagnostics views, captures only the
application's own framebuffer, and closes each window. `scripts/demo.sql` seeds
the synthetic data. Set `POSTERN_SMOKE_SCREENS` to a space-separated list to
capture fewer views and `POSTERN_SMOKE_DEBUG=1` to trace the harness. It is not
full mouse and keyboard coverage.

The `tabs` and `tabs_light` scenarios exercise table switching with pending edits,
independent filters and selection, a background load, saving and restoring a row
while another tab is selected, closing tabs, and preventing a database switch
with edits in an inactive tab. They leave the fixture values unchanged.

On macOS, the `clipboard` scenario also verifies native copy and paste,
including a SQL editor selection with Unicode and a newline. It replaces the
system clipboard with fixture text. Run it on its own with:

```sh
POSTERN_CAPTURE_DIR=/tmp/postern-captures POSTERN_SMOKE_SCREENS=clipboard sh scripts/test-postgres.sh
```

Results and every screenshot are in [verification.md](verification.md).
`sh scripts/make-artwork.sh` regenerates the icon, the banners, and the framed
screenshots in `assets/`.

## App bundle

```sh
sh scripts/bundle-macos.sh --install
```

This builds `Postern.app` for the current machine's architecture, signs it with
a local ad-hoc signature, copies it to `/Applications`, and registers it with
Spotlight. Without `--install`, the app stays in `dist/Postern.app`, and
Spotlight then lists it next to an installed copy. Every build gets a new
signature, so Keychain asks again for access to saved passwords.

`./install.sh` checks the Xcode command line tools and Rust, installs the pinned
toolchain, and runs the bundle script with `--install`. `./install.sh --no-open`
skips opening the app.

## Releases

`sh scripts/make-dmg.sh` builds `dist/Postern-<version>-<architecture>.dmg`, a
disk image that installs by dragging the app onto Applications. Both the image
name and the app version come from `version` in `Cargo.toml`.

To release, raise `version` in `Cargo.toml`, commit, and push a matching tag,
such as `v0.1.1`. The release workflow builds the disk image and publishes it as
a GitHub release. Releases are not notarized yet.
