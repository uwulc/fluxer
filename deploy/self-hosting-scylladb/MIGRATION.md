# Migrating from Postgres to ScyllaDB

This guide covers switching an existing Postgres-backed installation of
upstream's Docker Compose bundle to this directory (ScyllaDB backend), and what
to expect when upgrading an existing `self-hosting-scylladb` installation.

Upstream no longer keeps a compose stack in the repository. It ships one inside
rexctl and as a compose bundle with each release. This directory is built from
that bundle (`fluxer_rexctl/internal/stack/files/` plus the generated
`.env.example`). A rexctl-managed instance has to be released with
`rexctl eject` first, which turns its folder into a plain compose stack.

The examples call the old stack's folder `old-stack` and this directory
`self-hosting-scylladb`.

## Overview

The Postgres backend stores all application data in a single `fluxer_kv` table as
tagged JSONB blobs. `fluxer-dev migrate postgres-to-scylla` reads every row from
that table, decodes the type wrappers, and inserts the data into the correct ScyllaDB
tables using `cassandra_target_schema.json` as the type map. Rows that carry an
expiry in Postgres keep their remaining lifetime as a ScyllaDB TTL.

Both stacks use `name: fluxer` in their compose files, which means they share the
same Docker project name and network (`fluxer_fluxer`). This is intentional: during
the migration window the Postgres container stays reachable by its `postgres` hostname
while ScyllaDB starts up alongside it. If the old `.env` sets
`COMPOSE_PROJECT_NAME` (`rexctl eject` writes it), set the same value in the new
`.env`; the names below then start with that value instead of `fluxer`.

Volumes for SeaweedFS, NATS, Meilisearch, and Valkey are also project-scoped, so
they carry over automatically — no extra steps needed for those services.

The fluxer images in this stack default to `ghcr.io/uwulc/*:latest`. The
`fluxer-dev` image that runs `cassandra-migrate` and `pg-migrate` exists only
there; upstream does not publish it.

## Prerequisites

- The old stack is running and healthy, and runs the same
  Fluxer version as the images this stack will use. Upgrade it first if it is
  older, so the Postgres data matches the schema being migrated into: point
  the old stack at the same `ghcr.io/uwulc` images (`FLUXER_REGISTRY_OWNER=uwulc`)
  and `docker compose up -d` it. Those images run the Postgres backend as well.
- You know the Postgres password (`POSTGRES_PASSWORD` from the old `.env`).
- The new `deploy/self-hosting-scylladb/.env` file is prepared (see next section).

## 1. Prepare the new `.env`

Copy your existing `.env` from the old stack's folder and update it:

```
# Remove — ScyllaDB needs no Postgres password, and the stack no longer reads it:
# POSTGRES_PASSWORD=...

# Keep — docker-compose.yml refuses to start without it:
FLUXER_SETTINGS_LOADED=true

# Add — used only during the one-shot migration:
FLUXER_MIGRATE_POSTGRES_PASSWORD=<value of POSTGRES_PASSWORD from the old .env>

# Point the images at the uwulc builds:
FLUXER_REGISTRY_OWNER=uwulc
FLUXER_REGISTRY=ghcr.io/${FLUXER_REGISTRY_OWNER}
FLUXER_IMAGE_TAG=latest

# All other variables are the same as the old stack.
# Every key listed below must be present:
FLUXER_DOMAIN=
FLUXER_SUDO_MODE_SECRET=
FLUXER_CONNECTION_INITIATION_SECRET=
FLUXER_PROFILE_PSEUDONYM_SECRET=
FLUXER_VAPID_PUBLIC_KEY=
FLUXER_VAPID_PRIVATE_KEY=
FLUXER_GATEWAY_RPC_AUTH_TOKEN=
FLUXER_ERLANG_COOKIE=
FLUXER_MEDIA_PROXY_SECRET_KEY=
FLUXER_MEDIA_PROXY_UPLOAD_RELAY_SECRET_BASE64=
FLUXER_ADMIN_SECRET_KEY_BASE=
FLUXER_ADMIN_OAUTH_CLIENT_SECRET=
FLUXER_S3_ACCESS_KEY=
FLUXER_S3_SECRET_KEY=
LIVEKIT_API_KEY=
LIVEKIT_API_SECRET=
MEILI_MASTER_KEY=
```

