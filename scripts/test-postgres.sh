#!/bin/sh
set -eu

cd "$(dirname "$0")/.."
PG_BIN=${PG_BIN:-/Applications/Postgres.app/Contents/Versions/latest/bin}
CARGO=${CARGO:-cargo}
POSTERN_TEST_PORT=${POSTERN_TEST_PORT:-55439}
export POSTERN_TEST_PORT
POSTERN_SETUP_PORT=$((POSTERN_TEST_PORT + 1))
cluster=$(mktemp -d "${TMPDIR:-/tmp}/postern-test.XXXXXX")
cleanup() {
  for data in "$cluster/data" "$cluster/setup" "$cluster/setup-ui"; do
    "$PG_BIN/pg_ctl" -D "$data" -m immediate stop >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT INT TERM
# A cluster without pg_stat_statements, started the way Postgres.app starts its
# servers, with shared_preload_libraries on the command line. Tests may restart it.
start_setup_cluster() {
  "$PG_BIN/initdb" -D "$1" -U postern_test --auth=trust --no-locale -E UTF8 >"$1-init.log"
  "$PG_BIN/pg_ctl" -D "$1" -l "$1.log" -o "-h 127.0.0.1 -p $POSTERN_SETUP_PORT -k $cluster" -o "-c shared_preload_libraries=auto_explain" -w start
}
"$PG_BIN/initdb" -D "$cluster/data" -U postern_test --auth=trust --no-locale -E UTF8 >"$cluster/init.log"
"$PG_BIN/pg_ctl" -D "$cluster/data" -l "$cluster/postgres.log" -o "-h 127.0.0.1 -p $POSTERN_TEST_PORT -k $cluster -c shared_preload_libraries=pg_stat_statements" -w start
"$CARGO" test --locked --test postgres -- --ignored --skip query_statistics_setup
start_setup_cluster "$cluster/setup"
POSTERN_SETUP_PORT=$POSTERN_SETUP_PORT "$CARGO" test --locked --test postgres -- --ignored --exact query_statistics_setup_restarts_a_local_server
"$PG_BIN/pg_ctl" -D "$cluster/setup" -m fast -w stop >/dev/null
if [ -n "${POSTERN_CAPTURE_DIR:-}" ]; then
  mkdir -p "$POSTERN_CAPTURE_DIR"
  "$PG_BIN/psql" -X -v ON_ERROR_STOP=1 -h 127.0.0.1 -p "$POSTERN_TEST_PORT" -U postern_test -d postgres -f scripts/demo.sql
  "$CARGO" build --locked --features smoke
  screens=${POSTERN_SMOKE_SCREENS:-servers table query structure edit selection delete discovery databases unicode unicode_query unicode_edit staged filter related structure_edit routine ddl completion find table_light query_light activity roles explain advisor slow_queries generic_plan statements_setup statements_restart}
  setup_screens=
  for screen in $screens; do
    case $screen in
      statements_*) setup_screens="$setup_screens $screen"; continue ;;
    esac
    POSTERN_SMOKE_SCREEN=$screen POSTERN_CAPTURE="$POSTERN_CAPTURE_DIR/$screen.png" POSTERN_DATA_DIR="$cluster/app-$screen" target/debug/postern
  done
  if [ -n "$setup_screens" ]; then
    # The setup screens run in order: the last one restarts this cluster.
    start_setup_cluster "$cluster/setup-ui"
    for screen in $setup_screens; do
      POSTERN_TEST_PORT=$POSTERN_SETUP_PORT POSTERN_SMOKE_SCREEN=$screen POSTERN_CAPTURE="$POSTERN_CAPTURE_DIR/$screen.png" POSTERN_DATA_DIR="$cluster/app-$screen" target/debug/postern
    done
    "$PG_BIN/pg_ctl" -D "$cluster/setup-ui" -m fast -w stop >/dev/null
  fi
fi
printf 'Test cluster retained at %s (servers stopped on exit).\n' "$cluster"
