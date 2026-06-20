# `online-data` branch + nightly refresh

The repo carries a long-lived orphan branch called `online-data` that holds
periodically-refreshed reference datasets fbuild reads at runtime. Datasets
currently published:

| Dataset | Path | Description |
|---|---|---|
| `usb-vid` | `data/usb-vid.json` | USB VID:PID → `{vendor, product}` (union of multiple sources) |
| `usb-vid-conflicts` | `data/usb-vid-conflicts.json` | Per-key disagreements between USB-VID sources (observability) |
| `pio-boards` | `data/pio-boards.json` | Full PlatformIO board catalog (vendor, mcu, frameworks, debug tools, etc.) |
| `vendor_boards` | `data/vendor_boards.json` | Slim view of `pio-boards` — only `{vendor, name, mcu}` per board id, for cheap "what board is plugged in?" lookups |

The format is **future-forward** — new datasets are added by writing a new
JSON file under `data/`; `tools/build_manifest.py` auto-discovers them on
the next workflow run. No client breakage when datasets are added.

The companion in-process USB resolver lives at `fbuild_core::usb` — see
`crates/fbuild-core/src/usb/`. The branch is the **tier-2 fallback** when
the bundled `usb-ids` crate doesn't know a VID:PID.

## URLs

Always start from the manifest — direct dataset URLs may change in the
future, but the manifest's `datasets.<name>.url` field is the contract.

- Manifest (entry point — clients fetch this first):
  `https://raw.githubusercontent.com/fastled/fbuild/online-data/manifest.json`
- USB VID:PID dataset:
  `https://raw.githubusercontent.com/fastled/fbuild/online-data/data/usb-vid.json`
- USB-VID source-conflict log:
  `https://raw.githubusercontent.com/fastled/fbuild/online-data/data/usb-vid-conflicts.json`
- PlatformIO full board catalog:
  `https://raw.githubusercontent.com/fastled/fbuild/online-data/data/pio-boards.json`
- PlatformIO slim vendor-name lookup (small, ~200 KB):
  `https://raw.githubusercontent.com/fastled/fbuild/online-data/data/vendor_boards.json`

The matching constants in code: `fbuild_core::usb::MANIFEST_URL` and
`fbuild_core::usb::USB_VID_JSON_URL`.

## Branch shape

```
online-data (orphan, NEVER merged into main)
├── README.md
├── manifest.json
├── data/
│   ├── usb-vid.json            # alphabetically sorted, lowercase hex keys
│   └── usb-vid-conflicts.json  # only keys where sources disagreed
└── tools/
    ├── README.md
    └── merge_sources.py        # union + sort + manifest emit
```

There is **no `Cargo.toml`, no `src/`, no workspace member** on
`online-data` — the dump-side tooling for the bundled `usb-ids` crate
lives on `main` as an example (`crates/fbuild-core/examples/dump_usb_ids.rs`)
so we don't have to add a new crate. The nightly workflow checks out main
to build that example, then checks out `online-data` in a worktree to run
the merger script and commit results.

## How a refresh happens

`.github/workflows/update-data.yml` is the only workflow that touches
`online-data`. It lives on `main` because GitHub Actions requires `schedule`
and `workflow_dispatch` triggers to be defined on the default branch.

Per run:

1. Checkout `main` (workflow + dump example live here).
2. `git worktree add` the `online-data` branch into a sibling directory.
3. Install uv + soldr.
4. `soldr cargo build --release --example dump_usb_ids -p fbuild-core`
   then run it → `/tmp/usb-ids-rs.json` (one input source).
5. `curl --retry 5` two upstream `usb.ids` text mirrors:
   `http://www.linux-usb.org/usb.ids` and
   `https://raw.githubusercontent.com/usbids/usbids/master/usb.ids`
   (independently fault-tolerant — one mirror going down does not break
   the run).
6. `uv run --no-project --script .online-data/tools/merge_sources.py …`
   over whichever sources arrived intact. The merger:
   - takes the union, prefers `usb-ids-rs` > `linux-usb.org` > `usbids-github`
     on conflict;
   - sorts keys alphabetically (lowercase `vvvv:pppp`);
   - writes `data/usb-vid.json`, `data/usb-vid-conflicts.json`,
     and the freshly-stamped `manifest.json`;
   - **refuses to write** if the union has fewer than 1000 entries so a
     truncated upstream cannot blow away a healthy committed dataset.
7. If files actually changed, commit on `online-data`.
8. Prune history: if `git rev-list --count HEAD > 200`, graft the
   200-th-most-recent commit as the new root and `git filter-repo`.
9. `git push --force-with-lease origin online-data` (the force is needed
   only when history was pruned).

Manual trigger: Actions → "Update data" → Run workflow.

## Fault tolerance contract

- **`usb-ids` build / dump fails** → workflow continues with text sources.
- **One upstream mirror unreachable** → merger still runs against the
  remaining sources.
- **All upstream sources fail** → merger refuses to write → workflow
  finishes with no commit; existing committed data is preserved.
- **Merger writes too-small output** → same as above (sanity floor).
- **Workflow itself fails before commit** → previous commit on
  `online-data` remains the live data.

In every failure mode the *previously committed* data on `online-data`
stays as the live truth — fbuild keeps working against the last good
snapshot.

## Why orphan + force-push?

- Orphan: `online-data` shares no history with `main`. We never want
  data churn rebasing into the source tree.
- Force-push: only after the history-prune step rewrites the chain to
  cap at 200 commits. A non-pruning run produces a normal fast-forward.

## Manifest schema (future-forward)

```json
{
  "schema_version": "1.0",
  "generated_at": "2026-06-20T04:17:00Z",
  "datasets": {
    "usb-vid": {
      "description": "USB VID:PID → {vendor, product} ...",
      "url": "https://raw.githubusercontent.com/fastled/fbuild/online-data/data/usb-vid.json",
      "conflicts_url": "https://raw.githubusercontent.com/fastled/fbuild/online-data/data/usb-vid-conflicts.json",
      "format": "json-object",
      "key_format": "vvvv:pppp",
      "entries": 20536,
      "sources": [
        {"name": "usb-ids-rs", "kind": "json",          "entries": "20480"},
        {"name": "linux-usb.org", "kind": "usb.ids-text", "entries": "20536"},
        {"name": "usbids-github", "kind": "usb.ids-text", "entries": "20536"}
      ]
    }
  }
}
```

Adding a new dataset (`pci-vid`, `board-features`, …) means appending
another entry under `datasets` and shipping a parser in the consuming
crate — no schema break.
