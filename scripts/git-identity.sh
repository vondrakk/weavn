#!/usr/bin/env bash
# Enforce the vondrakk identity on this repo and scrub Claude attribution.
#
#   ./scripts/git-identity.sh            read-only: audit author emails + Claude trailers
#   ./scripts/git-identity.sh --rewrite  re-author OLD_EMAIL -> NEW across ALL history and
#                                        strip Claude co-author trailers (keeps a backup ref)
#   ./scripts/git-identity.sh --push     force-push the current branch (--force-with-lease)
#
# --rewrite rewrites every commit hash. Only for a repo you control. It does NOT push;
# run --push afterwards (and make sure your github.com:vondrakk credential is the vondrakk
# account, so the push goes out as vondrakk).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "$ROOT"
OLD_EMAIL="vvondrakk@domaintools.com"
NEW_EMAIL="viktorvondrakk@gmail.com"
NEW_NAME="Viktor von Drakk"

audit() {
  echo "=== repo git identity (config) ==="
  echo "  $(git config user.name) <$(git config user.email)>"
  echo "=== author emails across all history ==="
  git log --all --format='%ae' | sort | uniq -c
  echo "=== commits whose message mentions Claude ==="
  local any=0
  while read -r h; do
    if git log -1 --format='%B' "$h" | grep -qi 'claude'; then
      echo "  $(git log -1 --format='%h %s' "$h")"; any=1
    fi
  done < <(git log --all --format='%H')
  [ "$any" = 0 ] && echo "  (none)"
  echo "=== branch vs remote ==="
  git status -sb | head -1
}

case "${1:-}" in
  --rewrite)
    # make sure forward config is right too
    git config user.name  "$NEW_NAME"
    git config user.email "$NEW_EMAIL"
    BK="backup/pre-identity-$(date +%Y%m%d%H%M%S)"
    git branch "$BK" >/dev/null 2>&1 || true
    echo ">> backup ref: $BK (current HEAD before rewrite)"
    export FILTER_BRANCH_SQUELCH_WARNING=1
    git filter-branch -f \
      --env-filter "
        if [ \"\$GIT_AUTHOR_EMAIL\" = \"$OLD_EMAIL\" ]; then
          export GIT_AUTHOR_EMAIL='$NEW_EMAIL'; export GIT_AUTHOR_NAME='$NEW_NAME'; fi
        if [ \"\$GIT_COMMITTER_EMAIL\" = \"$OLD_EMAIL\" ]; then
          export GIT_COMMITTER_EMAIL='$NEW_EMAIL'; export GIT_COMMITTER_NAME='$NEW_NAME'; fi
      " \
      --msg-filter "grep -v -iE '^Co-Authored-By:.*Claude|Generated with .*Claude Code' || true" \
      -- --all
    echo ">> rewrite complete. Review below, then: ./scripts/git-identity.sh --push"
    echo
    audit
    ;;
  --push)
    echo ">> force-pushing $(git rev-parse --abbrev-ref HEAD) to origin (--force-with-lease)"
    git push --force-with-lease origin "$(git rev-parse --abbrev-ref HEAD)"
    git status -sb | head -1
    ;;
  "")
    audit
    ;;
  *)
    echo "usage: $0 [--rewrite|--push]" >&2; exit 2 ;;
esac
