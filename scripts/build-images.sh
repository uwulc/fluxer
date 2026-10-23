#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

# Build and push all Fluxer Docker images to ghcr.io/uwulc.
# The list matches the images deploy/self-hosting-scylladb/docker-compose.yml
# pulls (api and worker share fluxer-api).
# Requires: docker with buildx, `docker login ghcr.io` already done.
#
# Usage:
#   ./scripts/build-images.sh [OPTIONS]
#
# Options:
#   --tag TAG          Image tag (default: latest)
#   --version VER      BUILD_VERSION baked into every image (default: a fresh
#                      UTC CalVer, YYYY.MDD.HHMMSS without leading zeroes, like
#                      upstream's builds). Web clients offer an update when it
#                      changes, so keep it unique per build.
#   --platform PLAT    Build platform, e.g. linux/amd64,linux/arm64 (default: native)
#   --only NAME,...    Comma-separated list of image names to build (e.g. fluxer-api,fluxer-gateway)
#   --no-push          Build only, do not push
#   --dry-run          Print commands without running them

REGISTRY="ghcr.io/uwulc"
TAG="latest"
BUILD_VERSION="$(date -u +%Y.%-m%d).$((10#$(date -u +%H%M%S)))"
PLATFORM=""
ONLY=""
PUSH=true
DRY_RUN=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)        TAG="$2";      shift 2 ;;
    --version)    BUILD_VERSION="$2"; shift 2 ;;
    --platform)   PLATFORM="$2"; shift 2 ;;
    --only)       ONLY="$2";     shift 2 ;;
    --no-push)    PUSH=false;    shift   ;;
    --dry-run)    DRY_RUN=true;  shift   ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# image-name:dockerfile[:extra_build_args...]
# Extra build args use KEY=VALUE pairs separated by spaces (no colons).
IMAGES=(
  "fluxer-api:fluxer_api/Dockerfile"
  "fluxer-gateway:fluxer_gateway/Dockerfile"
  "fluxer-messages:fluxer_messages/Dockerfile"
  "fluxer-users:fluxer_users/Dockerfile"
  "fluxer-snowflakes:fluxer_snowflakes/Dockerfile"
  "fluxer-gifs:fluxer_gifs/Dockerfile"
  "fluxer-media-proxy:fluxer_media_proxy/Dockerfile"
  "fluxer-push:fluxer_push/Dockerfile"
  "fluxer-unfurl:fluxer_unfurl/Dockerfile"
  "fluxer-static:fluxer_static/Dockerfile"
  # Self-hosted variant of app-proxy, built the way upstream's
  # build-app-proxy-self-hosted workflow does: assets bundled in the image and
  # served same-origin, FLUXER_SELF_HOSTED baked in. The regular
  # fluxer-app-proxy points at fluxerstatic.com; use this for self-hosting.
  "fluxer-app-proxy-self-hosted:fluxer_app_proxy/Dockerfile:FLUXER_SELF_HOSTED=true PUBLIC_ASSET_BASE_URL= BUNDLE_LOCAL_ASSETS=true"
  "fluxer-admin:fluxer_admin/Dockerfile"
  # Fork-only tool image: runs `cassandra apply` (cassandra-migrate) at stack
  # startup and the one-shot `migrate postgres-to-scylla` (pg-migrate).
  "fluxer-dev:tools/dev/Dockerfile"
)

should_build() {
  local name="$1"
  [[ -z "$ONLY" ]] && return 0
  IFS=',' read -ra FILTER <<< "$ONLY"
  for f in "${FILTER[@]}"; do
    [[ "$f" == "$name" ]] && return 0
  done
  return 1
}

run() {
  if "$DRY_RUN"; then
    echo "[dry-run]" "$@"
  else
    "$@"
  fi
}

cd "$REPO_ROOT"

# OCI revision/created labels, as upstream's image workflows pass them.
SOURCE_SHA="$(git rev-parse HEAD)"
SOURCE_DATE="$(TZ=UTC git log -1 --no-show-signature --pretty=%cd --date=format-local:%Y-%m-%dT%H:%M:%SZ)"

built=()
skipped=()

for entry in "${IMAGES[@]}"; do
  IFS=':' read -r name dockerfile extra_args <<< "$entry"

  if ! should_build "$name"; then
    skipped+=("$name")
    continue
  fi

  echo ""
  echo "==> Building $name"

  cmd=(docker buildx build)
  cmd+=(--file "$dockerfile")
  cmd+=(--tag "$REGISTRY/$name:$TAG")
  cmd+=(--build-arg "BUILD_VERSION=$BUILD_VERSION")
  cmd+=(--build-arg "SOURCE_SHA=$SOURCE_SHA")
  cmd+=(--build-arg "SOURCE_DATE=$SOURCE_DATE")

  if [[ -n "${extra_args:-}" ]]; then
    IFS=' ' read -ra args <<< "$extra_args"
    for arg in "${args[@]}"; do
      cmd+=(--build-arg "$arg")
    done
  fi

  if [[ -n "$PLATFORM" ]]; then
    cmd+=(--platform "$PLATFORM")
  fi

  if "$PUSH"; then
    cmd+=(--push)
  else
    cmd+=(--load)
  fi

  cmd+=(.)

  run "${cmd[@]}"
  built+=("$REGISTRY/$name:$TAG")
done

echo ""
echo "Done (BUILD_VERSION=$BUILD_VERSION)."
if [[ ${#built[@]} -gt 0 ]]; then
  echo "Built:"
  for img in "${built[@]}"; do
    echo "  $img"
  done
fi
if [[ ${#skipped[@]} -gt 0 ]]; then
  echo "Skipped: ${skipped[*]}"
fi
