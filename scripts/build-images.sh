#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

# Build and push the fork's Fluxer Docker images to ghcr.io/uwulc.
# The image list and build args live in docker-bake.hcl, which CI
# (.github/workflows/uwulc-images.yaml) builds from too.
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
#   --dry-run          Print the command and the resolved bake config without building

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
cd "$REPO_ROOT"

# OCI revision/created labels, as upstream's image workflows pass them.
export TAG BUILD_VERSION
SOURCE_SHA="$(git rev-parse HEAD)"
SOURCE_DATE="$(TZ=UTC git log -1 --no-show-signature --pretty=%cd --date=format-local:%Y-%m-%dT%H:%M:%SZ)"
export SOURCE_SHA SOURCE_DATE

cmd=(docker buildx bake --file docker-bake.hcl)

if [[ -n "$PLATFORM" ]]; then
  cmd+=(--set "*.platform=$PLATFORM")
fi

if "$PUSH"; then
  cmd+=(--push)
else
  cmd+=(--load)
fi

if [[ -n "$ONLY" ]]; then
  IFS=',' read -ra targets <<< "$ONLY"
  cmd+=("${targets[@]}")
fi

if "$DRY_RUN"; then
  echo "[dry-run] TAG=$TAG BUILD_VERSION=$BUILD_VERSION SOURCE_SHA=$SOURCE_SHA SOURCE_DATE=$SOURCE_DATE" "${cmd[@]}"
  "${cmd[@]}" --print
  exit 0
fi

"${cmd[@]}"

echo ""
echo "Done (TAG=$TAG BUILD_VERSION=$BUILD_VERSION)."
