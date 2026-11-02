#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

# Rebuild deploy/self-hosting-scylladb from upstream's rexctl compose bundle.
#
# Generates the bundle from fluxer_rexctl in the working tree (in a golang
# container, at the Go version go.mod asks for), applies the ScyllaDB edits
# (scylla_compose.py, scylla_env.py) and copies the result over the deploy dir.
# MIGRATION.md, .gitignore and the .gitkeep files are the deploy dir's own and
# are left alone. Both edit scripts stop when a pattern no longer matches;
# adjust them, never the output.
# Requires: docker, python3.
#
# Usage:
#   ./scripts/fork/scylla-bundle/rebuild.sh [--check] [--keep]
#
# Options:
#   --check   Only compare the result with the deploy dir; exit 1 if it differs
#   --keep    Keep the scratch folder (bundle/ and new/) and print its path

CHECK=false
KEEP=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --check) CHECK=true; shift ;;
    --keep)  KEEP=true;  shift ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../.." && pwd)"
DEPLOY="$REPO_ROOT/deploy/self-hosting-scylladb"
OWN=(-x .gitkeep -x .gitignore -x MIGRATION.md)

WORK="$(mktemp -d)"
if "$KEEP"; then
  echo "Scratch folder: $WORK"
else
  trap 'rm -rf "$WORK"' EXIT
fi
mkdir -p "$WORK/rex" "$WORK/bundle" "$WORK/gocache"

# 1. Bundle
cp -a "$REPO_ROOT/fluxer_rexctl/." "$WORK/rex/"
cp "$HERE/bundle_test.go.in" "$WORK/rex/internal/instance/zz_write_bundle_test.go"
GO_VERSION="$(awk '$1 == "go" { split($2, v, "."); print v[1] "." v[2] }' "$WORK/rex/go.mod")"
if ! docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -e GOFLAGS=-modcacherw \
  -e GOCACHE=/cache/build -e GOMODCACHE=/cache/mod -e BUNDLE_OUT=/out \
  -v "$WORK/rex:/src" -v "$WORK/bundle:/out" -v "$WORK/gocache:/cache" -w /src \
  "golang:$GO_VERSION" go test ./internal/instance -run '^TestZZWriteBundle$' -count=1 \
  >"$WORK/go.log" 2>&1 </dev/null; then
  cat "$WORK/go.log" >&2
  echo "Generating the bundle failed." >&2
  exit 1
fi

# 2. ScyllaDB edits
cp -a "$WORK/bundle" "$WORK/new"
python3 "$HERE/scylla_compose.py" "$WORK/new/docker-compose.yml"
python3 "$HERE/scylla_env.py" "$WORK/bundle/.env.example" "$WORK/new/.env.example"

# 3. Compare or apply
if "$CHECK"; then
  if diff -r "${OWN[@]}" "$WORK/new" "$DEPLOY"; then
    echo "deploy/self-hosting-scylladb is up to date."
  else
    exit 1
  fi
  exit 0
fi

cp -a "$WORK/new/." "$DEPLOY/"
# Keep the bundle's empty folders in git.
find "$DEPLOY" -type d -empty -exec touch {}/.gitkeep \;
# Files the bundle no longer ships.
stale="$(diff -rq "${OWN[@]}" "$WORK/new" "$DEPLOY" | sed -n "s|^Only in $DEPLOY|deploy/self-hosting-scylladb|p" || true)"
if [[ -n "$stale" ]]; then
  echo "Not in the bundle any more (remove them if upstream dropped them):"
  echo "$stale"
fi
echo "Rebuilt deploy/self-hosting-scylladb. Review with git diff."
