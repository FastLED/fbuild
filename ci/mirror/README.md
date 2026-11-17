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
  droplets to work around the registry's per-IP daily download cap.

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

## User-Agent

The registry CDN only serves User-Agents that start with `PlatformIO/`. On
2026-11-17 it also answered 403 "This download method is not supported" to any
UA carrying the old `fbuild-mirror` token, which had worked on 2026-10-30.
The default is now fbuild's production format, `PlatformIO/6.2.0 (fbuild/<version>)`
(`registry_user_agent()` in `crates/fbuild-core/src/http.rs`), with the version
read from the checkout's `Cargo.toml`. `FBUILD_MIRROR_UA` overrides it.
`rotate.py` resolves the UA locally and writes it into the droplet's unit file.

## Configuration

Credentials are never in the repo. Both scripts read a JSON file with
`access_key` and `secret_key`.

| Variable | Used by | Default |
|---|---|---|
| `FBUILD_MIRROR_UA` | both | production UA for the checkout's version |
| `FBUILD_MIRROR_CREDS` | both | `~/.config/do-spaces/credentials.json` (rotate), `/etc/fbuild-mirror/spaces.json` (droplet) |
| `FBUILD_MIRROR_WORKDIR` | rotate | `~/.local/state/fbuild-mirror` (status file, `rotation-work/`) |
| `FBUILD_MIRROR_SSH_KEY_ID` | rotate | required: the DigitalOcean SSH key id (`doctl compute ssh-key list`) |
| `FBUILD_MIRROR_SSH_KEY` | rotate | `~/.ssh/id_rsa` |
| `FBUILD_MIRROR_STATE`, `_BUCKET`, `_REGION`, `_CONCURRENCY` | mirror | set by the droplet's unit file |

```bash
uv run --no-project --script ci/mirror/rotate.py --print-user-agent
uv run --no-project --script ci/mirror/rotate.py --status
uv run --no-project --script ci/mirror/rotate.py        # creates droplets and writes the bucket
```
