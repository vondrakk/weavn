#!/usr/bin/env bash
# Exercise the real JIRA Cloud path, reading creds from scripts/jira.env (git-ignored).
#
# Convention (per project standing instructions):
#   no argument  -> read-only: verify the token authenticates and the project is
#                   visible. Creates nothing.
#   --post       -> ACTION: create one test issue (and a comment) in the project,
#                   printing the issue key.
#   -h|--help    -> this help.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ENVF="scripts/jira.env"
if [[ ! -f "$ENVF" ]]; then
  echo "missing $ENVF" >&2
  echo "  cp scripts/jira.env.example scripts/jira.env   then fill in your creds" >&2
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
    echo "==> verify (read-only) against ${WEAVN_JIRA_BASE}"
    cargo run --quiet -p weavn-cli --features real-jira -- jira-verify
    ;;
  --post)
    echo "==> posting one test issue to ${WEAVN_JIRA_BASE} / project ${WEAVN_PROJECT}"
    cargo run --quiet -p weavn-cli --features real-jira -- \
      real-check "eng / backend / deploys" "demo" "Weavn real-check at $(date -u +%FT%TZ)"
    echo
    echo "RESULT: created an issue in ${WEAVN_PROJECT}; open it in JIRA to confirm the mapping."
    ;;
  *)
    echo "unknown argument: $1 (try --help)" >&2
    exit 2
    ;;
esac
