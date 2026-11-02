#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Turn upstream's bundle .env.example into the ScyllaDB variant.

The bundle env documents every variable as a block of comment lines that ends
in the variable line (`NAME=` or `# NAME=`), blocks separated by blank lines,
sections introduced by a `# ====` banner. Postgres blocks are dropped, the
DATABASE section becomes the SCYLLADB section, and the remaining mentions are
edited by exact match so a wording change upstream fails loudly.
"""
import re
import sys

src, dst = sys.argv[1], sys.argv[2]
s = open(src).read()


def rep(old, new):
    global s
    if s.count(old) != 1:
        sys.exit(f"expected exactly one match for: {old!r} (got {s.count(old)})")
    s = s.replace(old, new)


BANNER = "# " + "=" * 76 + "\n"

# 1. Header
rep(
    """#  If you move this instance to rexctl later with rexctl takeover, the rexctl
#  line under each setting shows the command that changes it.
""" + BANNER,
    """#  This is the ScyllaDB variant of upstream's bundle. It runs the uwulc
#  images, which are built with the ScyllaDB backend and include fluxer-dev.
#  rexctl does not manage it: rexctl takeover expects the Postgres stack. The
#  rexctl lines under each setting only show what the setting means upstream.
""" + BANNER,
)

# 2. DATABASE section -> SCYLLADB section
m = re.search(
    re.escape(BANNER) + r"#  DATABASE\n#\n#  Leave these unset to use the bundled Postgres\.\n" + re.escape(BANNER)
    + r"(?:.*\n)*?(?=\n" + re.escape(BANNER) + ")",
    s,
)
if not m:
    sys.exit("DATABASE section not found")
body = m.group(0)[len(BANNER):]
if "Postgres" not in body or re.search(r"^#? ?(?!FLUXER_POSTGRES_)[A-Z_]+=", body, re.M):
    sys.exit("DATABASE section holds more than the Postgres settings:\n" + body)
s = s.replace(m.group(0), BANNER + """#  SCYLLADB
#
#  The bundled scylladb service holds all of Fluxer's data. cassandra-migrate
#  brings its schema up to date on every start. Leave the connection settings
#  unset to use it.
""" + BANNER + """
# Set these to use ScyllaDB outside the stack. Backing it up is then yours to
# arrange. The values below are examples. The bundled scylladb service still
# starts unless an override removes it.
# FLUXER_CASSANDRA_HOSTS=scylla1.example.com,scylla2.example.com
# FLUXER_CASSANDRA_PORT=9042
# FLUXER_CASSANDRA_KEYSPACE=fluxer
# FLUXER_CASSANDRA_LOCAL_DC=datacenter1
# FLUXER_CASSANDRA_USERNAME=
# FLUXER_CASSANDRA_PASSWORD=

# Bundled ScyllaDB sizing. FLUXER_SCYLLADB_MEMORY plus
# FLUXER_SCYLLADB_RESERVE_MEMORY must fit inside the memory limit, or ScyllaDB
# refuses to start (it reserves 1.5 GiB for the OS unless told otherwise).
# Developer mode skips the production I/O and kernel checks, which a small host
# fails. Raise the SMP count with the CPUs you give it.
# FLUXER_SCYLLADB_MEMORY_LIMIT=1536mb
# FLUXER_SCYLLADB_SMP=1
# FLUXER_SCYLLADB_MEMORY=750M
# FLUXER_SCYLLADB_RESERVE_MEMORY=512M
# FLUXER_SCYLLADB_DEVELOPER_MODE=1
# FLUXER_SCYLLADB_HEALTHCHECK_TIMEOUT=10s
# FLUXER_SCYLLADB_HEALTHCHECK_RETRIES=20
# FLUXER_SCYLLADB_HEALTHCHECK_START_PERIOD=60s
# FLUXER_CASSANDRA_MIGRATE_MEMORY_LIMIT=256mb

# The one-shot pg-migrate service, which moves an old Postgres-backed instance
# into ScyllaDB. Only `docker compose --profile migrate run --rm pg-migrate`
# reads these. See MIGRATION.md.
# FLUXER_MIGRATE_POSTGRES_HOST=postgres
# FLUXER_MIGRATE_POSTGRES_PORT=5432
# FLUXER_MIGRATE_POSTGRES_DATABASE=fluxer
# FLUXER_MIGRATE_POSTGRES_USERNAME=fluxer
# FLUXER_MIGRATE_POSTGRES_PASSWORD=
# FLUXER_MIGRATE_POSTGRES_KV_TABLE=fluxer_kv
# FLUXER_MIGRATE_TABLES=
# FLUXER_MIGRATE_DRY_RUN=false

""", 1)

# 3. Every other Postgres setting block (comment lines ending in the variable)
pg_block = re.compile(
    r"(?:^# (?![A-Z_]+=).*\n|^#\n)*^(?:# )?(?:POSTGRES_PASSWORD|FLUXER_(?!MIGRATE_)[A-Z_]*POSTGRES[A-Z_]*)=.*\n\n?",
    re.M,
)
s, n = pg_block.subn("", s)
if n < 30:
    sys.exit(f"only {n} Postgres setting blocks removed; did the format change?")

# 4. Images
rep(
    """#  Set one to use your own mirror or another version. Fluxer's own images are
#  pinned to the release in docker-compose.yml.
""" + BANNER,
    """#  Set one to use your own mirror or another version.
""" + BANNER + """
# Fluxer's own images. This stack needs the uwulc images: they are built with
# the ScyllaDB backend and include fluxer-dev, which upstream does not publish.
FLUXER_REGISTRY_OWNER=uwulc
FLUXER_REGISTRY=ghcr.io/${FLUXER_REGISTRY_OWNER}
FLUXER_IMAGE_TAG=latest
""",
)
# The postgres image block went with step 3; put the scylladb one in its place.
rep(
    """# The image the valkey container runs.
""",
    """# The image the scylladb container runs. Move ScyllaDB one major version at
# a time.
#   default:  scylladb/scylla:6.2
# FLUXER_SCYLLADB_IMAGE=

# The image the valkey container runs.
""",
)

# 5. Shared health check settings: scylladb has its own retries and start period
rep("mark the edge, postgres, valkey,\n", "mark the edge, valkey,\n")
rep("# How long the edge, postgres, valkey, nats,", "# How long the edge, valkey, nats,")

# 6. Nothing about Postgres may remain outside the pg-migrate settings
left = [l for l in s.splitlines() if re.search("postgres", l, re.I) and "MIGRATE" not in l
        and "Postgres-backed instance" not in l and "expects the Postgres stack" not in l]
if left:
    sys.exit("postgres left over:\n" + "\n".join(left))
open(dst, "w").write(s)
