#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Turn upstream's bundle docker-compose.yml into the ScyllaDB variant."""
import re
import sys

path = sys.argv[1]
s = open(path).read()


def sub(pattern, repl, count=1, flags=0):
    global s
    new, n = re.subn(pattern, repl, s, count=0 if count == 0 else count, flags=flags)
    if n == 0 or (count and n != count):
        sys.exit(f"pattern matched {n}x (want {count or '>0'}): {pattern!r}")
    s = new


# 1. Postgres env anchor -> Cassandra env anchor
sub(
    r"x-fluxer-postgres-env: &fluxer-postgres-env\n(?:  FLUXER_[A-Z_]+: .*\n)+",
    """x-fluxer-cassandra-env: &fluxer-cassandra-env
  FLUXER_DATABASE_BACKEND: cassandra
  FLUXER_CASSANDRA_HOSTS: ${FLUXER_CASSANDRA_HOSTS:-scylladb}
  FLUXER_CASSANDRA_PORT: ${FLUXER_CASSANDRA_PORT:-9042}
  FLUXER_CASSANDRA_KEYSPACE: ${FLUXER_CASSANDRA_KEYSPACE:-fluxer}
  FLUXER_CASSANDRA_LOCAL_DC: ${FLUXER_CASSANDRA_LOCAL_DC:-datacenter1}
  FLUXER_CASSANDRA_USERNAME: ${FLUXER_CASSANDRA_USERNAME:-}
  FLUXER_CASSANDRA_PASSWORD: ${FLUXER_CASSANDRA_PASSWORD:-}
""",
)
sub(r"  <<: \*fluxer-postgres-env\n", "  <<: *fluxer-cassandra-env\n")

# 2. NO_PROXY names the database host
sub(r"(no_proxy: localhost,127\.0\.0\.1,::1,edge,)postgres,", r"\1scylladb,", flags=re.I, count=2)

