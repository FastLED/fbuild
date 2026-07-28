# Build Progress Web Assets

`index.html` is the self-contained Build Progress page served by
`fbuild-daemon` at `GET /build-progress` (FastLED/fbuild#1076 Phase 2,
`crates/fbuild-daemon/src/handlers/build_progress.rs`, embedded via
`include_str!` following the same pattern as `../plotter/index.html`).

The page has no build step and no external dependencies. It is built
entirely out of existing, unmodified daemon endpoints:

- `GET /api/daemon/info`, polled every ~2s, for the status header
  (`daemon_state`, `current_operation`, `operation_in_progress`,
  `dependency_install`, uptime/pid/version).
- `GET /ws/logs`, the existing `BroadcastHub`-backed WebSocket that every
  daemon `tracing::*` event already flows through, for a live scrolling
  log pane with an autoscroll toggle and clear button.

See the "Observability reality" doc comment at the top of
`build_progress.rs` for why the page does *not* attach to the build's own
NDJSON output stream (`POST /api/build`): that stream is per-HTTP-request,
not broadcast, so a second client (this page) cannot observe another
client's in-flight build's compiler output without new server-side
broadcast plumbing. `/ws/logs` and `/api/daemon/info` are the observable,
already-broadcast alternative, so this page reuses them unmodified rather
than inventing new daemon state.
