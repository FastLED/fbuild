# Getting Started

Use this guide when you want to install fbuild, compile a first sketch, and
understand what happens on the first run.

## Install

Install the published Python package:

```bash
pip install fbuild
```

Or install from a local checkout:

```bash
git clone https://github.com/fastled/fbuild.git
cd fbuild
pip install -e .
```

## First Project

Create a project with a PlatformIO-compatible layout:

```bash
mkdir my-project
cd my-project
mkdir src
```

Create `platformio.ini`:

```ini
[env:uno]
platform = atmelavr
board = uno
framework = arduino
```

Create `src/main.ino`:

```cpp
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
}

void loop() {
  digitalWrite(LED_BUILTIN, HIGH);
  delay(1000);
  digitalWrite(LED_BUILTIN, LOW);
  delay(1000);
}
```

Build it:

```bash
fbuild build
```

The first build downloads the AVR-GCC toolchain and Arduino AVR core, then
caches them. Later builds reuse the cache and write firmware under
`.fbuild/build/<env>/`.

## First Deploy And Monitor

Deploy to a connected board:

```bash
fbuild deploy -e uno
```

Deploy and attach the serial monitor:

```bash
fbuild deploy -e uno --monitor
```

Run the monitor by itself:

```bash
fbuild monitor -e uno --timeout 60
```

Serial monitoring uses pyserial and follows the same general port-selection
model as PlatformIO. You can pass `--port COM3` or `--port /dev/ttyUSB0` when
auto-detection is not enough.

## First Emulator Run

Run a build in the default emulator for the board:

```bash
fbuild test-emu . -e uno --timeout 10
```

See the [emulator testing guide](../guides/emulator-testing.md) for backend
selection, QEMU notes, and CI-friendly halt conditions.

## Next Steps

- CLI commands and options: [reference/cli.md](../reference/cli.md)
- `platformio.ini` keys: [reference/platformio-ini.md](../reference/platformio-ini.md)
- Supported boards: [BOARD_STATUS.md](../BOARD_STATUS.md)
- Troubleshooting: [development/README.md](../development/README.md)
