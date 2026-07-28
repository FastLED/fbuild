# Board Manager Web Assets

`index.html` is the self-contained Board Manager page served by
`fbuild-daemon` at `GET /boards` (FastLED/fbuild#1076 Phase 2,
`crates/fbuild-daemon/src/handlers/boards.rs`, embedded via `include_str!`
following the same pattern as `../plotter/index.html`).

Read-only first cut: browsing/inspection only, no install/mutation
actions. It fetches the full board list once from the daemon's new
`GET /api/ide/boards` endpoint (backed by
`fbuild_config::search_boards`, the same embedded PlatformIO-registry
board database used by `fbuild build`/`fbuild deploy`), then filters and
expands rows entirely client-side — typing in the search box does not
re-fetch. Clicking a row expands a raw-JSON detail view of that board's
summary fields (id, name, platform, mcu, f_cpu, ram, flash).

No build step, no external dependencies — all CSS/JS inline, no CDN.
