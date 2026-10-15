#!/usr/bin/env bash
# Operational store (Postgres/Supabase) admin, reading DATABASE_URL from
# scripts/db.env (git-ignored).
#
# Convention (per project standing instructions):
#   no argument  -> read-only: check the DB is reachable (weavndb ping).
#   --migrate    -> ACTION: apply migrations (weavndb migrate).
#   -- <args>    -> pass the rest straight to weavndb (e.g. -- config list acme,
#                   -- emoji add acme ':~Y:' <image> <me> <me>,<dan>).
#   -h|--help    -> this help.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ENVF="scripts/db.env"
if [[ ! -f "$ENVF" ]]; then
  echo "missing $ENVF — cp scripts/db.env.example scripts/db.env then fill in DATABASE_URL" >&2
  exit 2
fi
# shellcheck disable=SC1090
set -a; source "$ENVF"; set +a

case "${1:-}" in
  -h|--help)
    sed -n '2,11p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "")
    cargo run --quiet -p weavn-store --bin weavndb -- ping
    ;;
  --migrate)
    cargo run --quiet -p weavn-store --bin weavndb -- migrate
    ;;
  --)
    shift
    cargo run --quiet -p weavn-store --bin weavndb -- "$@"
    ;;
  *)
    echo "unknown argument: $1 (try --help)" >&2
    exit 2
    ;;
esac
