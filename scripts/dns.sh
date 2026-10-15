#!/usr/bin/env bash
# Weavn DNS — manage the weavn.io zone in DigitalOcean DNS.
#
#   ./scripts/dns.sh           read-only: show the DO zone + records and live resolution
#   ./scripts/dns.sh --ensure  create the zone if missing; ensure A @ and A www point at
#                              the cluster ingress LB; then print the records back
#
# Creds: DigitalOcean token from $DO_CONF (default ~/projects/vrt/.conf/do.conf).
set -euo pipefail

DOMAIN="${WEAVN_DOMAIN:-weavn.io}"
DO_CONF="${DO_CONF:-$HOME/projects/vrt/.conf/do.conf}"
LB_IP="${WEAVN_LB_IP:-24.199.69.92}"   # vv-k8s ingress-nginx LoadBalancer
API="https://api.digitalocean.com/v2"

[ -f "$DO_CONF" ] || { echo "ERROR: DO creds not found at $DO_CONF" >&2; exit 1; }
# shellcheck disable=SC1090
set -a; source "$DO_CONF"; set +a
TOK="${DIGITALOCEAN_ACCESS_TOKEN:?DIGITALOCEAN_ACCESS_TOKEN missing in $DO_CONF}"

doapi() { curl -fsS -H "Authorization: Bearer $TOK" -H "Content-Type: application/json" "$@"; }

zone_exists() { curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOK" "$API/domains/$DOMAIN" | grep -q 200; }

# find a record id by type+name ("" name = apex, printed as "@")
record_id() {
  local rtype="$1" rname="$2"
  doapi "$API/domains/$DOMAIN/records?per_page=200" \
    | python3 -I -c "import sys,json;r=json.load(sys.stdin)['domain_records'];print(next((str(x['id']) for x in r if x['type']=='$rtype' and x['name']=='$rname'),''))"
}

ensure_A() {
  local name="$1" label="$2" id
  id="$(record_id A "$name")"
  if [ -n "$id" ]; then
    echo ">> updating A $label -> $LB_IP (id $id)"
    doapi -X PUT "$API/domains/$DOMAIN/records/$id" -d "{\"data\":\"$LB_IP\",\"ttl\":1800}" >/dev/null
  else
    echo ">> creating A $label -> $LB_IP"
    doapi -X POST "$API/domains/$DOMAIN/records" \
      -d "{\"type\":\"A\",\"name\":\"$name\",\"data\":\"$LB_IP\",\"ttl\":1800}" >/dev/null
  fi
}

if [ "${1:-}" = "--ensure" ]; then
  if zone_exists; then
    echo ">> zone $DOMAIN already in DO DNS"
  else
    echo ">> creating zone $DOMAIN in DO DNS (apex A -> $LB_IP)"
    doapi -X POST "$API/domains" -d "{\"name\":\"$DOMAIN\",\"ip_address\":\"$LB_IP\"}" >/dev/null
  fi
  ensure_A "@"   "@"
  ensure_A "www" "www"
  ensure_A "app" "app"   # app.weavn.io — the SaaS gateway apex redirector
  ensure_A "*"   "*"     # *.weavn.io — one A record per tenant subdomain (wildcard)
elif [ -n "${1:-}" ]; then
  echo "usage: $0 [--ensure]" >&2; exit 2
fi

echo
echo "================ $DOMAIN — DO DNS state ================"
if zone_exists; then
  echo "--- records (type / name / data / ttl) ---"
  doapi "$API/domains/$DOMAIN/records?per_page=200" \
    | python3 -I -c "import sys,json;[print(f\"{x['type']:6} {x['name']:20} {x['data']:28} ttl={x['ttl']}\") for x in json.load(sys.stdin)['domain_records']]"
  echo "--- DO nameservers for this zone: ns1/2/3.digitalocean.com ---"
else
  echo "zone $DOMAIN is NOT in DO DNS yet (run with --ensure to create it)"
fi
echo "--- live resolution (public, may be cached) ---"
echo "NS : $(dig +short NS "$DOMAIN" @1.1.1.1 2>/dev/null | tr '\n' ' ')"
echo "A  : $(dig +short A  "$DOMAIN" @1.1.1.1 2>/dev/null | tr '\n' ' ')"
echo "    (target LB IP = $LB_IP)"
echo "======================================================="
