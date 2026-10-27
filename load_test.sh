#!/usr/bin/env bash
# Load and stress tests (tests/load.rs). These never run in CI; this
# script is the way to run them. It starts throwaway Postgres and MySQL
# containers on non-default ports, runs the ignored tests against all
# three backends, and removes the containers on exit.
#
# Usage: ./load_test.sh [extra cargo test args]
# LOAD_N overrides the bulk size (default 1000).

set -euo pipefail

PG_NAME=seaorm-auto-audit-load-pg
MY_NAME=seaorm-auto-audit-load-my
PG_PORT=55432
MY_PORT=53306
# One throwaway password for both containers, local use only.
PW="loadtest"

cleanup() {
    docker rm -f "$PG_NAME" "$MY_NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

docker run -d --name "$PG_NAME" \
    -e "POSTGRES_PASSWORD=${PW}" -e POSTGRES_DB=test \
    -p "$PG_PORT:5432" postgres:18-alpine >/dev/null
docker run -d --name "$MY_NAME" \
    -e "MYSQL_ROOT_PASSWORD=${PW}" -e MYSQL_DATABASE=test \
    -p "$MY_PORT:3306" mysql:8 >/dev/null

echo "waiting for postgres..."
for _ in $(seq 1 60); do
    if docker exec "$PG_NAME" pg_isready -U postgres -d test >/dev/null 2>&1; then
        break
    fi
    sleep 1
done

echo "waiting for mysql..."
for _ in $(seq 1 90); do
    if docker exec "$MY_NAME" mysqladmin ping -h localhost -uroot -p"${PW}" >/dev/null 2>&1; then
        break
    fi
    sleep 1
done

# Debug builds give the bulk tests large futures; give the test threads
# stack headroom to match.
export RUST_MIN_STACK=16777216
export DATABASE_URL="postgres://postgres:${PW}@127.0.0.1:$PG_PORT/test"
export MYSQL_DATABASE_URL="mysql://root:${PW}@127.0.0.1:$MY_PORT/test"

cargo test --test load -- --ignored --nocapture "$@"
