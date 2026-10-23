# Rebasing the fork onto upstream

How to move the uwu.lc fork's commits onto a newer upstream `main` and prove
the result works with real data before it reaches production. Written after the
2026-10-23 rebase (1257 upstream commits, tag `uwulc-20261023031400`) and
updated during the 2026-10-27 one, when upstream moved its compose stack into
rexctl, and during the 2026-11-02 one, when CI took over the image builds.
Commands reflect the stack as of that rebase. Expect env var and service names to drift,
and check them against `deploy/self-hosting-scylladb/` each time.

## Principles

- **The latest `uwulc-*` tag is the source of truth.** It is what production
  runs. Start from it, not from any earlier partial rebase. When `main` carries
  fork commits newer than the tag (CI, docs), start from `main` instead, after
  folding its `fixup!`/`amend!` commits in.
- **One commit per fork patch.** Each fork commit stays a single commit focused
  on its task. Fold fixes in with `fixup!` commits and `--autosquash`. Do not
  add "fix the rebase" commits on top.
- **Keep the intent, not just a clean apply.** A commit that applies without
  conflicts can still be wrong against changed upstream code. Some fork commits
  only matter on an *upgrade* path that a fresh install never exercises.
- **The production deploy is a separate step.** This runbook ends at a pushed
  `main`, a `uwulc-*` tag ready to push, and a list of what the production
  override and `.env` need. Pushing the tag is the release.

## Where to run things

