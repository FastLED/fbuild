---
name: board-support
description: Diagnose and fix board definition issues. Use when a board is missing, misconfigured, has wrong build flags, or when comparing fbuild's board database against external sources (Arduino, Zephyr, PlatformIO).
allowed-tools: Bash Read Grep Glob WebFetch Agent
---

# Board Support Lookup & Diagnosis

You are diagnosing a board definition issue in fbuild. fbuild maintains its own board database (1609+ boards as JSON files) derived primarily from PlatformIO, but boards also exist in Arduino, Zephyr, and other ecosystems that we may need to ingest.

## fbuild Board Database

- **Board JSONs**: `crates/fbuild-config/assets/boards/json/{board_id}.json`
- **Manifest**: `crates/fbuild-config/assets/boards/manifest.json`
- **Board config code**: `crates/fbuild-config/src/board.rs`
- **Enrichment tool**: `crates/fbuild-config/src/bin/enrich_boards.rs`
- **Validation tool**: `ci/validate_boards.py`
- **External source comparison**: `ci/board_sources.py`

## Step 1: Check if the board exists in fbuild

```bash
ls crates/fbuild-config/assets/boards/json/{BOARD_ID}.json
```

If it exists, read it and check for missing/wrong fields (build.core, build.variant, build.mcu, build.extra_flags, upload.protocol, upload.speed, etc.)

## Step 2: Check PlatformIO source (local)

```bash
ls ~/.platformio/platforms/*/boards/{BOARD_ID}.json
```

Compare against our copy. Run validation:
```bash
uv run python ci/validate_boards.py --platforms {PLATFORM}
```

## Step 3: Search external board registries

Use `ci/board_sources.py` to fetch and search external board lists:

```bash
# List all boards from Arduino package indices
uv run python ci/board_sources.py --list-arduino

# List all boards from Zephyr
uv run python ci/board_sources.py --list-zephyr

# Search for a specific board across all sources
uv run python ci/board_sources.py --search {BOARD_NAME}

# Full comparison: find boards in external sources missing from fbuild
uv run python ci/board_sources.py --compare
```

## Step 4: External source URLs (for manual lookup)

### Arduino Package Index Files
These are JSON files listing board support packages. Fetch and inspect them:

| Source | URL |
|--------|-----|
| **Arduino Official** | `https://downloads.arduino.cc/packages/package_index.json` |
| **ESP32 (Espressif)** | `https://raw.githubusercontent.com/espressif/arduino-esp32/gh-pages/package_esp32_index.json` |
| **ESP8266** | `https://arduino.esp8266.com/stable/package_esp8266com_index.json` |
| **Adafruit** | `https://adafruit.github.io/arduino-board-index/package_adafruit_index.json` |
| **SparkFun** | `https://raw.githubusercontent.com/sparkfun/Arduino_Boards/main/IDE_Board_Manager/package_sparkfun_index.json` |
| **Teensy** | `https://www.pjrc.com/teensy/package_teensy_index.json` |

### CH32V (WCH RISC-V) Platform
The CH32V family (CH32V003, V103, V203, V208, V303, V307, CH32X035) uses the community PlatformIO platform:

| Resource | URL |
|----------|-----|
| **PlatformIO platform** | `https://github.com/Community-PIO-CH32V/platform-ch32v` |
| **Board definitions** | `https://github.com/Community-PIO-CH32V/platform-ch32v/tree/develop/boards` |
| **WCH Arduino core** | `https://github.com/openwch/arduino_core_ch32` |
| **WCH product pages** | `http://www.wch-ic.com/products/CH32V003.html` (replace series in URL) |

All CH32V boards use `platform = ch32v` in platformio.ini. Arduino framework is available for V003, V203, and V307 series. Other series use `noneos-sdk` or `ch32v003fun` frameworks.

Arduino package index JSON structure:
```
packages[].platforms[].boards[].name  — board display name
packages[].platforms[].architecture   — e.g. "esp32", "avr", "samd"
packages[].name                       — vendor/packager name
```

### Zephyr Board Definitions
Zephyr boards are defined as YAML + Devicetree in the Zephyr repo:

| Resource | URL |
|----------|-----|
| **Board index** | `https://raw.githubusercontent.com/zephyrproject-rtos/zephyr/main/boards/index.rst` |
| **Board directories** | `https://api.github.com/repos/zephyrproject-rtos/zephyr/contents/boards` |

Zephyr board.yml structure:
```yaml
board:
  name: <board_name>
  vendor: <vendor>
  socs:
    - name: <soc_name>
```

### Other Sources (for deep investigation)

| Source | Format | URL / Repo |
|--------|--------|------------|
| **PlatformIO Registry API** | JSON | `https://api.registry.platformio.org/v3/packages` |
| **CMSIS-Pack Index** | XML | `https://www.keil.com/pack/index.pidx` |
| **Mbed OS targets** | JSON | `https://raw.githubusercontent.com/ARMmbed/mbed-os/master/targets/targets.json` |
| **probe-rs targets** | YAML | `https://github.com/probe-rs/probe-rs/tree/master/probe-rs/targets` |
| **STM32CubeMX DB** | XML | `https://github.com/STMicroelectronics/STM32_open_pin_data` |
| **CircuitPython** | Mixed | `https://github.com/adafruit/circuitpython/tree/main/ports` |

## Step 5: Fix the board

If the board is **missing entirely**:
1. Find its definition in PlatformIO or Arduino sources
2. Create `crates/fbuild-config/assets/boards/json/{board_id}.json` with the standard fields
3. Update `manifest.json` (sorted alphabetically)
4. Run enrichment: `uv run cargo run -p fbuild-config --bin enrich_boards`

If the board has **wrong fields**:
1. Compare against PlatformIO source: `uv run python ci/validate_boards.py`
2. Fix the JSON directly or re-run enrichment
3. Run tests: `uv run test -p fbuild-config`

If the board is **in Arduino/Zephyr but not PlatformIO**:
1. Note this in the issue — it may need manual creation
2. Extract fields from the Arduino boards.txt or Zephyr board.yml
3. Map to fbuild's JSON schema (see esp32dev.json as a reference)
