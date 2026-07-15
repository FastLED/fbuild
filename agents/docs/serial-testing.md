# Serial detection real-device testing (FastLED/fbuild#899)

## When to reach for this

Use the Docker/WSL real-device harness in `ci/docker-test-serial/`
when you need to validate one of:

- A change to `crates/fbuild-serial/src/port_class.rs` — the OS-side
  kernel-class detection introduced in #895. Sysfs paths can drift
  between kernel versions; real-device validation catches it.
- A new FastLED/boards USB catalogue record or data-driven family-classifier
  change. The disagreement warning shipped in #897 will fire if the catalogue
  identity's implied class disagrees with what the kernel actually binds. Never
  add a production VID/PID literal to the legacy table.
- Anything that touches `SharedSerialManager::open_port`'s DTR/RTS
  handling — getting `(false, false)` vs `(true, true)` wrong on
  attach is the difference between "firmware runs" and "firmware
  gets reset on every connect."

Otherwise, **don't reach for it** — the unit tests in
`port_class::tests` and `boards::tests` cover synthetic cases at sub-
second iteration time. Save the Docker harness for real-hardware
sanity checks.

## How it works (Windows host)

```
Windows host                  WSL2 (Microsoft kernel)            Ubuntu distro
┌──────────────┐              ┌─────────────────────────┐        ┌──────────────┐
│ ESP32-S3 on  │  usbipd      │  vhci_hcd imports USB   │        │ cdc_acm      │
│ COM<N>       │ ───────────► │  device into the VM     │ ─────► │ creates      │
│ usbser.sys   │  attach      │  bus                    │  same  │ /dev/ttyACM0 │
└──────────────┘              └─────────────────────────┘ kernel └──────────────┘
                                                                       │
                                                                       │ /sys/class/tty/
                                                                       │ ttyACM0/device/
                                                                       │ driver -> cdc_acm
                                                                       │
                                                                       ▼
                                                                fbuild's port_class
                                                                Linux detector reads
                                                                this exact symlink.
```

The Linux WSL distro shares the same kernel as `docker-desktop` —
that's just Microsoft's WSL2 kernel build. Docker Desktop's own
distro doesn't have `cdc_acm` userspace tooling (no modprobe, no
udev), so attach there doesn't surface `/dev/ttyACM0`. **Always
attach to a real distro (Ubuntu in this setup).**

## One-time machine setup (admin, ~2 minutes)

```powershell
# Triggers ONE UAC prompt — Yes.
powershell -NoProfile -Command "Start-Process powershell -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\Users\<you>\dev\fbuild\ci\docker-test-serial\setup-wsl-usb.ps1' -Verb RunAs -Wait"
```

The script:
1. Installs `usbipd-win` MSI (signed driver — admin required, fundamental Windows constraint).
2. Binds every `303A:1001` device it finds (admin-only — modifies USB device claim).
3. Installs Ubuntu WSL distro if missing.

After this completes, **every subsequent test run is user-mode**.

## Per-test run (user-mode, no UAC)

```bash
# From any shell, no admin:
bash ci/docker-test-serial/run-test.sh
```

The script:
1. Looks up a `303A:1001 Shared` BUSID via `usbipd list`.
2. `usbipd attach --wsl=Ubuntu --busid X-Y` (user-mode).
3. Ensures `cdc_acm` is loaded in Ubuntu.
4. Builds + runs a small Rust program (the source code is in the
   script as a here-doc) that calls `port_class::detect_port_kernel_class`,
   the catalogue-backed family lookup and `family_for_port` against
   `/dev/ttyACM0`,
   and asserts both signals return `Esp32NativeUsbCdc` / `CdcAcm`.
5. `usbipd detach` (user-mode) — leaves the bind in place for the
   next session.

Expected last line:

```
*** REAL-DEVICE TEST: PASS ***
```

## Gotchas

- **`docker-desktop` is not a target.** Its kernel-attached USB
  enumeration works, but it ships without `cdc_acm`-binding userspace,
  so `/dev/ttyACM0` never appears there. Always target the Ubuntu
  distro for tests.
- **First Rust build is ~4 minutes** because it cross-compiles from
  the Windows-mounted source on `/mnt/c`. The build cache survives
  WSL sessions; subsequent runs are seconds.
- **WSL distros idle-shut-down** after ~60s of inactivity. The
  `run-test.sh` script keeps the distro alive via the build process.
- **`usbipd attach` on Linux host** is slightly different — see
  `usbipd-win`'s docs for native-Linux setup.

## Closed issues this exercises

- [#895](https://github.com/FastLED/fbuild/issues/895) — OS-native
  CDC kernel-class detection. The Linux sysfs read path
  (`/sys/class/tty/<port>/device/driver`) is exactly what
  `linux::detect` does.
- [#897](https://github.com/FastLED/fbuild/issues/897) — disagreement
  warning between VID/PID table and kernel-class signal. The test
  verifies both signals agree for `303A:1001` (the warning does NOT
  fire — correct).
- [#899](https://github.com/FastLED/fbuild/issues/899) — original
  post-mortem on Docker USB passthrough setup, now resolved.

## What to do if the test fails

1. Run the steps from `run-test.sh` interactively in Ubuntu (`wsl -d
   Ubuntu --user root`) so you can poke at intermediate state.
2. Check `dmesg | tail -20` after `usbipd attach` — there should be
   a "cdc_acm 1-1:1.0: ttyACM0: USB ACM device" line.
3. Verify `readlink /sys/class/tty/ttyACM0/device/driver` resolves
   to `.../bus/usb/drivers/cdc_acm`. If it resolves to something
   else, the FastLED/boards record/ingestion or
   `port_class::linux::classify_driver` may need updating. Do not patch in a
   literal VID/PID fallback.
4. Worst case, the kernel driver name changed across mainline Linux
   versions — bump the Microsoft WSL kernel and re-test.