Images come from CI (see [CI images](#ci-images)): push `main` untagged, and
test stacks follow `:dev` with `docker compose pull && docker compose up -d`.
Run the test stacks, and anything heavy such as `cargo` checks, on a machine
with plenty of disk and inodes. The examples call that machine `$HOST` and
reach it with `ssh "$HOST"` or `docker --context "$HOST"`. Test stacks use
compose bridge networks and their own project names, so they never collide
with other workloads on the host. `build-images.sh --no-push` still builds the
set locally when CI is unavailable.

Directories on the test host: `~/fluxer-src/<name>` holds source trees,
`~/fluxer-test` holds the stack that gets upgraded, and `~/fluxer-pgtest` holds
the postgres-to-scylla test.

## CI images

`.github/workflows/uwulc-images.yaml` builds the `docker-bake.hcl` image set,
the same set `build-images.sh` builds:

- **Push to `main`:** every image as `:sha-<commit>`, then `:dev` once the whole
  set built. Test stacks run `FLUXER_IMAGE_TAG=dev`.
- **Push of a `uwulc-*` tag:** `:<tag>` and `:latest` point at the commit's
  `:sha-<commit>` images, so production runs the exact digests that were
  tested. The workflow builds them first only if they are missing, so push
  `main` and wait for its run before pushing the tag. Production follows
  `:latest`, so pushing the tag *is* the production release. `:<tag>` stays
  around for rollback.
- **Manual run on another branch:** `:sha-<commit>` only. Use it to get
  registry images for a rebase branch before it lands.

Builds run on a self-hosted runner labelled `uwulc-builder` that reaches
BuildKit at `unix:///run/buildkit/buildkitd.sock`. A manual run with `hosted`
checked builds on a GitHub-hosted runner instead, with a cold cache.

Upstream's workflows stay in the tree but are disabled on the fork, since they
need upstream's GitHub App, runners and environments. Rebases bring in new
ones enabled, so after every push of a rebased `main`:

```bash
gh -R uwulc/fluxer workflow list --all          # anything active besides uwulc images?
gh -R uwulc/fluxer workflow disable "<name>"
```

Always pass `-R uwulc/fluxer`. With both `origin` and `upstream` remotes and no
`gh repo set-default`, `gh` resolves to upstream's repository, whose workflows
and runs look just like the fork's.

## Phase 0: refs

```bash
git fetch upstream
OLD=$(git tag -l 'uwulc-*' --sort=-creatordate | head -1)   # check: prod runs this
BASE=$(git merge-base "$OLD" upstream/main)
git log --oneline "$BASE..$OLD"                             # the fork commits
git rev-list --count "$BASE..upstream/main"                 # size of the jump
```

- **Use the real merge-base.** It is the upstream commit directly below the fork
  commits. `origin/main` lags behind: rebasing from it once would have replayed
  about 480 upstream commits.
- **Save whatever `main` points at** before moving it, for example
  `git tag fork-main-pre-rebase-<yyyymmdd> main`, unless it already equals
  `$OLD`. Keep such tags out of the `uwulc-*` namespace, which `$OLD` and CI
  treat as releases.
- **Fold pending `fixup!`/`amend!` commits first**, on the old base, so the
  rebase moves only the final fork commits:
  `GIT_SEQUENCE_EDITOR=true git rebase -i --autosquash "$BASE"`. The tree must
  not change.
- **Work in a worktree:**
  `git worktree add -b rebase/upstream-<yyyymmdd> ../fluxer-rebase "$OLD"`.
- **Check rerere.** It is enabled for this repo with `rerere.autoupdate=false`.
  It replays earlier conflict resolutions, which helps with repeated rebases,
  but it once replayed a *bad* resolution silently. Review every file it reports
  as "Resolved using previous resolution". After a failed or untrustworthy
  attempt, clear `.git/rr-cache`.

## Phase 1: baseline stack on the test host

Phases 1 and 2 can run in parallel.

**Reusing the previous cycle's stack.** If `~/fluxer-test` still runs images
built from a commit whose diff to `$OLD` touches nothing that goes into an
image (check with `git diff --stat <built-from> "$OLD"`), it already is the
baseline. Tag the images it runs (`:dev` from CI) as `pre-rebase`, set
`FLUXER_IMAGE_TAG=pre-rebase` in its `.env` so the Phase 3 pull can't change
it, move the previous cycle's `verify/`, `snapshots/` and notes into an
`archive-<date>/` folder, and continue at step 5. Its data was written by
older releases and has already been through one upgrade, which is a useful
extra check.

1. **Ship the old tag's source as a git checkout.** `build-images.sh` calls
   `git rev-parse`, so an archive won't do. A thin bundle plus a shallow fetch
   of its base is small:

   ```bash
   git bundle create /tmp/old.bundle "$BASE..$OLD" "$OLD"
   scp /tmp/old.bundle "$HOST":fluxer-src/
   ssh "$HOST" bash -s <<EOF
   git init -q ~/fluxer-src/baseline && cd ~/fluxer-src/baseline
   git fetch -q --depth 1 https://github.com/fluxerapp/fluxer $(git rev-parse "$BASE")
   git fetch -q ../old.bundle "refs/tags/$OLD:refs/tags/$OLD" && git checkout -q "$OLD"
   EOF
   ```

   GitHub only serves fetch-by-SHA for the **full** 40-character SHA.
2. **Build the old images under their own tag:**
   `./scripts/build-images.sh --no-push --tag pre-rebase`. The rebased images
   will be `latest` on the same host, and the old ones must survive alongside
   them. If the old tree's script lists an image the tree can't build, use
   `--only`. A full build takes about 15 to 30 minutes, so run it with `nohup`
   and write a log.
3. **Create `~/fluxer-test`** from the old tag's `deploy/self-hosting-scylladb/`.
   Generate fresh secrets from `.env.example`. **Never copy production's
   `.env`.** Settings that make the web client usable through an SSH tunnel:

   | Setting | Why |
   |---|---|
   | `COMPOSE_PROJECT_NAME=fluxer-test` | Overrides `name: fluxer`, so volumes become `fluxer-test_*` |
   | `FLUXER_DOMAIN=localhost:8380`, `FLUXER_PUBLIC_SCHEME=http`, `FLUXER_PUBLIC_PORT=80` | URLs come out as `http://localhost:8380/...` |
   | `FLUXER_PUBLIC_ORIGIN=http://localhost:8380` | Newer stacks append `:$PUBLIC_PORT` to the domain otherwise |
   | `FLUXER_EDGE_SITE_ADDRESS=:80` (`FLUXER_CADDY_SITE_ADDRESS` in older stacks) | Plain HTTP, no ACME |
   | `FLUXER_PASSKEY_RP_ID=localhost` | A valid RP ID has no port |
   | `FLUXER_LIVEKIT_TCP_PORT` / `_UDP_PORT` | Pick free ports |
   | `FLUXER_IMAGE_TAG=pre-rebase` | |
   | `FLUXER_SETTINGS_LOADED=true` | Stacks built from upstream's compose bundle refuse to start without it |

   In `docker-compose.override.yml`, publish only the edge (`caddy` in older
   stacks) on a free port: `ports: !override ["8380:80"]`.
   The client calls `crypto.randomUUID()`, which needs a secure context. Plain
   `http://<host>:8380` breaks it, so open the stack through
   `ssh -L 8380:localhost:8380 "$HOST"`.
4. **Bring it up and check it.** Run `docker compose up -d`, wait until every
   service is healthy, and check that `/api/_health` and the app load through
   the edge.
   **Don't create accounts by probing `POST /auth/register`**, even with an
   empty body: it creates real accounts.
5. **Pause and seed it by hand through the UI.** The first account with an
   email becomes admin. Good coverage includes:
   - messages with attachments
   - a guild with channels
   - DMs, reactions and an avatar
6. **Record an inventory** under `~/fluxer-test/verify/before/`:
   - row counts for every table in keyspace `fluxer`
   - dumps of the key entities, plus `DESCRIBE KEYSPACE fluxer`
   - S3 objects with sha256
   - Meilisearch stats
   - status and sha256 of a few attachments and avatars fetched through the
     public URLs
   - image IDs

## Phase 2: rebase

```bash
cd ../fluxer-rebase
git rebase --onto upstream/main "$BASE"
```

Resolve conflicts commit by commit, then **review every commit**, including
the ones that applied cleanly. If a fork commit is now covered by upstream,
drop it and explain why in the report. Checks per commit:

- **README / .gitignore.** Are the ignored paths still not provided by upstream?
- **`tools/dev` postgres-to-scylla migration.**
  - Take upstream's `Cargo.toml`/`Cargo.lock` and re-add only the fork's
    dependencies, at versions that match the workspace.
  - Coverage is schema-driven, so new tables are picked up automatically.
  - Re-check the KV encoding against upstream's `fluxer_svc` decode logic and
    `PostgresKvQueryExecutor.ts`. In particular, check the meaning of
    `expires_at`: `infinity` means `USING TTL 0`.
  - Run `cargo test`, `clippy -D warnings` and `fmt --check` for `fluxer-dev`.
- **`tools/dev` `cassandra apply`.** Upstream writes this against Cassandra.
  Check that every DDL statement it can emit is valid on **ScyllaDB**, for
  example that `ALTER ... ADD IF NOT EXISTS` does not reappear. The real test is
  applying it to a keyspace created by the *old* tag's schema; Phase 4 does
  that.
- **fluxer-dev Dockerfile.**
  - Compare its structure with upstream's current Rust Dockerfiles: base images,
    labels, user.
  - The image runs as uid 65532. Any compose tmpfs over `/usr/src/app/.fluxer`
    needs `uid=65532,gid=65532`.
- **Scylla features in other images.** Check that upstream still builds the
  database services with scylla:
  `git grep -n 'features scylla' upstream/main -- '*/Dockerfile'`.
  Look for services that newly gained or lost database access.
- **`deploy/self-hosting-scylladb`.**
  - Upstream's compose stack now lives in `fluxer_rexctl/internal/stack/files/`
    and ships per release as a compose bundle: those files, the empty folders
    the stack mounts (`certs/`, `ca/`, `caddy.d/sites/`), a `voice/` folder for
    standalone LiveKit servers, and `.env.example` files that rexctl generates.
    Deploy dirs must be self-contained: no references across dirs.
  - Rebuild the deploy dir with `scripts/fork/scylla-bundle/rebuild.sh`. It
    generates the bundle from the working tree's `fluxer_rexctl` (a Go test
    mirroring `cmd/bundle.go`, run in a `golang` container at the go.mod
    version), applies `scylla_compose.py` and `scylla_env.py`, and copies the
    result over the deploy dir, leaving `MIGRATION.md`, `.gitignore` and the
    `.gitkeep` files alone. `--keep` keeps the raw bundle for the diff below.
    `--check` only compares, and must pass on a finished branch.
  - Both edit scripts stop when a pattern no longer matches. Fix the script,
    never the output, so the next rebase can rerun it. The bundle's
    `.env.example` documents every variable as a comment block ending in the
    variable line; `scylla_env.py` drops the Postgres blocks and replaces the
    DATABASE section with the SCYLLADB one.
  - `diff -r <bundle> deploy/self-hosting-scylladb` must show only intentional
    differences. Merged, split or renamed services upstream need their
    `depends_on` edits carried over by hand.
  - Run `docker compose config -q` with `.env.example`, and once with each
    overlay file.
  - Upstream's generated `.env.example` covers only part of the compose
    variables. Check that every `${VAR}` the fork *adds* is declared:
    compare the variable sets of the bundle's and the fork's compose.
  - Update `MIGRATION.md`, including its "Upgrading an existing ScyllaDB stack"
    section.
- **`scripts/build-images.sh`.** The image list must equal the set of images
  the scylla compose references:

  ```bash
  grep -oE 'fluxer-[a-z-]+:\$\{FLUXER_IMAGE_TAG' deploy/self-hosting-scylladb/docker-compose.yml | sort -u
  ```

  Check the build args against upstream's Dockerfiles. The app-proxy
  self-hosted args in particular change.
- **Commit messages.** Keep the authorship and the original message. Extend the
  message when the content changes.
- **New commits.** New commits and commits rewritten substantially get a
  `Co-Authored-By` trailer for the assistant that wrote them.

## Phase 3: land `main` and let CI build

Once the Phase 2 review and checks pass, move `main` to the rebased branch and
push it untagged. Production follows `:latest`, which only a `uwulc-*` tag
moves, so this does not touch production.

```bash
cd <main checkout>
git status --short       # untracked files that the new tree tracks block the reset
git reset --keep rebase/upstream-<yyyymmdd>
git push --force-with-lease origin main
```

The `uwulc images` run builds `:sha-<commit>` and then moves `:dev`. Fixes
found later go in as `fixup!` commits, are autosquashed, and are pushed the
same way. Each push rebuilds `:dev`.

Stage the new stack files next to the test stack while CI runs (Phase 4,
step 2), with `FLUXER_IMAGE_TAG=dev`.

## Phase 4: upgrade the baseline and verify

1. **Snapshot.**
   - `docker compose stop` in `~/fluxer-test`.
   - Tar every `fluxer-test_*` volume using read-only mounts:

     ```bash
     for v in $(docker volume ls -q | grep '^fluxer-test_'); do
       docker run --rm -v "$v:/v:ro" -v ~/fluxer-test/snapshots/pre:/out alpine \
         tar -C /v -czf "/out/$v.tgz" .
     done
     ```

   - Copy the old stack files into `pre-rebase-stack/` and write the restore
     steps down. Rollback restores **all** volumes: Meilisearch and the
     schema upgrade in place.
2. **Switch.**
   - Replace the stack files with the rebased deploy dir. Keep the project name
     and the volumes.
   - Rebuild `.env` from the new `.env.example`, carrying over every secret.
     Generate any newly required secrets.
   - Port the override.
   - If upstream merged or removed services, add `--remove-orphans` to this one
     `up`. Old containers keep running otherwise. The `*-shard` containers,
     for example, kept serving shard 0 beside the combined services.
   - Write every non-stock change to `STACK-CHANGES.md`, marking the ones that
     also apply to production.
   - Once CI has moved `:dev` to the pushed commit (check the images'
     `org.opencontainers.image.revision` label), run
     `docker compose pull && docker compose up -d`.
3. **Watch the first start.**
   - `cassandra-migrate` must exit 0. Expect "Applying N statements", then
     "extra table" warnings for tables upstream removed.
   - The API must **not** generate a new setup token.
   - Every service must become healthy.
4. **Verify against the Phase 1 inventory.**
   - Every seeded row is present and unchanged. Explain every count difference:
     new tables, new null columns, new job or config rows.
   - The schema diff is additive.
   - S3 objects and the attachments served through the edge have the same
     sha256 as before.
   - Search counts are unchanged.
   - A **separate** test account can do all of these through the API: register
     (solve the Altcha captcha), log in, reach gateway READY, create a guild,
     send messages, upload and fetch an attachment, react, edit, delete, and
     search.
   - Every error in the logs since the switch is classified as either already
     present or new.
5. **Restart once more on the stock compose**, with the test workarounds
   removed. Any bug found here belongs in the fork commit that caused it.
6. **Leave the stack running** so someone can check the seeded data by hand in the UI.

## Phase 5: postgres-to-scylla test

Production no longer exercises this path, so test it separately.

- **Projects:** run two compose projects in `~/fluxer-pgtest`: `fluxer-pg` (a
  copy of upstream's compose bundle, built as in Phase 2, on the uwulc images)
  and `fluxer-sc` (the scylladb deploy dir).
- **Network:** attach `pg-migrate` to the postgres project's network so that
  `postgres` resolves. With distinct project names, copy the shared volumes
  (seaweedfs, meilisearch, nats, valkey, edge) across by hand.
- **Password:** give Postgres a password that contains spaces.
- **Open sign-ups first.** Newer releases keep registration closed after the
  setup-token admin signs up until setup is marked done and registration is
  open. Right after the admin registers, have the seed script send
  `PATCH /admin/instance/config` with
  `{app_public: {setup: {configured: true}}, registration: {mode: 'open'}}`
  using the admin's token.
- **Seed several accounts through the API**, covering:
  - avatars and settings
  - roles, permission overwrites and custom emoji
  - an expiring invite and a permanent one
  - attachments, edits, reactions, pins, replies and a deletion
  - DMs and group DMs
  - relationships
  - username changes, which produce `user_contact_change_logs` rows
- **Inventory after stopping writes.** Take the Postgres inventory only after
  step 2 of `MIGRATION.md`. The worker flushes buffered activity timestamps
  (`last_active_at`, `approx_last_used_at`) a few seconds later, and an earlier
  dump shows them as false differences.
- **Runs:** follow `MIGRATION.md` as written, then run `pg-migrate` four ways:
  1. a dry run
  2. a real run
  3. a second real run, which must change nothing
  4. a run with a bogus `FLUXER_MIGRATE_TABLES` name
- **Verify:**
  - Per-table counts and field-by-field equality. `__fluxer_schema_migrations`
    is Postgres-only and expected to be missing.
  - `TTL()` on expiring rows matches their remaining lifetime; permanent and
    `infinity` rows have none.
  - The contact-change-log timeuuids carry the original timestamps.
  - On the full scylla stack, every account works through the API: login with
    the old password, carried-over sessions, READY, history, files, invites,
    search and new writes.
- **Fixes:** validate any fix with a separately tagged image, for example
  `fluxer-dev:pgtest-fix`. **Never** retag `latest` or `pre-rebase` for this.
- **Docs:** record every place `MIGRATION.md` was wrong.

## Phase 6: tag

`main` is already pushed (Phase 3) and its last `uwulc images` run has
finished. Tag the verified commit:

```bash
# name: upstream base's commit date in its own timezone, e.g. uwulc-20261023031400
git tag "uwulc-$(git log -1 --format=%cd --date=format:%Y%m%d%H%M%S upstream/main)" main
```

- Tags are lightweight. Pushing the tag only retags the commit's
  `:sha-<commit>` images as `:<tag>` and `:latest`, which is the production
  release. Push it as part of the production deploy, not before.
- Disable any upstream workflow the rebase added (see [CI images](#ci-images)).
- Make sure the test stack ran the final tip: pull `:dev` again after the last
  fix and rerun `cassandra-migrate`.
- Clean up the test host: `~/fluxer-pgtest` and its `fluxer-pg_*`/`fluxer-sc_*`
  volumes, the scratch images, and the worktree.
- Keep `~/fluxer-test` and its snapshot until production is upgraded.
- Hand the production checklist (the prod-relevant rows of `STACK-CHANGES.md`)
  to the production deploy step.

## Gotchas seen so far

- ScyllaDB rejects `ALTER TABLE/TYPE ... ADD IF NOT EXISTS`. The bug only shows
  up when upgrading an existing keyspace.
- `cassandra apply` only set table options at create time. Upstream TTL changes
  to existing tables need the explicit sync the fork adds.
- Seastar holds back memory for the OS unless given `--reserve-memory`. A
  container memory limit that looks generous can still stop ScyllaDB from
  starting.
- A root-owned compose tmpfs hides a directory that a non-root image chowned.
- The meilisearch upgrade is in place and one-way. Snapshot before switching.
- Captcha moved into instance config and is **on by default** (Altcha). The old
  env flags are ignored, and API test scripts must solve it.
- The voice endpoint is derived from the API URL without its port, so voice is
  broken in the tunnel setup. Ignore it in tests.
- API snapshots taken across a restart differ in harmless ways: set and
  clustering order (guild features, roles, group DM recipients), presence
  counts, and `guild_id` on guild members, which only members that joined
  while the gateway was running carry. The member list comes from the
  gateway's memory, not the database.
- `docker compose wait` prints "no containers" once a one-shot service has
  already exited. That message is harmless.
- In `ssh "$HOST" bash -s <<EOF` scripts, `docker compose exec -T` and
  `docker run` read the rest of the script from stdin and silently swallow
  it. Give them `</dev/null`.
