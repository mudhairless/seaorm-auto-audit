#!/usr/bin/env bash
# Cluster soak test for seaorm-auto-audit. Local Docker only; this
# never runs in CI, same deal as ../load_test.sh.
#
# Steps: `up` brings up the database and the api replicas; `run`
# also starts the three writers (profile tools) and streams their
# logs until they exit; `verify` reconciles the writer logs against
# the audit trail and exits with the verifier's code.
#
# Usage:
#   ./soak.sh up        # build and start the stack
#   ./soak.sh run       # up, then run the writers to completion
#   ./soak.sh verify    # check the trail against the writer logs
#   ./soak.sh down      # stop it and drop the database volume data
#   BACKEND=mysql ./soak.sh up
#
# Knobs: BACKEND (postgres default, or mysql), SOAK_PW (the throwaway
# database password), OPS (operations per writer, default 200).

set -euo pipefail
cd "$(dirname "$0")"

BACKEND="${BACKEND:-postgres}"
COMPOSE=(docker compose -f docker-compose.yml -f "docker-compose.$BACKEND.yml")

case "${1:-up}" in
    up)
        "${COMPOSE[@]}" up -d --build --force-recreate db api1 api2 api3
        echo "waiting for the api replicas..."
        for port in 18081 18082 18083; do
            for _ in $(seq 1 60); do
                if curl -fs "http://127.0.0.1:$port/healthz" >/dev/null 2>&1; then
                    break
                fi
                sleep 1
            done
            curl -fs "http://127.0.0.1:$port/healthz" >/dev/null
            echo "api on :$port is up"
        done
        ;;
    run)
        "$0" up
        "${COMPOSE[@]}" --profile tools up --build --no-deps writer0 writer1 writer2
        ;;
    verify)
        "${COMPOSE[@]}" --profile tools build verify
        "${COMPOSE[@]}" --profile tools run --rm verify
        ;;
    down)
        "${COMPOSE[@]}" down -v
        ;;
    *)
        echo "usage: ./soak.sh [up|run|verify|down]" >&2
        exit 2
        ;;
esac
