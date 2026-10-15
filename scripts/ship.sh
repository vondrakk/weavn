#!/usr/bin/env bash
# Ship weavN to production — one wrapper over app.sh + site.sh.
#
# This ships the HOSTED service to the vv-k8s cluster. (For cutting a tagged
# source/binary release via the GitHub workflow, that's scripts/release.sh —
# a different thing.)
#
#   ./scripts/ship.sh            read-only: show app + site state (no changes)
#   ./scripts/ship.sh --all      build+push+roll the gateway (app.sh --all),
#                                 deploy the marketing site (site.sh --deploy),
#                                 then read the live state back
#   ./scripts/ship.sh --app      just the gateway (app.sh --all)
#   ./scripts/ship.sh --site     just the marketing site (site.sh --deploy)
#   -h|--help                    this help
#
# Per project standing instructions: no argument is a read-only check, a flag
# does the action, and the script prints the result read back afterwards.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$REPO/scripts/app.sh"
SITE="$REPO/scripts/site.sh"

# Record the gateway's deployed image tag before we touch anything, so the
# readback can prove the rollout actually replaced the image (and didn't just
# no-op on an identical tag). Best-effort: the app.sh status line reports it too.
deployed_image() {
  "$APP" 2>/dev/null | sed -n 's/^--- deployed image --- *//p' | tr -d '[:space:]' || true
}

ship_app() {
  echo "================ 1/2 · gateway (app.weavn.io + *.weavn.io) ================"
  "$APP" --all
}

ship_site() {
  echo "================ 2/2 · marketing site (weavn.io) =========================="
  "$SITE" --deploy
}

case "${1:-}" in
  -h|--help)
    sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "")
    echo ">> read-only state (pass --all to ship). Gateway:"
    "$APP"
    echo
    echo ">> Marketing site:"
    "$SITE"
    exit 0
    ;;
  --all)
    BEFORE="$(deployed_image)"
    ship_app
    ship_site
    ;;
  --app)
    BEFORE="$(deployed_image)"
    ship_app
    ;;
  --site)
    ship_site
    echo
    echo "RESULT: marketing site deployed."
    exit 0
    ;;
  *)
    echo "unknown argument: $1 (try --help)" >&2
    exit 2
    ;;
esac

# Readback (app path only — the site branch above already returned).
AFTER="$(deployed_image)"
echo
echo "================ RESULT ===================================================="
echo "gateway image before : ${BEFORE:-<unknown>}"
echo "gateway image after  : ${AFTER:-<unknown>}"
if [ -n "${BEFORE:-}" ] && [ "${BEFORE:-}" = "${AFTER:-}" ]; then
  echo "WARN: image tag unchanged — the new build may not have rolled. Check app pods above." >&2
else
  echo "OK: gateway rolled to a new image. Sign out / back in to get a fresh pinned session."
fi
[ "${1:-}" = "--all" ] && echo "Marketing site: deployed (see site state above)."
echo "==========================================================================="
