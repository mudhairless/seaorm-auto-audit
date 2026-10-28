#!/usr/bin/env bash
# Coverage for all three suites, mirroring the three CI jobs: the main
# suite (SQLite, with Postgres and MySQL unset so they skip, as in CI),
# then the Postgres suite and the MySQL suite against throwaway Docker
# containers on the same ports load_test.sh uses. All three reports
# print back to back on stdout so they can be copied in one pass.
# Containers are removed on exit.
#
# Usage: ./coverage_docker.sh
# Needs: docker, and cargo-llvm-cov (cargo install cargo-llvm-cov).

set -uo pipefail

PG_NAME=seaorm-auto-audit-cov-pg
MY_NAME=seaorm-auto-audit-cov-my
PG_PORT=55432
MY_PORT=53306
# One throwaway password for both containers, local use only.
PW="covtest"

command -v cargo-llvm-cov >/dev/null || {
    echo "cargo-llvm-cov not found: cargo install cargo-llvm-cov" >&2
    exit 1
}
rustup component add llvm-tools-preview >/dev/null 2>&1 || true

cleanup() {
    docker rm -f "$PG_NAME" "$MY_NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

docker run -d --name "$PG_NAME" \
    -e "POSTGRES_PASSWORD=${PW}" -e "POSTGRES_DB=covtest" \
    -p "$PG_PORT:5432" postgres:18-alpine >/dev/null
docker run -d --name "$MY_NAME" \
    -e "MYSQL_ROOT_PASSWORD=${PW}" -e "MYSQL_DATABASE=covtest" \
    -p "$MY_PORT:3306" mysql:8 >/dev/null

echo "waiting for postgres..."
for _ in $(seq 1 60); do
    if docker exec "$PG_NAME" pg_isready -U postgres >/dev/null 2>&1; then
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

# Debug builds give the suites large futures; match load_test.sh.
export RUST_MIN_STACK=16777216

COV_OPTS=(--all-features --show-missing-lines --ignore-filename-regex library/std)

echo
echo "================ coverage: main suite (SQLite) ================"
env -u DATABASE_URL -u MYSQL_DATABASE_URL \
    cargo llvm-cov "${COV_OPTS[@]}"

echo
echo "================ coverage: Postgres suite ================"
DATABASE_URL="postgres://postgres:${PW}@127.0.0.1:${PG_PORT}/covtest" \
    cargo llvm-cov "${COV_OPTS[@]}" --test postgres

echo
echo "================ coverage: MySQL suite ================"
MYSQL_DATABASE_URL="mysql://root:${PW}@127.0.0.1:${MY_PORT}/covtest" \
    cargo llvm-cov "${COV_OPTS[@]}" --test mysql
