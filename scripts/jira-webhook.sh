#!/usr/bin/env bash
# Register (or inspect) the JIRA Cloud -> weavN inbound webhook, so that issue and
# comment activity in JIRA flows live into the weavN stream.
#
# Creds come from scripts/jira.env (git-ignored), same as scripts/jira.sh.
#
# Convention (per project standing instructions):
#   no argument        -> read-only: authenticate and LIST the webhooks currently
#                         registered on the site, flagging the ones that point at
#                         weavN. Creates nothing.
#   --register <url>    -> ACTION: register a webhook for this project's issue +
#                         comment events, pointing at <url>. <url> is the weavN
#                         hook URL (looks like https://app.weavn.io/jira/hook/<secret>);
#                         get it from weavN:  curl -s --cookie "<your session>" \
#                         https://app.weavn.io/jira/webhook   or the weavN UI.
#                         May also be supplied via WEAVN_HOOK_URL in jira.env.
#   --delete <id>       -> ACTION: delete the webhook with that numeric id
#                         (as shown by the no-arg listing).
#   -h|--help           -> this help.
#
# After any action the script re-lists the webhooks so you can see the result.
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

: "${WEAVN_JIRA_BASE:?set WEAVN_JIRA_BASE in scripts/jira.env}"
: "${WEAVN_JIRA_TOKEN:?set WEAVN_JIRA_TOKEN in scripts/jira.env}"
AUTH_MODE="${WEAVN_JIRA_AUTH:-basic}"
HOOK_API="${WEAVN_JIRA_BASE%/}/rest/webhooks/1.0/webhook"

# Build the curl auth args for the configured mode.
auth_args() {
  if [[ "$AUTH_MODE" == "bearer" ]]; then
    printf -- '-H\nAuthorization: Bearer %s\n' "$WEAVN_JIRA_TOKEN"
  else
    : "${WEAVN_JIRA_EMAIL:?set WEAVN_JIRA_EMAIL for basic auth}"
    printf -- '-u\n%s:%s\n' "$WEAVN_JIRA_EMAIL" "$WEAVN_JIRA_TOKEN"
  fi
}
mapfile -t AUTH < <(auth_args)

# List registered webhooks, flagging weavN ones. Needs python3 for pretty output;
# falls back to raw JSON if absent.
list_hooks() {
  echo "==> webhooks on ${WEAVN_JIRA_BASE}"
  local json
  json="$(curl -sf "${AUTH[@]}" -H 'Accept: application/json' "$HOOK_API")" || {
    echo "   (failed to list webhooks -- check creds / permissions)" >&2
    return 1
  }
  if command -v python3 >/dev/null 2>&1; then
    printf '%s' "$json" | python3 -c '
import sys, json
for w in json.load(sys.stdin):
    wid = (w.get("self","").rstrip("/").split("/")[-1]) or "?"
    url = w.get("url","")
    tag = "  <-- weavN" if "/jira/hook/" in url else ""
    print(f"   [{wid}] {w.get(\"name\",\"?\")}  {url}{tag}")
    print(f"         events: {', '.join(w.get(\"events\",[]))}")
' || printf '%s\n' "$json"
  else
    printf '%s\n' "$json"
  fi
}

register() {
  local url="$1"
  [[ -n "$url" ]] || { echo "no URL given (pass one, or set WEAVN_HOOK_URL)" >&2; exit 2; }
  : "${WEAVN_PROJECT:?set WEAVN_PROJECT in scripts/jira.env}"
  echo "==> registering weavN webhook -> ${url}"
  echo "    project filter: ${WEAVN_PROJECT}"
  local body
  body="$(cat <<JSON
{
  "name": "weavN ${WEAVN_PROJECT}",
  "url": "${url}",
  "events": ["jira:issue_created","jira:issue_updated","comment_created","comment_updated"],
  "filters": { "issue-related-events-section": "project = ${WEAVN_PROJECT}" },
  "excludeBody": false
}
JSON
)"
  curl -sf -X POST "${AUTH[@]}" \
    -H 'Content-Type: application/json' -H 'Accept: application/json' \
    --data "$body" "$HOOK_API" >/dev/null || {
    echo "   registration failed" >&2; exit 1;
  }
  echo "   registered."
  echo
  list_hooks
}

delete_hook() {
  local id="$1"
  [[ -n "$id" ]] || { echo "no webhook id given" >&2; exit 2; }
  echo "==> deleting webhook ${id}"
  curl -sf -X DELETE "${AUTH[@]}" "${HOOK_API}/${id}" >/dev/null || {
    echo "   delete failed" >&2; exit 1;
  }
  echo "   deleted."
  echo
  list_hooks
}

case "${1:-}" in
  -h|--help)
    sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    ;;
  "")
    list_hooks
    ;;
  --register)
    register "${2:-${WEAVN_HOOK_URL:-}}"
    ;;
  --delete)
    delete_hook "${2:-}"
    ;;
  *)
    echo "unknown argument: $1 (try --help)" >&2
    exit 2
    ;;
esac
