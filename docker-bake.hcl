# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Fork image set: every image deploy/self-hosting-scylladb/docker-compose.yml
# pulls (api and worker share fluxer-api). Target names equal image names.
# Used by scripts/build-images.sh and .github/workflows/uwulc-images.yaml.
#
#   docker buildx bake --print            # resolved config
#   TAG=dev docker buildx bake --push     # build and push everything
#   docker buildx bake fluxer-api --load  # one image into the local daemon

variable "REGISTRY" {
  default = "ghcr.io/uwulc"
}

variable "TAG" {
  default = "latest"
}

# Callers always set these. BUILD_VERSION is a UTC CalVer (YYYY.MDD.HHMMSS
# without leading zeroes); web clients offer an update when it changes.
variable "BUILD_VERSION" {
  default = ""
}

variable "SOURCE_SHA" {
  default = ""
}

variable "SOURCE_DATE" {
  default = ""
}

function "image" {
  params = [name]
  result = ["${REGISTRY}/${name}:${TAG}"]
}

group "default" {
  targets = [
    "fluxer-api",
    "fluxer-gateway",
    "fluxer-messages",
    "fluxer-users",
    "fluxer-snowflakes",
    "fluxer-gifs",
    "fluxer-media-proxy",
    "fluxer-push",
    "fluxer-unfurl",
    "fluxer-static",
    "fluxer-app-proxy-self-hosted",
    "fluxer-admin",
    "fluxer-dev",
  ]
}

target "_common" {
  context = "."
  args = {
    BUILD_VERSION = BUILD_VERSION
    SOURCE_SHA    = SOURCE_SHA
    SOURCE_DATE   = SOURCE_DATE
  }
}

target "fluxer-api" {
  inherits   = ["_common"]
  dockerfile = "fluxer_api/Dockerfile"
  tags       = image("fluxer-api")
}

target "fluxer-gateway" {
  inherits   = ["_common"]
  dockerfile = "fluxer_gateway/Dockerfile"
  tags       = image("fluxer-gateway")
}

target "fluxer-messages" {
  inherits   = ["_common"]
  dockerfile = "fluxer_messages/Dockerfile"
  tags       = image("fluxer-messages")
}

target "fluxer-users" {
  inherits   = ["_common"]
  dockerfile = "fluxer_users/Dockerfile"
  tags       = image("fluxer-users")
}

target "fluxer-snowflakes" {
  inherits   = ["_common"]
  dockerfile = "fluxer_snowflakes/Dockerfile"
  tags       = image("fluxer-snowflakes")
}

target "fluxer-gifs" {
  inherits   = ["_common"]
  dockerfile = "fluxer_gifs/Dockerfile"
  tags       = image("fluxer-gifs")
}

target "fluxer-media-proxy" {
  inherits   = ["_common"]
  dockerfile = "fluxer_media_proxy/Dockerfile"
  tags       = image("fluxer-media-proxy")
}

target "fluxer-push" {
  inherits   = ["_common"]
  dockerfile = "fluxer_push/Dockerfile"
  tags       = image("fluxer-push")
}

target "fluxer-unfurl" {
  inherits   = ["_common"]
  dockerfile = "fluxer_unfurl/Dockerfile"
  tags       = image("fluxer-unfurl")
}

target "fluxer-static" {
  inherits   = ["_common"]
  dockerfile = "fluxer_static/Dockerfile"
  tags       = image("fluxer-static")
}

# Self-hosted variant of app-proxy: assets bundled in the image and served
# same-origin, FLUXER_SELF_HOSTED baked in. The regular fluxer-app-proxy points
# at fluxerstatic.com; use this for self-hosting.
target "fluxer-app-proxy-self-hosted" {
  inherits   = ["_common"]
  dockerfile = "fluxer_app_proxy/Dockerfile"
  tags       = image("fluxer-app-proxy-self-hosted")
  args = {
    FLUXER_SELF_HOSTED    = "true"
    PUBLIC_ASSET_BASE_URL = ""
    BUNDLE_LOCAL_ASSETS   = "true"
  }
}

target "fluxer-admin" {
  inherits   = ["_common"]
  dockerfile = "fluxer_admin/Dockerfile"
  tags       = image("fluxer-admin")
}

# Fork-only tool image: runs `cassandra apply` (cassandra-migrate) at stack
# startup and the one-shot `migrate postgres-to-scylla` (pg-migrate).
target "fluxer-dev" {
  inherits   = ["_common"]
  dockerfile = "tools/dev/Dockerfile"
  tags       = image("fluxer-dev")
}
