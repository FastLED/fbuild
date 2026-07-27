# Serial Plotter Web Assets

`index.html` is the self-contained Serial Plotter page served by
`fbuild-daemon` at `GET /plotter` (FastLED/fbuild#1076 Phase 2,
`crates/fbuild-daemon/src/handlers/plotter.rs`, embedded via
`include_str!` following the same pattern as `../avr8js/app.js`).

The page has no build step and no external dependencies: all CSS and JS
are inline, and the chart is hand-rolled `<canvas>` drawing (no charting
library). It connects to the daemon's existing `/ws/serial-monitor`
WebSocket to receive serial data and to `POST /api/devices/list` to
populate its port selector — both endpoints already exist and are used
unmodified by other fbuild-daemon clients (the CLI's `fbuild monitor`
and `fbuild device` commands).

Numeric series are parsed from incoming serial lines Arduino-Serial-
Plotter style: whitespace/comma-separated numbers, with an optional
`name:value` label per token.
