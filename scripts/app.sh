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
# Push + pull creds: a GitLab deploy token (scopes: write_registry to push,
# read_registry for the cluster pull secret). Put them in scripts/gitlab-deploy.env
# (gitignored):  GITLAB_DEPLOY_USER=gitlab+deploy-token-N   GITLAB_DEPLOY_TOKEN=...
GL_ENV="$REPO/scripts/gitlab-deploy.env"
[ -f "$GL_ENV" ] && { set -a; . "$GL_ENV"; set +a; }
K8S="$REPO/deploy/k8s"
KC="$REPO/scripts/.vv-k8s.kubeconfig"
TAG="${TAG:-$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo dev)-$(date +%Y%m%d%H%M%S)}"

[ -f "$DO_CONF" ] || { echo "ERROR: DO creds not found at $DO_CONF" >&2; exit 1; }
# shellcheck disable=SC1090
set -a; source "$DO_CONF"; set +a
TOK="${DIGITALOCEAN_ACCESS_TOKEN:?DIGITALOCEAN_ACCESS_TOKEN missing in $DO_CONF}"

kube() { curl -fsS -H "Authorization: Bearer $TOK" \
  "https://api.digitalocean.com/v2/kubernetes/clusters/$CLUSTER_ID/kubeconfig" -o "$KC"; chmod 600 "$KC"; export KUBECONFIG="$KC"; }

build() {
  : "${GITLAB_DEPLOY_USER:?set GITLAB_DEPLOY_USER (e.g. gitlab+deploy-token-N) in $GL_ENV}"
  : "${GITLAB_DEPLOY_TOKEN:?set GITLAB_DEPLOY_TOKEN in $GL_ENV}"
  echo ">> building $IMAGE:$TAG (podman, context=$REPO)"
  podman build -t "$IMAGE:$TAG" -t "$IMAGE:latest" -f "$REPO/deploy/Dockerfile" "$REPO"
  echo ">> logging in to $REGISTRY and pushing"
  echo "$GITLAB_DEPLOY_TOKEN" | podman login "$REGISTRY" --username "$GITLAB_DEPLOY_USER" --password-stdin
  podman push "$IMAGE:$TAG"
  podman push "$IMAGE:latest"
  echo ">> pushed $IMAGE:$TAG"
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
echo "--- certificate ---";     kubectl -n "$NS" get certificate weavn-app-tls 2>&1 || true
LB_IP="$(kubectl -n ingress-nginx get svc ingress-nginx-controller -o jsonpath='{.status.loadBalancer.ingress[0].ip}' 2>/dev/null || true)"
if [ -n "$LB_IP" ]; then
  code=$(curl -s -o /dev/null -w '%{http_code}' --resolve "app.weavn.io:443:$LB_IP" https://app.weavn.io/health 2>/dev/null || echo "---")
  echo "--- live: https://app.weavn.io/health -> HTTP $code (via cluster LB) ---"
fi
echo "============================================================"
