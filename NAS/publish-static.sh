#!/bin/sh
set -eu

: "${PUBLISH_CHECKOUT:?Set PUBLISH_CHECKOUT to the reading-site clone}"
: "${WORK_ROOT:?Set WORK_ROOT to a staging directory outside the checkout}"
: "${SITE_URL:?Set SITE_URL including the preview base path or production root}"
: "${STATIC_IMAGE:?Set STATIC_IMAGE to a reviewed image tag or digest}"
: "${CACHE_VOLUME:?Set CACHE_VOLUME to a dedicated static-export cache volume}"
: "${GSRF_MAILTO:?Set GSRF_MAILTO}"
if [ "${GSRF_PROVIDER:-openalex}" = openalex ]; then
  : "${GSRF_OPENALEX_API_KEY:?Set a private verified OpenAlex API key for cursor paging}"
fi

DOCKER_BIN=${DOCKER_BIN:-docker}
GH_BIN=${GH_BIN:-gh}
PUBLISH_REPOSITORY=${PUBLISH_REPOSITORY:-michaelmvh/reading-site}
COMPOSE_FILE=${COMPOSE_FILE:-"$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/static-compose.yml"}

for tool in "$DOCKER_BIN" "$GH_BIN" git rsync find; do
  command -v "$tool" >/dev/null 2>&1 || { echo "Required command is unavailable: $tool" >&2; exit 1; }
done
PUBLISH_CHECKOUT=$(CDPATH= cd -- "$PUBLISH_CHECKOUT" && pwd -P)
export PUBLISH_CHECKOUT
[ "$(git -C "$PUBLISH_CHECKOUT" rev-parse --show-toplevel)" = "$PUBLISH_CHECKOUT" ] ||
  { echo "PUBLISH_CHECKOUT must be a repository root." >&2; exit 1; }
[ "$(git -C "$PUBLISH_CHECKOUT" branch --show-current)" = main ] ||
  { echo "Publishing checkout must be on main." >&2; exit 1; }
[ ! -L "$PUBLISH_CHECKOUT/site" ] ||
  { echo "Refusing a symlinked publication directory." >&2; exit 1; }
mkdir -p "$WORK_ROOT"
WORK_ROOT=$(CDPATH= cd -- "$WORK_ROOT" && pwd -P)
case "$WORK_ROOT/" in
  "$PUBLISH_CHECKOUT/"*) echo "WORK_ROOT must be outside the checkout." >&2; exit 1 ;;
esac
lock="$WORK_ROOT/publish.lock"
if ! mkdir "$lock" 2>/dev/null; then
  echo "Cannot acquire $lock; another run may be active. Check before removing a stale lock." >&2
  exit 75
fi
STATIC_RUN_DIR=
finish() {
  result=$?
  trap - EXIT
  if [ "$result" -ne 0 ]; then
    echo "Publication failed (exit $result). Last deployed site is unchanged unless a push already succeeded." >&2
    [ -z "$STATIC_RUN_DIR" ] || echo "Inspect retained staging output: $STATIC_RUN_DIR" >&2
  fi
  rmdir "$lock" || { echo "Could not release $lock; inspect it before the next run." >&2; result=1; }
  exit "$result"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

[ -z "$(git -C "$PUBLISH_CHECKOUT" status --porcelain)" ] ||
  { echo "Publishing checkout is dirty; inspect it rather than discarding changes." >&2; exit 1; }
git -C "$PUBLISH_CHECKOUT" fetch origin main
git -C "$PUBLISH_CHECKOUT" merge --ff-only origin/main
[ "$(git -C "$PUBLISH_CHECKOUT" rev-list --count origin/main..HEAD)" = 0 ] ||
  { echo "Unpushed commits remain; recover the previous push before exporting again." >&2; exit 1; }
[ -f "$PUBLISH_CHECKOUT/scripts/validate.ts" ] ||
  { echo "Merge the reading-site infrastructure PR before publishing." >&2; exit 1; }

STATIC_RUN_DIR=$(mktemp -d "$WORK_ROOT/run.XXXXXX")
export STATIC_RUN_DIR
VALIDATION_PREVIOUS_SITE=
export VALIDATION_PREVIOUS_SITE
if [ -f "$PUBLISH_CHECKOUT/site/data/archive.json" ]; then
  VALIDATION_PREVIOUS_SITE=/publisher/site
  export VALIDATION_PREVIOUS_SITE
  "$DOCKER_BIN" compose -f "$COMPOSE_FILE" run --rm exporter \
    scholarly-rss-feed --export /work/site --site-url "$SITE_URL" --previous /previous/site
elif [ "${BOOTSTRAP_EXPORT:-0}" = 1 ] && [ ! -e "$PUBLISH_CHECKOUT/site" ]; then
  "$DOCKER_BIN" compose -f "$COMPOSE_FILE" run --rm exporter \
    scholarly-rss-feed --export /work/site --site-url "$SITE_URL"
else
  echo "Previous archive is missing. Restore it, or explicitly set BOOTSTRAP_EXPORT=1 for the first export." >&2
  exit 1
fi

"$DOCKER_BIN" compose -f "$COMPOSE_FILE" run --rm validator
mkdir -p "$PUBLISH_CHECKOUT/site"
rsync -a --delete "$STATIC_RUN_DIR/site/" "$PUBLISH_CHECKOUT/site/"
git -C "$PUBLISH_CHECKOUT" add -- site
if git -C "$PUBLISH_CHECKOUT" diff --cached --quiet; then
  echo "No public snapshot changes; nothing to publish."
else
  git -C "$PUBLISH_CHECKOUT" commit -m "Publish scholarly reading snapshot"
  revision=$(git -C "$PUBLISH_CHECKOUT" rev-parse HEAD)
  git -C "$PUBLISH_CHECKOUT" push origin HEAD:main
  echo "Pushed $revision; waiting for its publication workflow."
  run=
  attempt=0
  while [ "$attempt" -lt 12 ]; do
    run=$("$GH_BIN" run list --repo "$PUBLISH_REPOSITORY" --commit "$revision" \
      --workflow pages.yml --limit 1 --json databaseId --jq '.[0].databaseId // empty')
    [ -z "$run" ] || break
    attempt=$((attempt + 1))
    sleep 5
  done
  [ -n "$run" ] || { echo "No publication workflow appeared for $revision." >&2; exit 1; }
  "$GH_BIN" run watch "$run" --repo "$PUBLISH_REPOSITORY" --exit-status --interval 10
  conclusion=$("$GH_BIN" run view "$run" --repo "$PUBLISH_REPOSITORY" --json jobs \
    --jq '.jobs[] | select(.name == "deploy") | .conclusion')
  [ "$conclusion" = success ] ||
    { echo "Snapshot passed CI but was NOT deployed (deploy=$conclusion). Check Pages settings and PAGES_DEPLOY_ENABLED." >&2; exit 1; }
  echo "Snapshot $revision deployed successfully."
fi

# This directory was created by this run, outside the checkout, and contains only staging output.
find "$STATIC_RUN_DIR" -depth -delete