Any `FLUXER_POSTGRES_*` settings from the old `.env` (external database, pool
sizes, tuning) have no effect here. The ScyllaDB equivalents are the
`FLUXER_CASSANDRA_*` and `FLUXER_SCYLLADB_*` lines in `.env.example`.

## 2. Stop writes

Stop every service on the old stack that writes to Postgres so no new data
lands there while the migration runs. Postgres itself, NATS and SeaweedFS keep
running.

```bash
cd old-stack
docker compose stop app-proxy edge admin push worker api gateway media-proxy \
  messages users
```

Older stacks also run `messages-shard` and `users-shard`; stop those too.

## 3. Start ScyllaDB and apply the schema

Switch to the new stack and bring up ScyllaDB. The `cassandra-migrate` service
runs `fluxer-dev cassandra apply` and exits when the schema is ready.

```bash
cd self-hosting-scylladb
docker compose up -d scylladb cassandra-migrate
docker compose wait cassandra-migrate   # blocks until schema is applied
```

Compose warns about orphan containers (`fluxer-postgres-1`). This is expected,
since both directories share the `fluxer` project. Do not pass
`--remove-orphans`, which would delete the postgres container.

## 4. Run the data migration

```bash
FLUXER_MIGRATE_POSTGRES_PASSWORD=<your postgres password> \
  docker compose --profile migrate run --rm pg-migrate
```

Or set `FLUXER_MIGRATE_POSTGRES_PASSWORD` in `.env` beforehand.

Progress is printed per-table. A summary line reports total rows and any errors.
The command exits non-zero if any rows failed. Re-running it is safe until the
new stack starts taking writes (step 6): every row is written under the same
primary key again. After that, a re-run would overwrite newer ScyllaDB rows with
older Postgres values and bring back rows deleted since.

### Optional: dry run

Decode and convert every row without preparing or executing any writes, so it
catches bad data but not CQL errors. The summary still reports the rows as
migrated. `FLUXER_MIGRATE_DRY_RUN` accepts `true`/`1`/`yes`/`on` and
`false`/`0`/`no`/`off` (any case); any other value is an error.

```bash
FLUXER_MIGRATE_DRY_RUN=true \
  docker compose --profile migrate run --rm pg-migrate
```

### Optional: migrate a subset of tables

`FLUXER_MIGRATE_TABLES` is an allowlist. Names that are not in the target
schema are reported and skipped.

```bash
FLUXER_MIGRATE_TABLES=users,messages \
  docker compose --profile migrate run --rm pg-migrate
```

## 5. Tear down the old stack

Once migration reports success, remove the old stack's containers. Keep the
`postgres-data` volume (the default) or a dump until the new stack has run for a
while, as it is the only way back.

```bash
cd old-stack
docker compose down        # stops and removes containers; volumes stay by default
# To also remove the postgres volume (irreversible):
# docker compose down -v
```

Compose reports `Network fluxer_fluxer Resource is still in use`, because
ScyllaDB is still attached to it. This is harmless.

## 6. Bring up the new stack

```bash
cd self-hosting-scylladb
docker compose up -d
```

Every service that reads the database depends on `cassandra-migrate`, which
already completed in step 3, so startup should be fast.

Search needs no reindex: the Meilisearch volume carries over. Sessions, admin
accounts and setup state carry over with the data, so no new setup token is
issued.

## Rolling back

Both directories share the `fluxer` project, so most services (api, valkey,
NATS, SeaweedFS, ...) are the same containers in both. Never run
`docker compose down` in `self-hosting-scylladb` while rolling back or
retrying: it would also remove the old stack's containers.

