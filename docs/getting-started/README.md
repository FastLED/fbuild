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

Source installs require a globally installed `soldr` executable and use Rust's
fast dev profile by default (no Rust LTO), which keeps local rebuilds quick.
To explicitly build an optimized Rust wheel, set the build environment flag:

```bash
FBUILD_BUILD_RELEASE=1 pip install .
```

`pip install . --config-settings profile=release` is no longer supported; the
setuptools backend does not interpret that setting. On Windows, set the same
environment variable before invoking pip.

## Windows Long Paths

ESP32 SDKs can exceed Windows' legacy path limits. Enable long paths with:

```powershell
fbuild --fix-win
```

This requests Windows administrator approval and changes only
`HKLM\SYSTEM\CurrentControlSet\Control\FileSystem\LongPathsEnabled` to DWORD `1`.
If it is already enabled, no elevation is requested. The elevated helper exits
without starting a daemon or a build. Interactive ESP32 commands also offer a
`[y/N]` repair prompt; after approval, the pending command resumes in a fresh
process with ordinary privileges. Existing daemon operations finish before
replacement. Help, version, diagnostics, and the repair command remain usable.

When repair is declined, cancelled, blocked by policy, or unavailable in CI,
fbuild selects a short cache and warns on stderr. Terminal warnings are yellow;
redirected output and HTTP responses contain plain text. The warning gives the
effective cache, extraction scratch, build fallback, and repair command.

The automatic location is a private directory directly under the Windows
system drive, for example `C:\fb-<user-home-hash>\prod\{cache,tmp,b}`. Dev mode
uses a separate `dev` directory. fbuild rejects foreign-owned directories and
junctions. If it cannot create this directory, set `FBUILD_CACHE_DIR` to a
short writable location, for example `C:\Users\me\fc`. A suitable explicit
cache uses its `t` and `b` subdirectories for scratch and deep build outputs;
fbuild reports when a deep explicit cache requires an automatic replacement.
`FBUILD_BUILD_DIR` can also shorten build output paths. These environment
variables shorten locations; they do not enable the Windows registry setting.

After enabling long paths, fbuild retains an automatic cache to avoid another
download. An explicit cache change takes effect on the next invocation. Running
operations keep their selected paths; a changed setting or cache causes the
daemon to drain before replacement. A command started during repeated changes
may ask you to retry. Projects, compiler inputs, or archive members that still
exceed legacy limits fail with the offending path and repair guidance. Moving
the project closer to the drive root is another option. Some external tools
retain their own limits even with Windows long paths enabled.

Windows requires both the registry setting and a long-path-aware application;
fbuild's CLI and daemon include that declaration. See Microsoft's
[long-path documentation](https://learn.microsoft.com/en-us/windows/win32/fileio/maximum-file-path-limitation).

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
