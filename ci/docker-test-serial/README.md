# Docker test rig for fbuild serial detection (FastLED/fbuild#899)

End-to-end test of fbuild's serial-classification code (PR #896 / #898)
against a **real USB device** that's been passed through into a Docker
container. This exercises the Linux sysfs read path
(`crates/fbuild-serial/src/port_class.rs::linux::detect`) against
actual hardware, not just synthetic temp-dir fixtures.

## Architecture

```
Windows host                WSL2 (Docker Desktop kernel)        Docker container
┌──────────────┐            ┌─────────────────────────┐         ┌──────────────────┐
│ ESP32 plugged│  usbipd    │  /dev/ttyACM0           │ --device│  /dev/ttyACM0    │
│ via USB-C    │ ─────────► │  /sys/class/tty/...     │ ───────►│  fbuild + tests  │
│ usbser.sys   │  attach    │  cdc_acm.ko bound       │         │                  │
└──────────────┘            └─────────────────────────┘         └──────────────────┘
```

## One-time setup (admin required exactly once per machine)

```powershell
# In an elevated PowerShell (one UAC prompt):
Start-Process powershell.exe -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\tmp\usbipd-install\install-and-bind.ps1' -Verb RunAs -Wait
```

That script:
- Installs the `usbipd-win` MSI (signed driver registration).
- Lists USB devices.
- Binds every `303A:1001` (Espressif native USB CDC) device it finds
  — the "bind" step is the privileged operation that takes ownership
  of the device's class driver claim.

After that completes, **everything below is user-mode** — no further
admin prompts ever.

## Per-session attach (user-mode)

```powershell
# Pick a BUSID from `usbipd list` (look for 303a:1001):
usbipd attach --wsl --busid 4-2
```

Verify in WSL or a Docker container:

```bash
ls /dev/ttyACM*       # should now show /dev/ttyACM0
```

## Build the test image

```bash
cd C:\Users\niteris\dev\fbuild
docker build -f ci/docker-test-serial/Dockerfile -t fbuild-test-serial .
```

Build is ~10s after the first time (no Rust toolchain in the image —
source is bind-mounted at runtime). Image is ~50 MB.

## Run the test

```bash
docker run --rm \
  --device=/dev/ttyACM0 \
  -v "$PWD:/work" \
  -w /work \
  fbuild-test-serial \
  bash ci/docker-test-serial/test.sh /dev/ttyACM0
```

The script:

1. Confirms `/dev/ttyACM0` exists inside the container.
2. Runs `lsusb -d 303a:1001` so the test log shows the device descriptor.
3. **Exercises the exact sysfs path fbuild's Linux detector reads** —
   `/sys/class/tty/<port>/device/driver` — and reports the driver
   symlink target. Expected: `cdc_acm`.
4. Builds fbuild from the mounted source (skipped if `target/debug/fbuild`
   already exists).
5. Runs `fbuild serial probe list` so the daemon-side classification
   output appears in the log.
6. Asserts the new code path: kernel-class is `CdcAcm`, VID/PID family
   is `Esp32NativeUsbCdc`, signals agree → no #897 disagreement warning.

## When to use this

- **After modifying `crates/fbuild-serial/src/port_class.rs`** — run
  this to validate the change against a real `cdc_acm`-bound device.
- **After modifying the VID/PID table** in
  `crates/fbuild-serial/src/boards.rs` — confirms a new entry's
  classification agrees with what the kernel actually binds.
- **Before merging any PR that touches serial attach DTR/RTS** — the
  test confirms a running ESP32 firmware isn't reset on attach.

## When NOT to use this

- For unit-level changes to logic that doesn't touch the runtime
  serial path. The unit-test suite (`port_class::tests::*` and
  `boards::tests::*`) covers the synthetic cases. Save the Docker
  test for real-hardware sanity checks.

## Agent guidance

Future agent sessions touching serial code SHOULD:

1. Read `agents/docs/serial-testing.md` (when added — see #899 TBD).
2. Verify changes in unit tests first (fast feedback).
3. Only spin up this Docker harness when real-hardware validation
   matters (idle DTR/RTS semantics, driver-binding behavior changes).
4. Detach with `usbipd detach --busid X-Y` (user-mode) when done so
   the device returns to the Windows host's normal claim.
