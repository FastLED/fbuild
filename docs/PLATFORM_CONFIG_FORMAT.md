# Platform Configuration File Format

## Overview

fbuild uses a configuration-driven build system where platform-specific compiler and linker settings are defined in JSON configuration files rather than hard-coded in Python classes. This approach enables easy support for new platforms and MCU variants without modifying the core build system.

## File Location

Platform configuration files are stored in:
```
src/fbuild/platform_configs/{mcu}.json
```

Where `{mcu}` is the MCU identifier from the board's JSON configuration (e.g., `esp32`, `esp32c3`, `esp32c6`, `esp32s3`).

## JSON Structure

### Root Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Human-readable platform name |
| `description` | string | Yes | Description of the platform configuration |
| `mcu` | string | Yes | MCU identifier (must match filename) |
| `architecture` | string | Yes | CPU architecture (e.g., `riscv32`, `xtensa-esp32`) |
| `compiler_flags` | object | Yes | Compiler flag configuration |
| `linker_flags` | array | Yes | Linker flags and options |
| `linker_scripts` | array | Yes | Linker script filenames |
| `defines` | array | Yes | Preprocessor defines |

### Compiler Flags Object

The `compiler_flags` object contains three arrays:

```json
{
  "compiler_flags": {
    "common": [...],  // Flags applied to both C and C++ compilation
    "c": [...],       // C-specific flags (combined with common)
    "cxx": [...]      // C++-specific flags (combined with common)
  }
}
```

**Common flags** apply to all compilation units and typically include:
- Optimization level (`-Os`, `-O2`, etc.)
- Section management (`-ffunction-sections`, `-fdata-sections`)
- Warning controls (`-Wno-error=...`)
- Architecture-specific options (`-march=...`, `-mlongcalls`)

**C-specific flags** apply only to `.c` files:
- C standard (`-std=gnu17`)
- C-specific warnings (`-Wno-old-style-declaration`)

**C++-specific flags** apply only to `.cpp`, `.cxx`, `.cc` files:
- C++ standard (`-std=gnu++2b`)
- Exception handling (`-fexceptions`)
- RTTI settings (`-fno-rtti`)

### Linker Flags Array

The `linker_flags` array contains all linker command-line options:

```json
{
  "linker_flags": [
    "-nostartfiles",
    "-march=rv32imac_zicsr_zifencei",
    "-Wl,--cref",
    "-Wl,--gc-sections",
    "-u", "start_app",
    ...
  ]
}
```

Common linker flags include:
- Architecture flags (must match compiler)
- Section management (`-Wl,--gc-sections`)
- Undefined symbol references (`-u symbol_name`)
- Custom linker options (`-Wl,--option`)

### Linker Scripts Array

The `linker_scripts` array lists linker script filenames in the order they should be applied:

```json
{
  "linker_scripts": [
    "memory.ld",
    "sections.ld",
    "esp32c6.rom.ld",
    ...
  ]
}
```

**Important**:
- Script filenames only (not full paths)
- Scripts are resolved from the SDK's `{mcu}/ld/` directory
- Order matters - scripts are processed in array order
- Common scripts:
  - `memory.ld` - Memory region definitions
  - `sections.ld` - Section placement
  - `*.rom.ld` - ROM function bindings

### Defines Array

The `defines` array contains preprocessor definitions in two formats:

```json
{
  "defines": [
    "SIMPLE_DEFINE",                    // -DSIMPLE_DEFINE
    ["KEY", "\"value\""],               // -DKEY="value"
    ["IDF_VER", "\"v5.5.1\""],         // -DIDF_VER="v5.5.1"
    ["ESP32", "ESP32"]                  // -DESP32=ESP32
  ]
}
```

**Formats**:
1. **Simple string**: Defines a symbol without a value
2. **[key, value] array**: Defines a symbol with a value
   - Use escaped quotes `\"` for string values
   - Numeric values don't need quotes

## Complete Example: ESP32-C6

