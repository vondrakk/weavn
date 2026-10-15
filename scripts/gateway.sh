#!/usr/bin/env bash
# Run the Weavn gateway (axum HTTP server, mock backend) on 127.0.0.1:8080.
#   no argument -> build and run the gateway in the foreground.
#   -h|--help   -> this help.
# Try it:  curl -s localhost:8080/health
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Optional: load auth config if present. oidc.env → Okta SSO; jira.env → JIRA-token
# login (only WEAVN_JIRA_BASE / WEAVN_JIRA_AUTH are used here, not the token).
for envf in scripts/db.env scripts/oidc.env scripts/jira.env; do
  if [[ -f "$envf" ]]; then
    # shellcheck disable=SC1090
    set -a; source "$envf"; set +a
  fi
done

case "${1:-}" in
  -h|--help)
    sed -n '2,5p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "")
    exec cargo run -p weavn-gateway
    ;;
  *)
    echo "unknown argument: $1 (try --help)" >&2
    exit 2
    ;;
esac
