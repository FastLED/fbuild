# Hardware CI runner setup

How to register a self-hosted runner so the
[`hw-ci.yml`](../../.github/workflows/hw-ci.yml) workflow has somewhere
to run. FastLED/fbuild#696.

## What you need

- A Linux box (Pi 5, NUC, or similar) with at least four free USB
  ports.
- One representative of each board family in
  [`crates/fbuild-serial/src/boards.rs::BOARD_FINGERPRINTS`](../../crates/fbuild-serial/src/boards.rs)
  that you want covered. The matrix in
  [`hw-ci.yml`](../../.github/workflows/hw-ci.yml) lists the canonical
  set today: ESP32-S3, LPC845-BRK, Pico, Teensy 4.1, SAMD51.
- Network access from the runner host to `github.com`.

## Steps

1. **Register the runner on the repo.**
   <https://github.com/FastLED/fbuild/settings/actions/runners/new>
   — pick the "Linux x64" / "Linux ARM64" variant matching your host.
   When prompted for labels, **add both `self-hosted` and `hw-ci`**.
   The workflow's `runs-on: [self-hosted, hw-ci]` requires both.

2. **Install fbuild's toolchain on the runner host.**
   ```bash
   # Match the version of rustup / cargo / uv that the workflow uses.
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   curl -LsSf https://astral.sh/uv/install.sh | sh
   uv tool install soldr
   ```

3. **Add per-board USB rules so the runner can talk to the devices
   without `sudo`.**
   ```bash
   # /etc/udev/rules.d/99-fbuild-hwci.rules
   SUBSYSTEM=="tty",  ATTRS{idVendor}=="303a", MODE="0666"
   SUBSYSTEM=="tty",  ATTRS{idVendor}=="16c0", MODE="0666"
   SUBSYSTEM=="tty",  ATTRS{idVendor}=="2e8a", MODE="0666"
   SUBSYSTEM=="hidraw", ATTRS{idVendor}=="16c0", MODE="0666"
   SUBSYSTEM=="usb",  ATTRS{idVendor}=="1fc9", MODE="0666"
   ```
   ```bash
   sudo udevadm control --reload-rules
   sudo udevadm trigger
   ```

4. **Plug each board into the runner.** Run
   ```bash
   cargo run --bin fbuild -- serial probe list
   ```
   on the runner host and verify every expected VID:PID is annotated
   with the right board hint. If a board's fingerprint isn't in the
   list, add the row to
   [`tests/hw/fingerprints/<board>.txt`](../../tests/hw/) and to
   `BOARD_FINGERPRINTS`.

5. **Pin a known-good firmware** for each board under
   `tests/hw/known_good_<board>.{bin,elf,uf2}`. The bring-up test
   target stays stable so CI failures are unambiguously regressions
   in `fbuild-serial` / `fbuild-deploy`, not in the firmware payload.
   See [`tests/hw/README.md`](../../tests/hw/README.md) for the
   layout convention.

6. **Trigger the first run manually.**
   ```bash
   gh workflow run hw-ci.yml -f board=all
   ```
   The job should pick up the runner, walk the matrix, and either
   pass on every board (good) or post a comment on a fresh
   `hw-ci-failure` issue with the failure details (also good — that's
   the report path working).

## Failure-path expectations

- **Nightly cron failures** open or update an `hw-ci-failure` issue
  with the run URL and timestamp. One issue per board family is
  the convention.
- **Per-PR failures** (via the `hw-ci` label) post the run URL to
  the PR's check status. They do NOT open a tracker issue — failure
  on a PR is the PR author's signal to fix before merging, not a
  fleet-wide alert.

## See also

- [`tests/hw/README.md`](../../tests/hw/README.md) — fixture layout.
- [`.github/workflows/hw-ci.yml`](../../.github/workflows/hw-ci.yml)
  — workflow source.
- FastLED/fbuild#696 — this scaffold's tracker.
- FastLED/fbuild#586 — LPC845-BRK on-hand burn-down meta.
