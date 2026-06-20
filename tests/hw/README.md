# `tests/hw/` — nightly hardware-CI fixtures

FastLED/fbuild#696. The on-disk fixtures the
[`hw-ci.yml`](../../.github/workflows/hw-ci.yml) workflow consumes.

## Layout

```
tests/hw/
├── README.md                           — this file
├── fingerprints/
│   ├── esp32s3.txt                     — one VID:PID per line, expected USB devices
│   ├── lpc845brk.txt
│   ├── pico.txt
│   ├── teensy41.txt
│   └── samd51.txt
├── known_good_esp32s3.bin              — pinned-by-content firmware target (added per-board)
├── known_good_lpc845brk.elf            — ARM ELF for SWD-flashed boards
├── known_good_pico.uf2                 — alternative extension for UF2 boards
└── …
```

Each board family's row in `fingerprints/` is what
`fbuild serial probe find --vid-pid VID:PID` looks for — see
[`crates/fbuild-serial/src/boards.rs`](../../crates/fbuild-serial/src/boards.rs)
for the curated `BOARD_FINGERPRINTS` table.

The `known_good_<board>.*` blobs are pinned-by-commit firmware
artifacts. Any change is a deliberate PR — the bring-up test target
stays stable so a CI failure is unambiguously a regression in
`fbuild-serial` / `fbuild-deploy`, not in the firmware payload.

## Adding a new board family

1. Add a row to
   [`crates/fbuild-serial/src/boards.rs::BOARD_FINGERPRINTS`](../../crates/fbuild-serial/src/boards.rs).
2. Write `tests/hw/fingerprints/<board>.txt` with the VID:PID(s)
   that should be present when the board is plugged in.
3. Build a known-good firmware (the bring-up `examples/AutoResearch.ino`
   equivalent for that board) and drop the binary under
   `tests/hw/known_good_<board>.{bin,elf,uf2}`.
4. Add the new family name to the `matrix.board` list in
   [`hw-ci.yml`](../../.github/workflows/hw-ci.yml).
5. Plug the board into the self-hosted runner host (see
   [`agents/docs/hardware-ci-setup.md`](../../agents/docs/hardware-ci-setup.md)).

## See also

- [`agents/docs/hardware-ci-setup.md`](../../agents/docs/hardware-ci-setup.md)
  — how to register a self-hosted runner with the `hw-ci` label.
- FastLED/fbuild#586 — LPC845-BRK on-hand burn-down meta. Same
  hardware, different question.
