#!/usr/bin/env bash
# Weavn marketing site — deploy to / inspect in the vv-k8s cluster (DigitalOcean).
#
#   ./scripts/site.sh            read-only: show deployed state + live HTTP check
#   ./scripts/site.sh --deploy   (re)build ConfigMaps from ./site, apply manifests, roll out
#
# Creds: DigitalOcean token from $DO_CONF (default ~/projects/vrt/.conf/do.conf).
# The cluster kubeconfig is fetched fresh to scripts/.vv-k8s.kubeconfig (gitignored).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DO_CONF="${DO_CONF:-$HOME/projects/vrt/.conf/do.conf}"
CLUSTER_ID="${VV_CLUSTER_ID:-f730cdcb-ded8-45bc-92b6-bcab12f583a7}"   # vv-k8s, sfo3
NS=weavn
SITE="$REPO/site"
K8S="$REPO/deploy/k8s"
KC="$REPO/scripts/.vv-k8s.kubeconfig"

[ -f "$DO_CONF" ] || { echo "ERROR: DO creds not found at $DO_CONF" >&2; exit 1; }
# shellcheck disable=SC1090
set -a; source "$DO_CONF"; set +a
TOK="${DIGITALOCEAN_ACCESS_TOKEN:?DIGITALOCEAN_ACCESS_TOKEN missing in $DO_CONF}"

echo ">> fetching vv-k8s kubeconfig"
curl -fsS -H "Authorization: Bearer $TOK" \
  "https://api.digitalocean.com/v2/kubernetes/clusters/$CLUSTER_ID/kubeconfig" -o "$KC"
chmod 600 "$KC"
export KUBECONFIG="$KC"

deploy() {
  echo ">> applying namespace"
  kubectl apply -f "$K8S/namespace.yaml"

  echo ">> building ConfigMaps from $SITE"
  # root files (served at /) — every file directly under site/ (assets/ is a dir)
  root_args=()
  for p in "$SITE"/*; do
    [ -f "$p" ] && root_args+=( --from-file="$(basename "$p")=$p" )
  done
  kubectl -n "$NS" create configmap weavn-site-root "${root_args[@]}" \
    --dry-run=client -o yaml | kubectl apply -f -

  # assets (served at /assets/) — handles binary (og.png) via binaryData automatically
  kubectl -n "$NS" create configmap weavn-site-assets --from-file="$SITE/assets" \
    --dry-run=client -o yaml | kubectl apply -f -

  # nginx server config
  kubectl -n "$NS" create configmap weavn-site-nginx \
    --from-file=default.conf="$K8S/nginx.conf" \
    --dry-run=client -o yaml | kubectl apply -f -

  echo ">> applying deployment, service, ingress"
  HASH="$(cat "$SITE"/index.html "$SITE"/styles.css "$SITE"/assets/* "$K8S/nginx.conf" | sha256sum | cut -c1-12)"
  sed "s/PLACEHOLDER/$HASH/" "$K8S/deployment.yaml" | kubectl apply -f -
  kubectl apply -f "$K8S/ingress.yaml"

  echo ">> waiting for rollout"
  kubectl -n "$NS" rollout status deploy/weavn-web --timeout=120s
}

if [ "${1:-}" = "--deploy" ]; then
  deploy
elif [ -n "${1:-}" ]; then
  echo "usage: $0 [--deploy]" >&2; exit 2
fi

echo
echo "================ Weavn site — current state ================"
echo "--- pods ---";       kubectl -n "$NS" get pods -o wide 2>&1 || true
echo "--- service ---";    kubectl -n "$NS" get svc 2>&1 || true
echo "--- ingress ---";    kubectl -n "$NS" get ingress 2>&1 || true
echo "--- certificate ---";kubectl -n "$NS" get certificate 2>&1 || true
echo "--- configmaps ---"; kubectl -n "$NS" get cm 2>&1 | grep -v kube-root-ca || true

LB_IP="$(kubectl -n ingress-nginx get svc ingress-nginx-controller \
  -o jsonpath='{.status.loadBalancer.ingress[0].ip}' 2>/dev/null || true)"
echo "--- live HTTP check via ingress (${LB_IP:-?}) ---"
if [ -n "$LB_IP" ]; then
  code=$(curl -s -o /dev/null -w '%{http_code}' --resolve "weavn.io:443:$LB_IP" https://weavn.io/ 2>/dev/null || echo "---")
  echo "https://weavn.io/        -> HTTP $code (resolved to cluster LB; TLS may warn until cert is issued)"
else
  echo "could not determine ingress LB IP"
fi
echo "============================================================"
