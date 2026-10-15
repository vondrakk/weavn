#!/usr/bin/env bash
# Scrub authorship/Claude/CLAUDE.md from the Weavn repo and republish clean.
#   (no arg)    -> read-only: show current state and the plan. Changes nothing.
#   rewrite     -> rebuild local history as ONE clean commit (personal identity,
#                  no Claude trailers, CLAUDE.md untracked). Local only.
#   recreate    -> DELETE the GitHub repo and recreate it, then push the clean
#                  history. Needs the delete_repo gh scope. Destructive + remote.
#   forcepush   -> force-push the clean history over the existing repo (no delete;
#                  uses the normal repo scope). Destructive + remote.
# Run from this terminal with:  ! scripts/republish-clean.sh [rewrite|recreate|forcepush]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

REPO="vondrakk/weavn"
NAME="vondrakk"
EMAIL="2789924+vondrakk@users.noreply.github.com"

read -r -d '' MSG <<'EOM' || true
weaveN — Slack-style team chat backed by JIRA

Messages are JIRA issues, replies are comments, and channel names drive the
boards. Rust workspace: an axum gateway (real-time hub, JIRA-token auth,
projection), a Postgres store, a CLI, and a single-page web client. Covers
channels, threads, reactions, search, mentions, file attachments, rich text,
status/transitions, private channels, notifications, scheduling, webhooks,
link unfurling, and sprints. See docs/ for the design and roadmap.
EOM

has_scope() { gh auth status 2>&1 | grep -q 'delete_repo'; }
# history_dirty uses grep -c (reads all input) to avoid a SIGPIPE early-close
# that pipefail would surface as a false "clean"

history_dirty() {
  # only the history that will be PUBLISHED (reachable from HEAD), not stale
  # remote-tracking refs like origin/main that recreate/push will replace
  local n
  n="$(git log HEAD --format='%ae %cn %ce %B' 2>/dev/null | grep -icE 'domaintools|claude' || true)"
  [ "${n:-0}" -gt 0 ]
}

status() {
  echo "repo:        $REPO"
  echo "gh user:     $(gh auth status 2>&1 | sed -n 's/.*Logged in to github.com as \([^ ]*\).*/\1/p' | head -1)"
  echo "identity →   $NAME <$EMAIL>"
  echo "commits (HEAD): $(git rev-list --count HEAD 2>/dev/null || echo 0)"
  echo "author emails to publish (HEAD):"; git log HEAD --format='  %ae' 2>/dev/null | sort -u || true
  echo -n "CLAUDE.md tracked: "; git ls-files | grep -qi '^CLAUDE.md$' && echo yes || echo no
  echo -n "history clean (no work-email / claude): "; history_dirty && echo "NO — run 'rewrite'" || echo yes
  echo -n "delete_repo scope: "; has_scope && echo present || echo "absent (grant: gh auth refresh -h github.com -s delete_repo)"
}

rewrite() {
  git config user.name "$NAME"
  git config user.email "$EMAIL"
  git checkout --orphan _clean -q          # keeps the tracked set staged, no parent
  git rm --cached -q CLAUDE.md 2>/dev/null || true
  git add .gitignore                        # include the CLAUDE.md ignore rule
  printf '%s\n' "$MSG" | git commit -q -F -
  git branch -D main -q 2>/dev/null || true
  git branch -m main
  echo "Local history rebuilt. Now:"
  git log --format='  %h  %an <%ae>  %s' | cat
  echo
  status
}

recreate() {
  if history_dirty; then echo "History still dirty — run '$0 rewrite' first."; exit 2; fi
  if ! has_scope; then echo "Missing delete_repo scope. Run: gh auth refresh -h github.com -s delete_repo"; exit 3; fi
  gh repo delete "$REPO" --yes
  gh repo create "$REPO" --public --description "weaveN — Slack-style team chat backed by JIRA"
  git push -u origin main
  echo "== read back =="; gh repo view "$REPO" --json nameWithOwner,isPrivate,pushedAt -q '.nameWithOwner+"  private="+(.isPrivate|tostring)+"  pushed="+.pushedAt'
  git log --format='  %h  %an <%ae>  %s' -1 | cat
}

forcepush() {
  if history_dirty; then echo "History still dirty — run '$0 rewrite' first."; exit 2; fi
  git push --force origin main
  echo "== read back =="; git log --format='  %h  %an <%ae>  %s' -1 | cat
}

case "${1:-}" in
  "") status ;;
  rewrite) rewrite ;;
  recreate) recreate ;;
  forcepush) forcepush ;;
  -h|--help) sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' ;;
  *) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
esac
