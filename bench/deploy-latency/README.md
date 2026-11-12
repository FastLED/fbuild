# bench/deploy-latency

Flash-path timing and correctness probes against a **real attached board**.
Everything here needs live hardware; there is no CI gate, and that is
deliberate — a deploy measured against no board measures nothing.

This is the manual procedure behind the deploy-latency figures quoted in
issues and PR bodies. Follow it in full: a deploy receipt is not a boot
proof (see "Why the boot check is not optional").

## What it measures

For one firmware image already built by `fbuild`:

1. **Refusal latency** — how long the ESP32 flash-size preflight
   (`crates/fbuild-deploy/src/esp32/flash_size.rs`, FastLED/fbuild#1993)
   takes to reject an image whose declared flash size exceeds the chip's
   detected size. This must fail *before* any write, so the number is a
   preflight cost, not a flash cost.
2. **Deploy latency** — wall-clock of `fbuild deploy` on a size-matched
   image, several trials, one daemon lifetime. `Hash of data verified.`
   must appear for every trial.
3. **Boot proof** — the application actually runs after the deploy.

## Why the boot check is not optional

`upload.flash_size` in a board JSON describes the board *family*, and
esptool writes it into the image header without comparing it to the chip.
On a mismatched part esptool only *warns* and exits 0, so fbuild's verdict
(child exit status + hash) is a **false green**: the image flashes,
verifies, prints `succeeded`, and then the app panics

```
spi_flash: Detected size(4096k) smaller than the size in the binary image header(8192k). Probe failed.
assert failed: __esp_system_init_fn_init_flash startup_funcs.c:118 (flash_ret == ESP_OK)
```

FastLED/fbuild#1992 is that defect; #1993 added the preflight that now
refuses it up front. Always `fbuild reset` + `fbuild serial probe read`
after a deploy receipt and confirm the application prints something. A
sketch that prints nothing proves nothing — see "Sketch" below.

## How to run

Build release binaries once (the harness measures the real CLI, not a
debug shim):

```bash
soldr cargo build --release -p fbuild-cli --bin fbuild -p fbuild-daemon --bin fbuild-daemon
F=$PWD/target/release/fbuild
```

Point `platformio.ini` at the attached part. Two environments, one that
deliberately over-declares the flash size and one that matches it:

```ini
[platformio]
src_dir = src
build_flags = -DARDUINO_USB_CDC_ON_BOOT=1 -DARDUINO_USB_MODE=1

[env:esp32c6_8mb]
platform = https://github.com/pioarduino/platform-espressif32/releases/download/55.03.312/platform-espressif32.zip
board = esp32-c6-devkitc-1
framework = arduino
build_flags = ${platformio.build_flags}

[env:esp32c6_4mb]
platform = https://github.com/pioarduino/platform-espressif32/releases/download/55.03.312/platform-espressif32.zip
board = esp32-c6-devkitc-1
board_upload.flash_size = 4MB
framework = arduino
build_flags = ${platformio.build_flags}
```

`ARDUINO_USB_CDC_ON_BOOT` / `ARDUINO_USB_MODE` are required for `Serial`
to reach USB-CDC on this core; without them a `Serial.println` sketch
looks dead.

Refusal case — expect exit 1, roughly two seconds, and no write:

```bash
$F build  -e esp32c6_8mb
$F deploy -e esp32c6_8mb -p "ser=<usb-serial>"
```

Matched case — three trials, then the boot proof:

```bash
$F build  -e esp32c6_4mb
for i in 1 2 3; do
  /usr/bin/time -f "trial$i elapsed=%e" $F deploy -e esp32c6_4mb -p "ser=<usb-serial>"
done
$F reset -p "ser=<usb-serial>"
$F serial probe read /dev/ttyACM0 --seconds 6
```

Use `ser=` rather than a port name where possible: the USB-JTAG endpoint
renumbers (`ACM0` -> `ACM1`) across a flash.

## Sketch

Minimal, and it prints at both moments a boot can fail — after flash init
and then once per second:

```cpp
#include <Arduino.h>

void setup() {
    Serial.begin(115200);
    delay(200);
    Serial.println("fbuild-boot-proof up");
}

void loop() {
    Serial.println("fbuild-boot-proof tick");
    delay(1000);
}
```

## Recorded run

Captured 2026-11-11 on `origin/main` `aaa184a1` (release binaries from
that commit), ESP32-C6 FH4 (4 MB) on `/dev/ttyACM0`, USB serial
`8C:BF:EA:CF:87:B4`, firmware `firmware.bin` 277,186 bytes, host load
7.80/6.76/5.80 on 16 cores. The purpose was re-verifying #1993 after its
merge, not tuning anything:

| case | env | exit | elapsed | notes |
|---|---|---|---|---|
| refusal | `esp32c6_8mb` | 1 | 1.97 s | names `board_upload.flash_size = 4MB`; nothing written |
| deploy | `esp32c6_4mb` | 0 | 5.40 s | 3x `Hash of data verified.`, no esptool warning |
| deploy | `esp32c6_4mb` | 0 | 5.38 s | same |
| deploy | `esp32c6_4mb` | 0 | 5.32 s | same |

Boot proof: `fbuild-boot-proof up` then 10 `fbuild-boot-proof tick` lines
inside a 6 s read window. Median deploy 5.38 s, consistent with the
4.5-5.1 s measured the same day on a busier host, so the preflight's
`esptool flash-id` probe costs well under a second.

Timings here are serial-bound and far less load-sensitive than the
resolver matrix in [`../fastled-examples/`](../fastled-examples/README.md),
but they still drift with the host and the image size — compare against a
same-day run, never against an archived table.
