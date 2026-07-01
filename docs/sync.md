# `fbuild sync` — dependency sync + `platformio.lock`

`fbuild sync` reads `platformio.ini`, classifies every `lib_deps` entry per
environment, and writes a deterministic JSON `platformio.lock` next to
`platformio.ini`.

FastLED/fbuild#618 Phase 1. Phase 2 (network resolution of GitHub refs +
PIO registry versions + archive `sha256`) and Phase 3 (build/deploy
consumption of the lockfile) are follow-ups.

## Quick start

```bash
# Sync all envs — prompts if there is more than one.
fbuild sync

# Sync one env, no prompt.
fbuild sync -e uno

# Non-interactive multi-env sync.
fbuild sync --yes

# Validate freshness without installing/writing.
fbuild sync --check

# Refuse to write; require the on-disk lock to match the ini.
fbuild sync --locked

# Print the plan; don't touch the disk.
fbuild sync --dry-run
```

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Lockfile written / already fresh / `--check` passed / `--dry-run` |
| 1 | `--check` failed (stale or missing lock) |
| 2 | `--locked` failed (lock missing or doesn't match ini) |
| 3 | User cancelled the multi-env prompt |
| 4 | Hard error (missing `platformio.ini`, parse error, etc.) |

## `lib_deps` source classification

Each `lib_deps` entry is classified into one of these source types:

| Entry shape | `source_type` | `status` in lock |
|---|---|---|
| `FastLED` | `registry` | `unresolved` |
| `FastLED@^3.5.0` | `registry` | `unresolved` |
| `fastled/FastLED@^3.5.0` | `registry` | `unresolved` |
| `https://github.com/FastLED/FastLED[.git][#ref]` | `github` | `unresolved` |
| `git+https://gitlab.com/foo/bar.git[#ref]` | `git` | `unresolved` |
| `https://example.com/lib.zip` | `http-archive` | `unresolved` |
| `symlink:///path/to/lib` | `symlink` | `unlocked` |
| `file:///path/to/lib` | `file` | `unlocked` |
| `./libs/mylib` / `../shared/lib` / `/abs/path` / `C:\path` | `local-path` | `unlocked` |

**Phase 1 does not perform network resolution** — every remote entry gets
`status: "unresolved"` and captures the raw spec, extracted owner/name/ref,
and URL. Phase 2 will add the resolved commit SHA, archive URL, and
`sha256`, flipping the status to `locked`.

Local sources (`symlink://`, `file://`, filesystem paths) always get
`status: "unlocked"` — they can't be reproducibly locked off the
developer's machine, but they're still recorded for auditability.

## Lockfile schema

Written as `<project-dir>/platformio.lock`, JSON pretty-printed with a
trailing newline. Keys are sorted deterministically so identical inputs
always yield byte-identical output.

```json
{
  "version": 1,
  "generated_at": "2026-06-30T22:15:04Z",
  "envs": {
    "esp32": {
      "packages": [
        {
          "name": "FastLED",
          "source_type": "registry",
          "raw": "FastLED@^3.5.0",
          "version_spec": "^3.5.0",
          "status": "unresolved"
        },
        {
          "name": "mylib",
          "source_type": "local-path",
          "raw": "./libs/mylib",
          "local_path": "./libs/mylib",
          "status": "unlocked"
        }
      ]
    },
    "uno": { "packages": [ ... ] }
  }
}
```

Notes:

- `envs` is a sorted map — envs appear alphabetically.
- `packages` within each env is sorted by `(name, source_type, raw)`.
- Package records are **duplicated** under each env (issue decision:
  auditability wins over disk).
- `generated_at` is UTC to second precision. Identical resolutions
  produce identical bytes as long as the timestamp matches.
- Phase 2 will add `resolved_sha`, `resolved_url`, `sha256`, and
  `resolved_version` fields when the entry is fully locked.

## Atomicity

Every write goes through `fbuild_core::fs::write_atomic_sync` (from
FastLED/fbuild#865): write to a sibling temp file, fsync, then atomic
rename. Partial writes are never observable.

## Multi-env prompt

The default `fbuild sync` prompts before touching more than one env:

```
fbuild sync will resolve 3 environments: esp32, teensy41, uno
Proceed? [y/N]
```

Bypass with any of:

- `--yes` — non-interactive; proceed for scripts / CI
- `-e <env>` — pick one env; no prompt
- `--check` — validation-only; never prompts because it can't install/write

## Deferred to Phase 2 / Phase 3

Documented in issue #618:

- Actual network resolution (GitHub refs → SHA, PIO registry version → archive URL, archive sha256)
- `--upgrade` / `--upgrade-package <name>` semantics
- Toolchain / platform / framework locking
- HTTP `POST /api/sync` daemon endpoint (Phase 1 runs in-process)
- Strict build/deploy consumption of the lock — build/deploy still read
  `platformio.ini` directly; the lock is auditing-only for now
