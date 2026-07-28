# Library Manager Web Assets

`index.html` is the self-contained Library Manager page served by
`fbuild-daemon` at `GET /libraries` (FastLED/fbuild#1076 Phase 2,
`crates/fbuild-daemon/src/handlers/libraries.rs`, embedded via
`include_str!` following the same pattern as `../plotter/index.html`).

Read-only first cut: browsing/inspection only, no install/mutation
actions. This page is not daemon-global like `/boards` or `/plotter` — it
needs a project directory and environment to know which
`platformio.ini` to read, so it reads `?project=<dir>&env=<name>` from
its own URL and passes them straight through to
`GET /api/ide/libraries`. Missing `?project=` renders a short how-to
pointing at `fbuild libraries`, the CLI opener that resolves the current
project/env and pre-fills both params.

The data endpoint classifies each `lib_deps` entry with
`fbuild_config::classify_lib_dep` (the same classifier `fbuild sync`
uses) and reports a best-effort installed/not-installed flag — see the
response's `install_state_note` field (also rendered at the bottom of
this page) for what that detection does and does not guarantee.

No build step, no external dependencies — all CSS/JS inline, no CDN.
