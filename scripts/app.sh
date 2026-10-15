#!/usr/bin/env bash
# Weavn SaaS gateway — build/push the image and deploy it (+ Postgres) to vv-k8s.
#
#   ./scripts/app.sh            read-only: show DB + app + ingress + cert state
#   ./scripts/app.sh --build    build the gateway image and push to DOCR (vrtmv)
#   ./scripts/app.sh --deploy   apply DB (CNPG) + app Deployment/Service/Ingress, roll out
#   ./scripts/app.sh --all      --build then --deploy
#
# Creds: DO token from $DO_CONF (default ~/projects/vrt/.conf/do.conf).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DO_CONF="${DO_CONF:-$HOME/projects/vrt/.conf/do.conf}"
CLUSTER_ID="${VV_CLUSTER_ID:-f730cdcb-ded8-45bc-92b6-bcab12f583a7}"
NS=weavn
# In-cluster GitLab container registry. IMAGE path must match a GitLab project
# (group/project) whose registry we can push to. Override with WEAVN_IMAGE.
REGISTRY="registry.git.vondrakk.com"
IMAGE="${WEAVN_IMAGE:-$REGISTRY/weavn/weavn/gateway}"
# Push + pull creds. Two ways to supply them, both gitignored, in precedence order:
#   1. scripts/gitlab-deploy.env — a GitLab deploy token:
#        GITLAB_DEPLOY_USER=gitlab+deploy-token-N   GITLAB_DEPLOY_TOKEN=...
#   2. a personal access token at $GL_ADMIN (default below): the file provides
#      TOKEN, used as GITLAB_DEPLOY_TOKEN so the secret is never copied into the
#      repo. For a PAT the registry username is the token OWNER's GitLab username —
#      set it as GITLAB_DEPLOY_USER (in gitlab-deploy.env or the environment).
# The token needs write_registry (push) + read_registry (cluster pull secret); the
# PAT also wants api scope so --build can auto-create the weavn/weavn project.
GL_ENV="$REPO/scripts/gitlab-deploy.env"
[ -f "$GL_ENV" ] && { set -a; . "$GL_ENV"; set +a; }
# Token source files (gitignored), first match wins. Each is either KEY=value lines
# (GITLAB_DEPLOY_TOKEN= or TOKEN=) or a file whose sole line is the raw token. Set
# GL_TOKEN_FILE to override. The secret is read here, never copied into the repo.
GL_TOKEN_FILES=(
  "${GL_TOKEN_FILE:-}"
  "$REPO/../.conf/gitlab-git.vondrakk.com.conf"
  "$HOME/projects/vrt/.conf/gitlab-admin-token.conf"
)
if [ -z "${GITLAB_DEPLOY_TOKEN:-}" ]; then
  for tf in "${GL_TOKEN_FILES[@]}"; do
    [ -n "$tf" ] && [ -f "$tf" ] || continue
    if grep -qE '^[[:space:]]*(export[[:space:]]+)?(GITLAB_DEPLOY_TOKEN|TOKEN)=' "$tf"; then
      # shellcheck disable=SC1090
      set -a; . "$tf"; set +a
      GITLAB_DEPLOY_TOKEN="${GITLAB_DEPLOY_TOKEN:-${TOKEN:-}}"
    else
      GITLAB_DEPLOY_TOKEN="$(grep -vE '^[[:space:]]*(#|$)' "$tf" | head -n1 | tr -d '[:space:]')"
    fi
    [ -n "${GITLAB_DEPLOY_TOKEN:-}" ] && break
  done
fi
GL_API="https://git.vondrakk.com/api/v4"
K8S="$REPO/deploy/k8s"
KC="$REPO/scripts/.vv-k8s.kubeconfig"
TAG="${TAG:-$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo dev)-$(date +%Y%m%d%H%M%S)}"

[ -f "$DO_CONF" ] || { echo "ERROR: DO creds not found at $DO_CONF" >&2; exit 1; }
# shellcheck disable=SC1090
set -a; source "$DO_CONF"; set +a
TOK="${DIGITALOCEAN_ACCESS_TOKEN:?DIGITALOCEAN_ACCESS_TOKEN missing in $DO_CONF}"

kube() { curl -fsS -H "Authorization: Bearer $TOK" \
  "https://api.digitalocean.com/v2/kubernetes/clusters/$CLUSTER_ID/kubeconfig" -o "$KC"; chmod 600 "$KC"; export KUBECONFIG="$KC"; }

# Create the weavn/weavn project (and the weavn group) if missing, so the registry
# path exists before we push. Best-effort; needs an api-scoped token (a PAT does,
# a registry-only deploy token does not — then the project must already exist).
ensure_project() {
  curl -fsS -H "PRIVATE-TOKEN: $GITLAB_DEPLOY_TOKEN" "$GL_API/projects/weavn%2Fweavn" >/dev/null 2>&1 && return 0
  echo ">> weavn/weavn project missing — trying to create it via the API"
  local gid
  gid="$(curl -fsS -H "PRIVATE-TOKEN: $GITLAB_DEPLOY_TOKEN" "$GL_API/groups/weavn" 2>/dev/null \
    | python3 -c 'import sys,json;print(json.load(sys.stdin).get("id",""))' 2>/dev/null || true)"
  if [ -z "$gid" ]; then
    curl -fsS -X POST -H "PRIVATE-TOKEN: $GITLAB_DEPLOY_TOKEN" "$GL_API/groups" \
      -d "name=weavn&path=weavn&visibility=private" >/dev/null 2>&1 || true
    gid="$(curl -fsS -H "PRIVATE-TOKEN: $GITLAB_DEPLOY_TOKEN" "$GL_API/groups/weavn" 2>/dev/null \
      | python3 -c 'import sys,json;print(json.load(sys.stdin).get("id",""))' 2>/dev/null || true)"
  fi
  if curl -fsS -X POST -H "PRIVATE-TOKEN: $GITLAB_DEPLOY_TOKEN" "$GL_API/projects" \
       -d "name=weavn&path=weavn&namespace_id=$gid&visibility=private" >/dev/null 2>&1; then
    echo ">> created weavn/weavn"
  else
    echo ">> WARN: could not auto-create weavn/weavn — create it in GitLab, then re-run" >&2
  fi
}

