# ci/mirror

The tool that fills fbuild's download mirror bucket (`fbuild`, DigitalOcean
Spaces `sfo3`) from the PlatformIO registry. It moved here from an unversioned
local directory (FastLED/fbuild#2040). `ci/gen_mirror_manifest.py` then
publishes the bucket's contents as `site/mirror/`.

- **`mirror.py`** -- Catalogs the PlatformIO registry into a SQLite state DB
  and transfers every pending file to the bucket under its registry path.
  Runs on a droplet as a systemd service (`mirror.py run`); `status` and
  `verify` are read-only.
- **`rotate.py`** -- Local supervisor that runs `mirror.py` on rotating
  droplets to work around the registry's per-IP daily download cap. For a full
  pass only.
- **`update.py`** -- The incremental pass: mirrors only the registry releases
  the bucket lacks, from one machine, with no droplet. See below.

Tests: `ci/test_mirror_tool.py` (no network, no bucket).

## The state DB

`mirror.py` writes `files(url, key, system, state, ...)`. `rotate.py` keeps the
copy at `_state/manifest.db` in the bucket, and `gen_mirror_manifest.py` reads
the `system` of every `done` row from that copy. The tests build a DB with
`mirror.SCHEMA` and read it through the generator, so the two cannot drift
apart unnoticed.

The bucket copy is the source of truth. `rotate.py` always restores a fresh
droplet from it, never from its local work copy. It stops before creating a
droplet when the local copy has progress the bucket lacks (more rows, more
`done` rows or a newer `updated_at`), and it never uploads a droplet snapshot
that is behind the bucket copy. Copies with the same row count can still differ
in `done` rows, so all three measures are compared.

## Incremental passes (`update.py`)

New releases are a few files a day, so they do not need droplets. `update.py`
downloads the bucket's state DB, catalogs only what changed, prints the queue,
and with no `--dry-run` backs up the DB server-side, transfers the queue and
uploads the DB (both writes are conditional on the ETag it read).

- **What changed.** The registry search is paged newest-updated first
  (`sort=updated`) down to the DB's watermark, the `meta` row
  `registry_updated_since`, less a 6 h overlap. Every package updated since has
  its versions read again, and files the DB lacks are queued. Before this,
  `catalog` re-read versions only for packages it had never seen, so a new
  release of a known package (FastLED 3.10.9, FastLED/fbuild#2034) was never
  queued.
- **Watermark.** A scan records the newest `updated_at` it saw only if it
  covered everything since the old watermark. The bucket DB from the first
  full pass has none: pass `--since` once, at or before the start of that pass
  (2026-10-01 is safe), and later runs need no flag.
- **One package.** `--package owner/type/name` (repeatable) refreshes just
  those packages and leaves the watermark alone.
- **Caps.** `--max-bytes` (default 5 GB) refuses a bigger queue; that is
  `rotate.py`'s job.
- **Afterwards.** Regenerate and check the index, then commit `site/mirror/`:
  only the shards whose content changed get a new version (the UTC date).

```bash
uv run --no-project --script ci/mirror/update.py --since 2026-10-01 --dry-run
uv run --no-project --script ci/mirror/update.py --package fastled/library/FastLED
uv run --no-project --with boto3 python ci/gen_mirror_manifest.py --credentials ~/.config/do-spaces/credentials.json
uv run --no-project --with boto3 python ci/gen_mirror_manifest.py --credentials ~/.config/do-spaces/credentials.json --check
```

Each publishing run leaves a dated `_state/manifest.db.bak-<UTC>` beside the
DB; prune old ones by hand.

## A refused client stops the run

Any 403 from the registry stops the pass with exit 3. Before, it was retried
6 times per file and recorded as that file's error, file after file. In
practice a 403 means the CDN refuses the User-Agent ("This download method is
not supported"), and every other request would fail the same way. Nothing else
is marked as an error; queued rows stay queued. On a droplet `mirror.py`
writes `registry_refused` to its state dir, its unit does not restart it
(`RestartPreventExitStatus=3`), and `rotate.py` snapshots the DB, destroys
the droplet and exits 3 without rotating: a new IP will not help.

## User-Agent

The registry CDN only serves User-Agents that start with `PlatformIO/`. On
2026-11-17 it also answered 403 "This download method is not supported" to any
UA carrying the old `fbuild-mirror` token, which had worked on 2026-10-30.
The default is now fbuild's production format, `PlatformIO/6.2.0 (fbuild/<version>)`
(`registry_user_agent()` in `crates/fbuild-core/src/http.rs`), with the version
read from the checkout's `Cargo.toml`. `FBUILD_MIRROR_UA` overrides it.
`rotate.py` resolves the UA locally and writes it into the droplet's unit file.

## Configuration

Credentials are never in the repo. The scripts read a JSON file with
`access_key` and `secret_key`.

| Variable | Used by | Default |
|---|---|---|
| `FBUILD_MIRROR_UA` | all | production UA for the checkout's version |
| `FBUILD_MIRROR_CREDS` | all | `~/.config/do-spaces/credentials.json` (rotate), `/etc/fbuild-mirror/spaces.json` (droplet) |
| `FBUILD_MIRROR_WORKDIR` | rotate, update | `~/.local/state/fbuild-mirror` (status file, `rotation-work/`, `update-work/`) |
| `FBUILD_MIRROR_SSH_KEY_ID` | rotate | required: the DigitalOcean SSH key id (`doctl compute ssh-key list`) |
| `FBUILD_MIRROR_SSH_KEY` | rotate | `~/.ssh/id_rsa` |
| `FBUILD_MIRROR_STATE`, `_BUCKET`, `_REGION`, `_CONCURRENCY` | mirror | set by the droplet's unit file |

```bash
uv run --no-project --script ci/mirror/rotate.py --print-user-agent
uv run --no-project --script ci/mirror/rotate.py --status
uv run --no-project --script ci/mirror/rotate.py        # creates droplets and writes the bucket
uv run --no-project --script ci/mirror/update.py --dry-run  # incremental: lists, writes nothing
```