```json
{
  "name": "ESP32-C6",
  "description": "Configuration for ESP32-C6 MCU extracted from PlatformIO",
  "mcu": "esp32c6",
  "architecture": "riscv32",

  "compiler_flags": {
    "common": [
      "-Os",
      "-ffunction-sections",
      "-fdata-sections",
      "-Wno-error=unused-function",
      "-MMD"
    ],
    "c": [
      "-march=rv32imac_zicsr_zifencei",
      "-std=gnu17"
    ],
    "cxx": [
      "-march=rv32imac_zicsr_zifencei",
      "-std=gnu++2b",
      "-fexceptions",
      "-fno-rtti"
    ]
  },

  "linker_flags": [
    "-nostartfiles",
    "-march=rv32imac_zicsr_zifencei",
    "-Wl,--gc-sections",
    "-u", "start_app"
  ],

  "linker_scripts": [
    "memory.ld",
    "sections.ld",
    "esp32c6.rom.ld"
  ],

  "defines": [
    "ESP_PLATFORM",
    ["IDF_VER", "\"v5.5.1-710-g8410210c9a\""],
    "ARDUINO_ARCH_ESP32"
  ]
}
```

## Architecture-Specific Notes

### RISC-V Platforms (ESP32-C3, ESP32-C6, ESP32-H2)

- Use `-march=rv32...` flags to specify ISA extensions
- Common: `rv32imc_zicsr_zifencei` (32-bit, integer, multiply, compressed, CSR, fence)
- No `-mlongcalls` needed (RISC-V has PC-relative addressing)

### Xtensa Platforms (ESP32, ESP32-S2, ESP32-S3)

- Use `-mlongcalls` for extended call range (programs > 256KB)
- Use `-mdisable-hardware-atomics` if required
- Architecture-specific target (`xtensa-esp32`, `xtensa-esp32s3`)

## Creating a New Platform Configuration

1. **Identify the MCU**: Determine the MCU identifier (e.g., `esp32c3`)

2. **Extract PlatformIO config**: Look for `pioarduino-build.py` in the platform SDK:
   ```
   .fbuild/cache/platforms/*/tools/sdk/{mcu}/pioarduino-build.py
   ```

3. **Map PlatformIO to JSON**:
   - `CCFLAGS` → `compiler_flags.common`
   - `CFLAGS` → `compiler_flags.c`
   - `CXXFLAGS` → `compiler_flags.cxx`
   - `LINKFLAGS` → `linker_flags`
   - `CPPDEFINES` → `defines`

4. **Extract linker scripts**: List `.ld` files referenced in linker flags or found in SDK's `{mcu}/ld/` directory

5. **Test the configuration**: Run a build and verify compilation and linking succeed

6. **Save the file**: Save as `src/fbuild/platform_configs/{mcu}.json`

## Usage in Code

The configuration files are automatically loaded by `ConfigurableCompiler` and `ConfigurableLinker`:

```python
from fbuild.build.configurable_compiler import ConfigurableCompiler
from fbuild.build.configurable_linker import ConfigurableLinker

# Compiler automatically loads config based on MCU
compiler = ConfigurableCompiler(
    platform,
    toolchain,
    framework,
    board_id,
    build_dir,
    platform_config=None  # Auto-loads from platform_configs/{mcu}.json
)

# Linker automatically loads config based on MCU
linker = ConfigurableLinker(
    platform,
    toolchain,
    framework,
    board_id,
    build_dir,
    platform_config=None  # Auto-loads from platform_configs/{mcu}.json
)
```

You can also provide a custom config path or dict:

```python
# Load from custom path
compiler = ConfigurableCompiler(..., platform_config=Path("my_config.json"))

# Use in-memory config
config_dict = {...}
compiler = ConfigurableCompiler(..., platform_config=config_dict)
```

## Benefits of Configuration-Driven Approach

1. **No code changes needed**: Add new platforms by creating JSON files
2. **Easy to maintain**: Configuration is separate from logic
3. **Transparent**: Build flags are visible and auditable
4. **Portable**: Configs can be shared and versioned independently
5. **PlatformIO compatible**: Easy to extract from existing PlatformIO builds

## Migration from Platform-Specific Classes

Old approach (ESP32Compiler, ESP32Linker):
```python
from fbuild.build.esp32_compiler import ESP32Compiler
from fbuild.build.esp32_linker import ESP32Linker

compiler = ESP32Compiler(...)  # Hard-coded ESP32 logic
linker = ESP32Linker(...)      # Hard-coded ESP32 logic
```

New approach (ConfigurableCompiler, ConfigurableLinker):
```python
from fbuild.build.configurable_compiler import ConfigurableCompiler
from fbuild.build.configurable_linker import ConfigurableLinker

compiler = ConfigurableCompiler(...)  # Loads config from JSON
linker = ConfigurableLinker(...)      # Loads config from JSON
```

The interfaces are identical, making it a drop-in replacement.
