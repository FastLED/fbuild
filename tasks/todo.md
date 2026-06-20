# TODO — USB VID:PID resolver + `online-data` branch (FastLED/fbuild)

## Goal

Two-part design so fbuild can always translate a USB `VID:PID` to a
human-readable `vendor / product` name:

1. **First line of defense** — bundled `usb-ids` Rust crate (offline, MIT,
   `phf` perfect-hash table, zero allocations, zero network).
2. **Backup / live updates** — orphan `online-data` branch in this same repo,
   refreshed once per day by a GitHub Actions workflow. Branch carries a
   `manifest.json` (future-forward index) pointing at `data/usb-vid.json`
   (sorted alphabetical union of multiple upstream sources). fbuild reads the
   JSON when the bundled crate cannot resolve a VID:PID. Conflicts between
   sources go into `data/usb-vid-conflicts.json` for observability.

The acceptance bar (verbatim from the user):
- PR is merged.
- The nightly workflow is run manually via `workflow_dispatch`.
- The manifest URL `https://raw.githubusercontent.com/fastled/fbuild/online-data/manifest.json` actually resolves.
- A fbuild unit test demonstrates the `resolve(vid, pid)` API works end-to-end.
- When fbuild **connects to** or **scans** a device, the printed output includes
  the pretty `vendor product (VVVV:PPPP)` string — not just raw hex.

## Sources we will merge

| Source | URL | Format | Notes |
|---|---|---|---|
| `usb-ids` crate (built via soldr) | bundled at compile time | Rust → JSON dump | Tracks upstream snapshots; primary truth |
| linux-usb.org (canonical) | `http://www.linux-usb.org/usb.ids` | plain text | HTTPS is broken (SAN mismatch) — `http://` is intentional |
| usbids/usbids GitHub mirror | `https://raw.githubusercontent.com/usbids/usbids/master/usb.ids` | plain text | CDN-backed, tracks upstream same-day |

Priority order on conflict: `usb-ids-rs` > `linux-usb.org` > `usbids/usbids`.
All conflicts are still recorded in `usb-vid-conflicts.json`.

## File layout (on `main` — minimal: resolver + workflow trigger only)

```
crates/fbuild-core/
  Cargo.toml                          # +usb-ids = "1.2025"
  src/lib.rs                          # +pub mod usb;
  src/usb/
    mod.rs                            # re-exports
    resolver.rs                       # resolve(vid, pid), tiers
    data.rs                           # online JSON load + OnceLock cache
    tests.rs                          # FTDI / CP210x / CH340 / unknown

crates/fbuild-daemon/
  src/device_manager.rs               # enrich description via resolver
  src/models.rs                       # +vendor_name/product_name on DeviceInfo, etc.
  src/handlers/devices.rs             # populate the new fields

crates/fbuild-cli/
  src/cli/device.rs                   # display "vendor product (VVVV:PPPP)" in list/status

.github/workflows/
  nightly-usb-ids.yml                 # cron + workflow_dispatch — checks out
                                      # online-data into a worktree, builds the
                                      # tools FROM THERE, commits data back.
                                      # No tooling lives on main.

docs/
  online-data.md                      # documents the branch + workflow + schema
```

## File layout (on `online-data` orphan branch — tools + data)

```
README.md                             # explains: orphan, do-not-merge, structure
manifest.json                         # future-forward index of datasets
data/usb-vid.json                     # {} initially, populated by workflow
data/usb-vid-conflicts.json           # {} initially
.gitignore                            # bury *.bak / *.tmp

tools/usb-ids-dump/                   # standalone, NOT a workspace crate (and
  Cargo.toml                          # not on main at all → no impact on the
  src/main.rs                         # main-branch crate-gate / monocrate policy)
  README.md
tools/merge_sources.py                # union + sort + manifest emit
tools/README.md                       # how the nightly workflow uses these
```

> The workflow YAML still lives on `main` (GitHub requires `schedule` and
> `workflow_dispatch` triggers to be defined on the default branch). The
> workflow itself does nothing except: `git worktree add` the `online-data`
> branch, run the tools from there, and commit data files back.