# 3. postgres service -> scylladb + cassandra-migrate + pg-migrate
sub(
    r"  postgres:\n    <<: \*fluxer-service\n(?:    .*\n|      .*\n)+?      <<: \*fluxer-healthcheck\n",
    """  scylladb:
    <<: *fluxer-service
    image: ${FLUXER_SCYLLADB_IMAGE:-scylladb/scylla:6.2}
    deploy:
      resources:
        limits:
          memory: ${FLUXER_SCYLLADB_MEMORY_LIMIT:-1536mb}
    command:
      - --smp=${FLUXER_SCYLLADB_SMP:-1}
      - --memory=${FLUXER_SCYLLADB_MEMORY:-750M}
      - --reserve-memory=${FLUXER_SCYLLADB_RESERVE_MEMORY:-512M}
      - --overprovisioned=1
      - --developer-mode=${FLUXER_SCYLLADB_DEVELOPER_MODE:-1}
    volumes:
      - scylladb-data:/var/lib/scylla
    healthcheck:
      test: ["CMD-SHELL", "nodetool status 2>/dev/null | grep -qE '^UN'"]
      <<: *fluxer-healthcheck
      timeout: ${FLUXER_SCYLLADB_HEALTHCHECK_TIMEOUT:-10s}
      retries: ${FLUXER_SCYLLADB_HEALTHCHECK_RETRIES:-20}
      start_period: ${FLUXER_SCYLLADB_HEALTHCHECK_START_PERIOD:-60s}

  # Applies tools/dev/cassandra_target_schema.json with `fluxer-dev cassandra
  # apply` and exits. Every service that reads the database waits for it, so an
  # upgrade picks up new tables and columns before the services start.
  cassandra-migrate:
    image: ${FLUXER_REGISTRY:-ghcr.io/${FLUXER_REGISTRY_OWNER:-uwulc}}/fluxer-dev:${FLUXER_IMAGE_TAG:-latest}
    deploy:
      resources:
        limits:
          memory: ${FLUXER_CASSANDRA_MIGRATE_MEMORY_LIMIT:-256mb}
    networks: [fluxer]
    logging: *fluxer-logging
    restart: "no"
    command: ["cassandra", "apply"]
    environment:
      <<: *fluxer-cassandra-env
    # fluxer-dev writes its last schema diff under the repository root baked in
    # at compile time. The image runs as 65532, so the tmpfs must be its own.
    tmpfs:
      - /usr/src/app/.fluxer:uid=65532,gid=65532,mode=0755
    depends_on:
      scylladb: {condition: service_healthy}

  # One-shot Postgres to ScyllaDB data move. See MIGRATION.md. Run it while the
  # old stack's postgres is still up:
  #   docker compose --profile migrate run --rm pg-migrate
  # Both stacks share the fluxer project network, so "postgres" resolves while
  # the old container runs. FLUXER_MIGRATE_* in .env configure the source.
  pg-migrate:
    image: ${FLUXER_REGISTRY:-ghcr.io/${FLUXER_REGISTRY_OWNER:-uwulc}}/fluxer-dev:${FLUXER_IMAGE_TAG:-latest}
    networks: [fluxer]
    logging: *fluxer-logging
    restart: "no"
    profiles: [migrate]
    command: ["migrate", "postgres-to-scylla"]
    environment:
      <<: *fluxer-cassandra-env
      FLUXER_POSTGRES_HOST: ${FLUXER_MIGRATE_POSTGRES_HOST:-postgres}
      FLUXER_POSTGRES_PORT: ${FLUXER_MIGRATE_POSTGRES_PORT:-5432}
      FLUXER_POSTGRES_DATABASE: ${FLUXER_MIGRATE_POSTGRES_DATABASE:-fluxer}
      FLUXER_POSTGRES_USERNAME: ${FLUXER_MIGRATE_POSTGRES_USERNAME:-fluxer}
      FLUXER_POSTGRES_PASSWORD: ${FLUXER_MIGRATE_POSTGRES_PASSWORD:-}
      FLUXER_POSTGRES_KV_TABLE: ${FLUXER_MIGRATE_POSTGRES_KV_TABLE:-fluxer_kv}
      FLUXER_MIGRATE_TABLES: ${FLUXER_MIGRATE_TABLES:-}
      FLUXER_MIGRATE_DRY_RUN: ${FLUXER_MIGRATE_DRY_RUN:-false}
    tmpfs:
      - /usr/src/app/.fluxer:uid=65532,gid=65532,mode=0755
    depends_on:
      cassandra-migrate: {condition: service_completed_successfully}
""",
)

# 4. Postgres-only per-service settings
sub(r'      FLUXER_POSTGRES_MAX_CONNECTIONS: "\$\{FLUXER_[A-Z_]+_POSTGRES_MAX_CONNECTIONS:-\d+\}"\n', "", count=4)
sub(r'      FLUXER_WORKER_POSTGRES_SYNCHRONOUS_COMMIT: .*\n', "")

# 5. Database readers wait for the schema instead of postgres
sub(
    r"      postgres: \{condition: service_healthy\}\n",
    "      cassandra-migrate: {condition: service_completed_successfully}\n",
    count=4,
)

# 6. uwulc images
sub(
    r"\$\{FLUXER_REGISTRY:-ghcr\.io/\$\{FLUXER_REGISTRY_OWNER:-fluxerapp\}\}/(fluxer-[a-z-]+):\$\{FLUXER_IMAGE_TAG:-v1\}",
    r"${FLUXER_REGISTRY:-ghcr.io/${FLUXER_REGISTRY_OWNER:-uwulc}}/\1:${FLUXER_IMAGE_TAG:-latest}",
    count=0,
)

# 7. Volume
sub(r"\n  postgres-data:\n", "\n  scylladb-data:\n")

pre_migrate = s.split("  pg-migrate:")[0]
post_migrate = s.split("      cassandra-migrate: {condition: service_completed_successfully}\n", 2)[-1]
left = [l for l in (pre_migrate + post_migrate).splitlines() if "postgres" in l.lower() and not l.lstrip().startswith("#")]
if left:
    sys.exit("postgres left over:\n" + "\n".join(left))
open(path, "w").write(s)
