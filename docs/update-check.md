# Update check

`fbuild` runs a passive update check in the background at the top of every
command. When a newer stable release exists, it prints a one-line warning
to `stderr` and points at the right update path for how you installed
`fbuild` in the first place.

FastLED/fbuild#626 Phase 1 — passive warning only. Phase 2 (`fbuild update
check` / `fbuild update apply` subcommands) and Phase 3 (self-replace on
direct GitHub installs) are follow-ups.

## What you see

```
fbuild 2.3.15 → 2.3.16 available (PyPI). run: python -m pip install --upgrade fbuild
```

or, for a native binary you downloaded directly:

```
fbuild 2.3.15 → 2.3.16 available (GitHub release). download: https://github.com/FastLED/fbuild/releases/tag/v2.3.16
```

The message is on `stderr` only, appears at most once per 24 hours per
install (per the on-disk cache), and never fails the command that
triggered it. Network errors are swallowed with a `tracing::debug!` line
so `RUST_LOG=fbuild_cli::update_check=debug fbuild ...` will surface them
when you're deliberately troubleshooting.

## Install-source classification

`fbuild` walks the filesystem around its own binary to figure out how it
was installed:

| Install shape | Detection | Update path |
|---|---|---|
| `pip install fbuild` from PyPI | `fbuild-*.dist-info/` next to the binary, no `direct_url.json` | `python -m pip install --upgrade fbuild` |
| `pip install .` in the repo | `dist-info/direct_url.json` with `dir_info.editable: true` OR `url: file://...` | `git pull && pip install -e .` |
| `pip install git+https://...` | `dist-info/direct_url.json` with `vcs_info` | `pip install --upgrade git+https://github.com/FastLED/fbuild` |
| Downloaded native binary | No `dist-info` in the ancestor tree | `https://github.com/FastLED/fbuild/releases/tag/v<latest>` |
| Ambiguous | Falls through to "GitHub release" (safer — the URL always works) | Same as above |

Set `FBUILD_INSTALL_SOURCE=pypi|local|vcs|direct|unknown` to override the
classification (handy when the filesystem probe misidentifies something
and you want to unblock the warning).

## Opting out

Any one of these disables the check:

- **`--no-update-check`** on the command line
- **`FBUILD_NO_UPDATE_CHECK=1`** environment variable
- Running in CI — `CI=true` / `GITHUB_ACTIONS=true` / `GITLAB_CI=true` /
  `CIRCLECI=true` / `JENKINS_URL=<anything>` all auto-suppress. The
  intent is that CI systems shouldn't get slower/noisier from a check
  they can't act on anyway.

## The cache

`fbuild` writes the last successful check to
`~/.fbuild/prod/cache/update_check.json` (or `~/.fbuild/dev/cache/...`
under `FBUILD_DEV_MODE=1`). Fields:

```json
{
  "checked_at_epoch_secs": 1751284800,
  "install_source": "pypi",
  "current_version": "2.3.15",
  "latest_version": "2.3.16",
  "stale": true,
  "check_url": "https://pypi.org/simple/fbuild/",
  "ttl_secs": 86400
}
```

Delete the file to force a fresh network check on the next command.

## Where the network calls go

- **PyPI**: `GET https://pypi.org/simple/fbuild/` with
  `Accept: application/vnd.pypi.simple.v1+json`. Picks the highest stable
  (non-prerelease) version from the returned `versions` array.
- **GitHub**: `GET https://api.github.com/repos/FastLED/fbuild/releases/latest`.
  This endpoint explicitly excludes drafts and prereleases per the GitHub
  REST spec, so the returned `tag_name` is always a stable release.

Both requests use a 3-second timeout and a `User-Agent: fbuild-cli/<version>`
header. Unauthenticated for public repos is fine (60 GitHub reqs/hour/IP;
the 24h cache keeps us well under).

## When to expect prerelease warnings

`fbuild 2.4.0-rc.1` (a prerelease) DOES see newer prereleases:

```
fbuild 2.4.0-rc.1 → 2.4.0-rc.2 available (PyPI). run: python -m pip install --upgrade fbuild
```

But `fbuild 2.3.15` (stable) does NOT get warned about `2.4.0-rc.1`. The
switch to `--pre` semantics (opting into prerelease-aware warnings from a
stable install) is filed as a Phase 2 follow-up on #626.
