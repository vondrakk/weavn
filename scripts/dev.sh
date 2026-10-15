#!/usr/bin/env bash
# Weavn prototype dev script.
#
# Convention (per project standing instructions):
#   no argument  -> read-only check: build, test, and run the in-memory demo.
#                   Touches only ./target; does not post anywhere external.
#   -h|--help    -> this help.
#
# For the live JIRA path, use ./scripts/jira.sh (no-arg verify, --post to write).
#
# After doing its work the script prints the result back.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

case "${1:-}" in
  -h|--help)
    sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "")
    echo "==> cargo build"
    cargo build --workspace
    echo "==> cargo test"
    cargo test --workspace
    echo "==> weavn demo (read-only, in-memory)"
    cargo run --quiet -p weavn-cli -- demo
    echo
    echo "RESULT: build + tests passed; demo ran above."
    ;;
  *)
    echo "unknown argument: $1 (try --help); for live JIRA use ./scripts/jira.sh" >&2
    exit 2
    ;;
esac