## API shape — `fbuild_core::usb`

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsbInfo {
    pub vendor: String,
    pub product: String,
}

/// Best-effort resolve; never returns None. If unknown, returns
/// `UsbInfo { vendor: "Unknown vendor 0xVVVV", product: "Unknown product 0xPPPP" }`.
pub fn resolve(vid: u16, pid: u16) -> UsbInfo;

/// Only returns Some if either the bundled crate or the online cache resolved.
pub fn try_resolve(vid: u16, pid: u16) -> Option<UsbInfo>;

/// Tier-1 only (bundled `usb-ids`).
pub fn resolve_bundled(vid: u16, pid: u16) -> Option<UsbInfo>;

/// Install an override map. Called by the daemon at startup with the
/// path to `~/.fbuild/cache/usb-vid.json` (or wherever the CLI cached
/// the last download).
pub fn install_online_cache(path: &std::path::Path);

/// Convenience pretty formatter:
///   "vendor product (VVVV:PPPP)"
/// Used by the CLI's device list / connect / scan log lines.
pub fn pretty(vid: u16, pid: u16) -> String;
```

Internals:
- Static `OnceLock<HashMap<u32, UsbInfo>>` for the online overlay.
- Key packing: `(vid as u32) << 16 | pid as u32`.
- `install_online_cache` reads file once, parses serde_json into the map. Silent on IO error (the resolver still works via tier 1 + fallback formatter).

## Daemon / CLI wiring

- `device_manager::refresh_devices` — when building each `DiscoveredDevice`, if `vid` and `pid` are present, call `fbuild_core::usb::resolve(vid, pid)` and stash both `vendor_name` + `product_name` on the device record. The free-form `description` becomes `"{vendor} {product}"` (overriding the bland `usb.product` string from `serialport`).
- `DeviceState`, `DeviceInfo`, `DeviceStatusResponse` — gain `Option<String> vendor_name` and `Option<String> product_name`.
- `fbuild-cli` — `device list` and `device status` print the new fields; deploy / monitor log lines that mention a port now use `fbuild_core::usb::pretty(vid, pid)` so the user always sees the friendly name.

## Workflow design — `.github/workflows/nightly-usb-ids.yml`

- Trigger: `schedule: cron '17 4 * * *'` (daily) + `workflow_dispatch`.
- Runner: ubuntu-latest.
- Permissions: `contents: write` (must push to `online-data`).
- Concurrency: `cancel-in-progress: false`, group `nightly-usb-ids` (don't trample a running update).

Steps (high-level):
1. `actions/checkout@v6` (default — `main`, for the dump binary + script).
2. `setup-uv` + `setup-soldr` (same versions as `check-ubuntu.yml`).
3. `soldr cargo build --release --manifest-path ci/usb_ids/Cargo.toml` → produces `ci/usb_ids/target/release/dump-usb-ids`. **If this step fails, the workflow continues with the existing data — fault tolerance #1.** We do this by `continue-on-error: true` + a step output flag.
4. Run the dump: redirect stdout to `/tmp/source-usb-ids-rs.json`. Sanity-check entry count (>= 5000) — else mark as failed-but-non-fatal.
5. Download external sources with `curl --retry 5 --retry-delay 10 --fail` into `/tmp/`. Each download is independently fault-tolerant: a failure flags the source as missing for this run but does NOT abort the workflow.
6. `uv run python ci/usb_ids/merge_sources.py --rs … --txt … --out-dir /tmp/merged`. The script:
   - reads only the sources that arrived intact this run,
   - falls back to the previously-committed `data/usb-vid.json` from `online-data` for any missing tier,
   - refuses to emit a `usb-vid.json` with fewer than 1000 entries (sanity floor),
   - sorts keys alphabetically (`json.dumps(..., sort_keys=True, indent=2)` with stable encoding),
   - writes `usb-vid.json`, `usb-vid-conflicts.json`, `manifest.json`.
7. Fetch + worktree `online-data` (we keep the branch in a separate `git worktree`, so we never touch the workflow checkout).
8. Replace the data files in the worktree only with files the merger actually emitted. If the merger emitted nothing for a given file, leave the existing committed copy in place — fault tolerance #2.
9. `git add manifest.json data/`. If `git diff --cached --quiet`, skip the commit (no churn commits).
10. Otherwise commit with a message like `chore(usb-ids): nightly refresh 2026-06-20 (sources: rs, linux-usb, github)` listing which sources contributed.
11. Prune history to last 200 commits: count via `git rev-list --count HEAD`; if > 200, find the boundary commit, `git replace --graft` it as a new root, run `git filter-repo` (or `filter-branch` if filter-repo isn't on the runner) to rewrite, clean the replace refs.
12. `git push --force-with-lease origin online-data` (force is needed only when history was pruned; otherwise a plain push).

No build artifacts saved. The merger writes to `/tmp/`; the workflow only commits `manifest.json` + `data/*.json` + the existing `README.md` in `online-data`.

## `manifest.json` schema (future-forward)

```json
{
  "schema_version": "1.0",
  "generated_at": "2026-06-20T04:17:00Z",
  "datasets": {
    "usb-vid": {
      "description": "USB VID:PID → {vendor, product} (union of multiple sources)",
      "url": "https://raw.githubusercontent.com/fastled/fbuild/online-data/data/usb-vid.json",
      "conflicts_url": "https://raw.githubusercontent.com/fastled/fbuild/online-data/data/usb-vid-conflicts.json",
      "format": "json-object",
      "key_format": "vvvv:pppp (lowercase hex, colon-separated)",
      "entries": 24536,
      "sources": [
        {"name": "usb-ids-rs", "version": "1.2025.2"},
        {"name": "linux-usb.org", "fetched_at": "2026-06-20T04:17:11Z"},
        {"name": "usbids/usbids", "fetched_at": "2026-06-20T04:17:13Z"}
      ]
    }
  }
}
```

Adding a future dataset (say `pci-vid`) means appending another entry under
`datasets` — no clients break.

## `usb-vid.json` schema

```json
{
  "0403:6001": {"vendor": "Future Technology Devices International, Ltd", "product": "FT232 Serial (UART) IC"},
  "10c4:ea60": {"vendor": "Silicon Labs", "product": "CP210x UART Bridge"},
  ...
}
```

Alphabetical sort by key (`json.dumps(sort_keys=True)`); 2-space indent; trailing newline.

## `usb-vid-conflicts.json` schema

```json
{
  "0403:6001": [
    {"source": "usb-ids-rs",   "vendor": "...", "product": "..."},
    {"source": "linux-usb.org","vendor": "...", "product": "..."}
  ]
}
```

Only entries that actually had disagreement appear here; the chosen winner is
the one in `usb-vid.json` (priority order above).

## Acceptance plan (executable)

1. `soldr cargo test -p fbuild-core usb::` — unit tests for `resolve()` pass for FTDI / CP210x / CH340 / Espressif / unknown.
2. `soldr cargo test -p fbuild-daemon` — DeviceManager tests still pass; new test confirms enriched description.
3. `soldr cargo clippy --workspace --all-targets -- -D warnings` clean.
4. PR open on a feature branch; `crate-gate.yml` passes (we did not add a workspace crate).
5. Push `online-data` orphan branch with seed contents — verify `https://raw.githubusercontent.com/fastled/fbuild/online-data/manifest.json` returns the seed manifest.
6. Merge PR.
7. From the Actions tab, manually run `nightly-usb-ids` via `workflow_dispatch`.
8. After the run succeeds, refetch `manifest.json` and confirm:
   - `entries >= 20000`
   - `sources` lists `usb-ids-rs` + the two text sources
   - the `url` field actually serves a JSON object with `>= 20000` keys
9. Goal hook should auto-clear once all of the above are demonstrated.

## Review (filled in at the end)

(left blank for now — to be appended once everything is merged and verified)
