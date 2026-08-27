# SPDX-License-Identifier: AGPL-3.0-or-later

FROM rust:1-trixie AS builder

WORKDIR /usr/src/app

COPY . .

RUN cargo build --release -p fluxer-dev

FROM debian:trixie-slim

ARG BUILD_VERSION=""
ARG SOURCE_SHA=""
ARG SOURCE_DATE=""

LABEL org.opencontainers.image.title="fluxer-dev"
LABEL org.opencontainers.image.description="Fluxer developer tooling (Cassandra schema apply, Postgres to ScyllaDB migration)"
LABEL org.opencontainers.image.licenses="AGPL-3.0-or-later"
LABEL org.opencontainers.image.version="${BUILD_VERSION}"
LABEL org.opencontainers.image.revision="${SOURCE_SHA}"
LABEL org.opencontainers.image.created="${SOURCE_DATE}"
LABEL app.fluxer.build-version="${BUILD_VERSION}"

WORKDIR /usr/local/bin

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

# `cassandra apply` records its last diff under the repository root baked in at
# compile time (/usr/src/app/.fluxer/dev/cassandra), so give the unprivileged
# runtime user that directory.
RUN install -d -o 65532 -g 65532 /usr/src/app/.fluxer/dev/cassandra

COPY --from=builder /usr/src/app/target/release/fluxer-dev /usr/local/bin/fluxer-dev

ENV BUILD_VERSION="${BUILD_VERSION}"

USER 65532:65532

ENTRYPOINT ["/usr/local/bin/fluxer-dev"]