build() {
  : "${GITLAB_DEPLOY_USER:?set GITLAB_DEPLOY_USER (the PAT owner username, or gitlab+deploy-token-N) in $GL_ENV}"
  : "${GITLAB_DEPLOY_TOKEN:?set GITLAB_DEPLOY_TOKEN in $GL_ENV or provide a PAT at $GL_ADMIN}"
  ensure_project
  echo ">> building $IMAGE:$TAG (podman, context=$REPO)"
  podman build -t "$IMAGE:$TAG" -t "$IMAGE:latest" -f "$REPO/deploy/Dockerfile" "$REPO"
  echo ">> logging in to $REGISTRY and pushing"
  echo "$GITLAB_DEPLOY_TOKEN" | podman login "$REGISTRY" --username "$GITLAB_DEPLOY_USER" --password-stdin
  podman push "$IMAGE:$TAG"
  podman push "$IMAGE:latest"
  echo ">> pushed $IMAGE:$TAG"
}

# Give cert-manager the DigitalOcean DNS API token so the DNS-01 issuer can solve
# the wildcard *.weavn.io challenge. Lives in the cert-manager namespace (where
# ClusterIssuer solver secrets are read from), sourced from do.conf (same $TOK).
ensure_do_dns_secret() {
  kubectl -n cert-manager create secret generic digitalocean-dns \
    --from-literal=access-token="$TOK" \
    --dry-run=client -o yaml | kubectl apply -f -
}

# Ensure the namespace has a pull secret for the GitLab registry (like swpmt).
ensure_pull_secret() {
  : "${GITLAB_DEPLOY_USER:?set GITLAB_DEPLOY_USER in $GL_ENV}"
  : "${GITLAB_DEPLOY_TOKEN:?set GITLAB_DEPLOY_TOKEN in $GL_ENV}"
  kubectl -n "$NS" create secret docker-registry gitlab-registry-pull \
    --docker-server="$REGISTRY" \
    --docker-username="$GITLAB_DEPLOY_USER" \
    --docker-password="$GITLAB_DEPLOY_TOKEN" \
    --dry-run=client -o yaml | kubectl apply -f -
}

deploy() {
  kube
  echo ">> namespace"
  kubectl apply -f "$K8S/namespace.yaml"
  echo ">> gitlab registry pull secret"
  ensure_pull_secret
  echo ">> cert-manager DNS-01 issuer + DO token secret (wildcard *.weavn.io TLS)"
  ensure_do_dns_secret
  kubectl apply -f "$K8S/dns01-issuer.yaml"
  kubectl apply -f "$K8S/wildcard-cert.yaml"
  echo ">> Postgres (CNPG) — apply + wait for primary"
  kubectl apply -f "$K8S/db-cluster.yaml"
  kubectl -n "$NS" wait --for=condition=Ready cluster/weavn-pg --timeout=300s || \
    kubectl -n "$NS" get cluster weavn-pg
  echo ">> app config/secret/deployment/service/ingress (image tag: $TAG)"
  sed "s#$IMAGE:TAG#$IMAGE:$TAG#" "$K8S/app-deployment.yaml" | kubectl apply -f -
  kubectl apply -f "$K8S/app-ingress.yaml"
  echo ">> rollout"
  kubectl -n "$NS" rollout status deploy/weavn-gateway --timeout=180s
}

case "${1:-}" in
  --build)  build ;;
  --deploy) deploy ;;
  --all)    build; deploy ;;
  "")       : ;;  # read-only below
  *)        echo "usage: $0 [--build|--deploy|--all]" >&2; exit 2 ;;
esac

kube
echo
echo "================ Weavn SaaS — current state ================"
echo "--- postgres (CNPG) ---"; kubectl -n "$NS" get cluster weavn-pg 2>&1 || echo "(not created)"
echo "--- db pods ---";         kubectl -n "$NS" get pods -l cnpg.io/cluster=weavn-pg 2>&1 || true
echo "--- app pods ---";        kubectl -n "$NS" get pods -l app.kubernetes.io/name=weavn-gateway -o wide 2>&1 || true
echo "--- deployed image ---";  kubectl -n "$NS" get deploy weavn-gateway -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null; echo
echo "--- service/ingress ---"; kubectl -n "$NS" get svc weavn-gateway ingress weavn-gateway 2>&1 || true
echo "--- certificate ---";     kubectl -n "$NS" get certificate weavn-wildcard 2>&1 || true
echo "--- dns-01 issuer ---";    kubectl get clusterissuer letsencrypt-dns 2>&1 || true
LB_IP="$(kubectl -n ingress-nginx get svc ingress-nginx-controller -o jsonpath='{.status.loadBalancer.ingress[0].ip}' 2>/dev/null || true)"
if [ -n "$LB_IP" ]; then
  code=$(curl -s -o /dev/null -w '%{http_code}' --resolve "app.weavn.io:443:$LB_IP" https://app.weavn.io/health 2>/dev/null || echo "---")
  echo "--- live: https://app.weavn.io/health -> HTTP $code (via cluster LB) ---"
fi
echo "============================================================"
