#!/usr/bin/env bash
# Local dev Postgres for Weavn via Podman.
#   up      -> start the weavn-pg container (postgres:16) on :5432 (idempotent)
#   down    -> stop & remove the container (keeps the data volume)
#   status  -> show the container and readiness
#   reset   -> down + delete the data volume (wipes the DB)
#   logs    -> tail container logs
# Connection string (put in scripts/db.env): postgresql://weavn:weavn@localhost:5432/weavn
set -euo pipefail

NAME=weavn-pg
VOL=weavn-pgdata
IMG=docker.io/library/postgres:16

case "${1:-status}" in
  up)
    if podman container exists "$NAME"; then
      podman start "$NAME" >/dev/null
    else
      podman run -d --name "$NAME" \
        -e POSTGRES_USER=weavn -e POSTGRES_PASSWORD=weavn -e POSTGRES_DB=weavn \
        -p 5432:5432 -v "$VOL":/var/lib/postgresql/data "$IMG" >/dev/null
    fi
    for i in $(seq 1 60); do
      if podman exec "$NAME" pg_isready -U weavn -d weavn >/dev/null 2>&1; then
        echo "postgres ready on localhost:5432 (user/db: weavn)"
        exit 0
      fi
      sleep 1
    done
    echo "postgres did not become ready in time" >&2
    exit 1
    ;;
  down)
    podman rm -f "$NAME" >/dev/null 2>&1 || true
    echo "stopped $NAME (data volume $VOL kept)"
    ;;
  reset)
    podman rm -f "$NAME" >/dev/null 2>&1 || true
    podman volume rm "$VOL" >/dev/null 2>&1 || true
    echo "removed $NAME and volume $VOL"
    ;;
  status)
    podman ps --filter "name=$NAME" --format '{{.Names}}  {{.Status}}' || true
    podman exec "$NAME" pg_isready -U weavn -d weavn 2>/dev/null || echo "not ready"
    ;;
  logs)
    podman logs --tail 30 "$NAME"
    ;;
  -h|--help)
    sed -n '2,9p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    ;;
  *)
    echo "usage: pg.sh up|down|status|reset|logs" >&2
    exit 2
    ;;
esac
