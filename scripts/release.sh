#!/usr/bin/env bash
# Cut a Weavn release.
#
# Convention (per project standing instructions):
#   no argument      -> read-only: show the workspace version and existing tags.
#   --tag vX.Y.Z     -> ACTION: create annotated tag vX.Y.Z and push it to origin,
#                       which triggers the GitHub `release` workflow (Linux x86_64
#                       + macOS arm64 binaries). Requires an `origin` remote.
#   -h|--help        -> this help.
#
# After doing its work the script prints the result back.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

workspace_version() {
  # version lives in [workspace.package] in the root Cargo.toml
  grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/'
}

case "${1:-}" in
  -h|--help)
    sed -n '2,13p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "")
    echo "workspace version : $(workspace_version)"
    echo "existing tags     :"
    git tag --list 'v*' --sort=-v:refname | sed 's/^/  /' || true
    echo
    echo "to release: $0 --tag v$(workspace_version)"
    ;;
  --tag)
    TAG="${2:-}"
    [[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
      echo "error: expected a tag like v0.1.0, got '${TAG}'" >&2
      exit 2
    }
    git remote get-url origin >/dev/null 2>&1 || {
      echo "error: no 'origin' remote yet — create the GitHub repo and add origin first" >&2
      exit 2
    }
    git tag -a "$TAG" -m "Weavn $TAG"
    git push origin "$TAG"
    echo
    echo "RESULT: pushed $TAG to origin; the release workflow is now building binaries."
    echo "        watch it at: $(git remote get-url origin | sed -E 's#(git@|https://)github.com[:/]#https://github.com/#; s/\.git$//')/actions"
    ;;
  *)
    echo "unknown argument: $1 (try --help)" >&2
    exit 2
    ;;
esac
