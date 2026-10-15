#!/usr/bin/env bash
# Get back into the self-hosted GitLab at https://git.vondrakk.com — omnibus CE
# (gitlab/gitlab-ce) running in the vv-k8s cluster as statefulset `gitlab`, pod
# `gitlab-0`, namespace `gitlab`.
#
#   ./scripts/gitlab-reset.sh             read-only: show the GitLab pod + version,
#                                         the login URL, and the next steps.
#   ./scripts/gitlab-reset.sh --show-root print the current `root` password from the
#                                         `gitlab-root` Secret, so you can just log in.
#   ./scripts/gitlab-reset.sh --reset     reset the `root` password via the in-pod
#                                         rake task (prompts for the new one on a TTY;
#                                         it never touches argv, env, or history).
#
# Creds: DigitalOcean token from $DO_CONF (default ~/projects/vrt/.conf/do.conf),
# used to fetch a fresh kubeconfig (like the other cluster scripts).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
DO_CONF="${DO_CONF:-$HOME/projects/vrt/.conf/do.conf}"
CLUSTER_ID="${VV_CLUSTER_ID:-f730cdcb-ded8-45bc-92b6-bcab12f583a7}"
KC="$REPO/scripts/.vv-k8s.kubeconfig"
NS=gitlab
POD=gitlab-0
URL="https://git.vondrakk.com"

case "${1:-}" in
  -h|--help) sed -n '2,19p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
  ""|--show-root|--reset) : ;;
  *) echo "usage: $0 [--show-root|--reset]" >&2; exit 2 ;;
esac

[ -f "$DO_CONF" ] || { echo "ERROR: DO creds not found at $DO_CONF" >&2; exit 1; }
# shellcheck disable=SC1090
set -a; source "$DO_CONF"; set +a
TOK="${DIGITALOCEAN_ACCESS_TOKEN:?DIGITALOCEAN_ACCESS_TOKEN missing in $DO_CONF}"
curl -fsS -H "Authorization: Bearer $TOK" \
  "https://api.digitalocean.com/v2/kubernetes/clusters/$CLUSTER_ID/kubeconfig" -o "$KC"
chmod 600 "$KC"; export KUBECONFIG="$KC"

case "${1:-}" in
  --show-root)
    echo ">> current 'root' password from the gitlab-root Secret:"
    kubectl -n "$NS" get secret gitlab-root -o json \
      | python3 -c 'import sys,json,base64;d=json.load(sys.stdin)["data"];k=next(iter(d));print(base64.b64decode(d[k]).decode())'
    echo ">> log in at $URL as user: root"
    exit 0
    ;;
  --reset)
    echo ">> resetting the 'root' password inside $POD (you will be prompted)."
    echo ">> if the prompt doesn't appear, the pod shell has no TTY — re-run from an"
    echo "   interactive terminal."
    kubectl -n "$NS" exec -it "$POD" -- gitlab-rake "gitlab:password:reset[root]"
    echo ">> done. Log in at $URL as user: root"
    exit 0
    ;;
esac

# read-only state + guidance
echo "================ GitLab (git.vondrakk.com) — state ================"
echo "--- pod ---";     kubectl -n "$NS" get pod "$POD" -o wide 2>&1 || true
echo "--- image ---";   kubectl -n "$NS" get pod "$POD" -o jsonpath='{.spec.containers[0].image}' 2>/dev/null; echo
echo "--- version ---"; kubectl -n "$NS" exec "$POD" -- cat /opt/gitlab/version-manifest.txt 2>/dev/null | head -1 || true
code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 8 "$URL/users/sign_in" 2>/dev/null || echo "---")
echo "--- login page $URL/users/sign_in -> HTTP $code ---"
cat <<EOF
===================================================================
To get back in:
  1. ./scripts/gitlab-reset.sh --show-root   # try the stored root password first
     (or ./scripts/gitlab-reset.sh --reset   # set a new one if that fails)
  2. Sign in at $URL as 'root'.
  3. Mint a token for the deploy: User Settings -> Access Tokens ->
     scopes api, read_registry, write_registry. (As root you can also just use
     the API now.)
  4. Put it in scripts/gitlab-deploy.env:  GITLAB_DEPLOY_USER=root
     (the PAT itself is auto-sourced by app.sh from the admin-token file, or set
      GITLAB_DEPLOY_TOKEN here). Then: ./scripts/app.sh --all
===================================================================
EOF