To retry, remove only the ScyllaDB containers and data, then re-run from step 3:

```bash
cd self-hosting-scylladb
docker compose rm -sf scylladb cassandra-migrate
docker volume rm fluxer_scylladb-data
```

To roll back fully before step 5, the old stack is still intact:

```bash
cd old-stack
docker compose up -d --remove-orphans
```

Once step 5 (`docker compose down`) has been run, rollback requires restoring the
`postgres-data` volume from a backup.

## Upgrading an existing ScyllaDB stack

`cassandra-migrate` runs on every `docker compose up` and brings the keyspace
up to the schema of the `fluxer-dev` image: it creates new tables, types and
indexes, adds new columns and fields, and sets each table's
`default_time_to_live` to the target. It never drops anything, so tables and
columns that newer releases removed stay behind and are reported as warnings.
Other table options (compaction, `gc_grace_seconds`) are not changed on
existing tables.

Changes that need attention when an existing stack moves past them:

- **Compose bundle.** `.env` needs `FLUXER_SETTINGS_LOADED=true`, or
  compose refuses to start. The `users`, `messages`, `snowflakes`, `gifs` and
  `unfurl` services each run as one combined container, so the `*-shard`
  services are gone: run `docker compose up -d --remove-orphans` once to remove
  their containers, which otherwise keep serving shard 0 next to the combined
  ones (only on an upgrade, never during the Postgres migration).
  The edge also mounts `./certs` and `./caddy.d` (extra `*.caddy` snippets),
  and media-proxy gets a `media-proxy-tmp` volume. An override that names a
  `*-shard` service has to be updated alongside.
- **Edge rename.** Upstream renamed the `caddy` service to `edge`, with new
  `edge-data` and `edge-config` volumes; the edge requests its TLS certificate
  again on first start. LiveKit is configured inline instead of through
  `livekit.yaml`. An override that names the `caddy` or `postgres` services,
  or mounts `livekit.yaml`, has to be updated alongside.
- **Private CA, edge sites and standalone voice.** `api` and `worker` mount
  `./ca` read-only (CA certificates to trust for SMTP), and the edge also loads
  whole site blocks from `caddy.d/sites/*.caddy`. Create both folders before
  the first `up`, or Docker creates them owned by root. `voice/` and
  `external-voice.compose.yml` run LiveKit on another machine and are unused
  otherwise. `voice_servers` gains nullable columns (`draining`, pinning,
  public IPv4/IPv6, an internal endpoint); existing rows read as not draining
  and unpinned.

## Environment variable reference

All `FLUXER_MIGRATE_*` variables are passed to `pg-migrate` via the compose file and
can be overridden on the command line or set in `.env`.

| Variable | Default | Description |
|---|---|---|
| `FLUXER_MIGRATE_POSTGRES_HOST` | `postgres` | Hostname of the source Postgres instance |
| `FLUXER_MIGRATE_POSTGRES_PORT` | `5432` | Postgres port |
| `FLUXER_MIGRATE_POSTGRES_DATABASE` | `fluxer` | Postgres database name |
| `FLUXER_MIGRATE_POSTGRES_USERNAME` | `fluxer` | Postgres username |
| `FLUXER_MIGRATE_POSTGRES_PASSWORD` | *(empty)* | Postgres password — must be set when running the migration |
| `FLUXER_MIGRATE_POSTGRES_KV_TABLE` | `fluxer_kv` | Name of the KV table |
| `FLUXER_MIGRATE_TABLES` | *(all tables)* | Comma-separated table allowlist |
| `FLUXER_MIGRATE_DRY_RUN` | `false` | `true` to skip writes (see "Optional: dry run") |

The tool also reads the standard `FLUXER_CASSANDRA_*` variables (hosts, port,
keyspace, local_dc, username, password), which the compose file sets for every
service from the same `.env` lines.
