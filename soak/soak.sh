#!/usr/bin/env bash
# Cluster soak test for seaorm-auto-audit. Local Docker only; this
# never runs in CI, same deal as ../load_test.sh.
#
# Step 1 (this version): bring up the database and the api replicas
# and wait until every replica answers. The writer and verifier
# steps join this script as their binaries land.
#
# Usage:
#   ./soak.sh up        # build and start the stack
#   ./soak.sh down      # stop it and drop the database volume data
#   BACKEND=mysql ./soak.sh up
#
# Knobs: BACKEND (postgres default, or mysql), SOAK_PW (the throwaway
# database password).

set -euo pipefail
cd "$(dirname "$0")"

BACKEND="${BACKEND:-postgres}"
COMPOSE=(docker compose -f docker-compose.yml)
if [ "$BACKEND" = "mysql" ]; then
    COMPOSE+=(-f docker-compose.mysql.yml)
fi

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
    down)
        "${COMPOSE[@]}" down -v
        ;;
    *)
        echo "usage: ./soak.sh [up|down]" >&2
        exit 2
        ;;
esac
