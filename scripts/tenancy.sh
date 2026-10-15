#!/usr/bin/env bash
# Verify multi-tenant subdomain provisioning end to end, against the local dev
# Postgres (scripts/pg.sh up) and the JIRA in scripts/jira.env.
#
#   ./scripts/tenancy.sh      read-only check: boot a throwaway gateway on
#                             127.0.0.1:$PORT with WEAVN_BASE_DOMAIN=weavn.io, probe
#                             /auth/config and the Host-pinned /auth/jira login, then
#                             print the provisioned tenant / seat / login rows. Writes
#                             only to the LOCAL dev DB; the JIRA call is read-only
#                             (/myself). Stops the gateway on exit.
#   ./scripts/tenancy.sh -h   this help.
#
# The subdomain is derived from WEAVN_JIRA_BASE (domaintools.atlassian.net ->
# domaintools), so this provisions the "domaintools" tenant in the dev DB.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

case "${1:-}" in
  -h|--help) sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
  "") : ;;
  *) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
esac

for envf in scripts/db.env scripts/jira.env; do
  [ -f "$envf" ] || { echo "missing $envf" >&2; exit 1; }
  set -a; . "$envf"; set +a
done
: "${DATABASE_URL:?set in scripts/db.env}"
: "${WEAVN_JIRA_BASE:?set in scripts/jira.env}"
: "${WEAVN_JIRA_EMAIL:?set in scripts/jira.env}"
: "${WEAVN_JIRA_TOKEN:?set in scripts/jira.env}"

PORT="${PORT:-8097}"
BASE_DOMAIN="weavn.io"
SLUG="$(printf '%s' "$WEAVN_JIRA_BASE" | sed -E 's#https?://##; s#/.*##; s#:.*##; s#\..*##' | tr 'A-Z' 'a-z')"
TMP="$(mktemp -d)"; JAR="$TMP/cookies"; LOG="$TMP/gateway.log"
URL="http://127.0.0.1:$PORT"

cleanup() { [ -n "${PID:-}" ] && kill "$PID" 2>/dev/null || true; rm -rf "$TMP"; }
trap cleanup EXIT

echo "==> building gateway"; cargo build -q -p weavn-gateway
echo "==> booting gateway on $URL (base domain $BASE_DOMAIN, tenant slug: $SLUG)"
WEAVN_BASE_DOMAIN="$BASE_DOMAIN" WEAVN_JIRA_MULTITENANT=1 WEAVN_BIND="127.0.0.1:$PORT" \
  ./target/debug/weavn-gateway >"$LOG" 2>&1 & PID=$!

for i in $(seq 1 50); do curl -fsS "$URL/health" >/dev/null 2>&1 && break; sleep 0.2; done
curl -fsS "$URL/health" >/dev/null 2>&1 || { echo "gateway did not start:"; cat "$LOG"; exit 1; }

pass=0; fail=0
chk() { # desc  expected  actual
  if [ "$2" = "$3" ]; then echo "  ok   $1 ($3)"; pass=$((pass+1));
  else echo "  FAIL $1 — expected [$2] got [$3]"; fail=$((fail+1)); fi
}

echo "==> /auth/config at the apex (app.$BASE_DOMAIN)"
APEX="$(curl -fsS -H "Host: app.$BASE_DOMAIN" "$URL/auth/config")"
chk "multitenant"      "true" "$(printf '%s' "$APEX" | python3 -c 'import sys,json;print(str(json.load(sys.stdin)["multitenant"]).lower())')"
chk "apex subdomain=null" "None" "$(printf '%s' "$APEX" | python3 -c 'import sys,json;print(json.load(sys.stdin)["subdomain"])')"

echo "==> /auth/config at the tenant host ($SLUG.$BASE_DOMAIN)"
TEN="$(curl -fsS -H "Host: $SLUG.$BASE_DOMAIN" "$URL/auth/config")"
chk "tenant subdomain" "$SLUG" "$(printf '%s' "$TEN" | python3 -c 'import sys,json;print(json.load(sys.stdin)["subdomain"])')"

echo "==> apex /auth/jira is refused (must sign in at the workspace host)"
APEXCODE="$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H "Host: app.$BASE_DOMAIN" \
  -H 'content-type: application/json' \
  -d "{\"email\":\"$WEAVN_JIRA_EMAIL\",\"token\":\"x\"}" "$URL/auth/jira")"
chk "apex login blocked" "400" "$APEXCODE"

echo "==> Host-pinned login at $SLUG.$BASE_DOMAIN (authenticates against $WEAVN_JIRA_BASE)"
LOGINCODE="$(curl -sS -o "$TMP/login.json" -w '%{http_code}' -c "$JAR" -X POST \
  -H "Host: $SLUG.$BASE_DOMAIN" -H 'content-type: application/json' \
  -d "{\"email\":\"$WEAVN_JIRA_EMAIL\",\"token\":\"$WEAVN_JIRA_TOKEN\"}" "$URL/auth/jira")"
chk "login ok" "200" "$LOGINCODE"
if [ "$LOGINCODE" = "200" ]; then
  python3 - "$TMP/login.json" "$SLUG" <<'PY'
import sys,json
d=json.load(open(sys.argv[1])); slug=sys.argv[2]
print("  login response: subdomain=%s  usage=%s" % (d.get("subdomain"), d.get("usage")))
assert d.get("subdomain")==slug, "canonical subdomain mismatch"
assert d.get("session") and d["session"].get("subject"), "no session subject"
PY
  ME="$(curl -sS -o /dev/null -w '%{http_code}' -b "$JAR" -H "Host: $SLUG.$BASE_DOMAIN" "$URL/auth/me")"
  chk "session cookie valid (/auth/me)" "200" "$ME"
else
  echo "  (login failed — gateway log tail:)"; tail -5 "$LOG"
fi

echo "==> control-plane rows written to the dev DB"
psql_q() { podman exec -i weavn-pg psql -U weavn -d weavn -tAc "$1" 2>/dev/null; }
WS="$WEAVN_JIRA_BASE"
echo "  tenant:       $(psql_q "select subdomain||' <- '||jira_base||' ('||plan||')' from tenant where subdomain='$SLUG'")"
echo "  founding:     $(psql_q "select coalesce(founding_email,'(none)')||' / '||coalesce(founding_name,'(none)') from tenant where subdomain='$SLUG'")"
echo "  active seats: $(psql_q "select count(*) from seat where workspace='$WS' and active")"
echo "  login events: $(psql_q "select count(*) from login_event where subdomain='$SLUG'")"

echo
echo "RESULT: $pass checks passed, $fail failed."
[ "$fail" = 0 ]
