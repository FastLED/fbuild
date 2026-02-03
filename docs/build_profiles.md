# Build Profiles

fbuild supports two build profiles that control optimization levels and Link-Time Optimization (LTO):

| Profile | Flag | Optimization | LTO | Use Case |
|---------|------|--------------|-----|----------|
| **release** (default) | `--release` | `-Os` | Enabled | Production builds, smallest binaries |
| **quick** | `--quick` | `-O2` | Disabled | Fast iteration during development |

## Usage

```bash
# Default: release build with LTO (smallest binary, slowest compile)
fbuild build tests/uno -e uno

# Explicit release build
fbuild build tests/uno -e uno --release

# Quick build (faster compile, no LTO)
fbuild build tests/uno -e uno --quick

# Quick build with parallel compilation (fastest development iteration)
fbuild build tests/uno -e uno --quick --jobs 4
```

## Profile Details

### Release Profile (Default)

The release profile produces the smallest possible binaries by enabling:

- **Optimization**: `-Os` (optimize for size, matching Arduino/PlatformIO defaults)
- **LTO Compile Flags**: `-flto -fno-fat-lto-objects`
- **LTO Link Flags**: `-flto -fuse-linker-plugin`
- **Section Flags**: `-ffunction-sections -fdata-sections`
- **Link GC**: `-Wl,--gc-sections`
- **Archive Linking**: `--whole-archive` (required for LTO visibility)

LTO (Link-Time Optimization) allows the compiler to perform whole-program optimization across translation units, enabling:
- Cross-TU inlining
- Aggressive dead code elimination
- Better register allocation

### Quick Profile

The quick profile prioritizes fast compile times for development iteration:

- **Optimization**: `-O2` (balanced speed/size)
- **LTO**: Disabled (no `-flto` flags)
- **Section Flags**: `-ffunction-sections -fdata-sections`
- **Link GC**: `-Wl,--gc-sections`
- **Archive Linking**: `--whole-archive` (same as release for consistency)

Without LTO, the compiler processes each source file independently, resulting in faster compilation but slightly larger binaries.

## How Dead Code Elimination Works

Both profiles use section-level garbage collection to remove unused code:

### Compilation Phase

With `-ffunction-sections -fdata-sections`, each function and data item is placed in its own section:

```
// Without section flags:
.text: func1, func2, func3    <- all in one section

// With section flags:
.text.func1: func1            <- separate sections
.text.func2: func2
.text.func3: func3
```

### Linking Phase

With `-Wl,--gc-sections`, the linker performs mark-and-sweep:

1. Start from entry points (main, interrupt vectors, .init_array)
2. Mark all reachable sections
3. Remove unmarked sections

### LTO vs Section-Level GC

| Aspect | LTO (release) | Section-Level GC (quick) |
|--------|---------------|--------------------------|
| Scope | Cross-translation-unit | Single TU |
| Inlining | Across all TUs | Within TU only |
| Dead code detection | Whole program | Section-level |
| Compile time | Slower | Faster |
| Binary size | Smallest | Slightly larger |

## Output Directory Separation

Build artifacts are stored in separate directories by profile:

```
.fbuild/
└── build/
    └── uno/
        ├── release/    <- Release profile artifacts
        │   ├── core/
        │   ├── src/
        │   └── firmware.hex
        └── quick/      <- Quick profile artifacts
            ├── core/
            ├── src/
            └── firmware.hex
```

This ensures:
- Switching profiles doesn't invalidate cached artifacts
- Both profiles can be built without full recompilation
- Easy comparison of binary sizes between profiles

## Platform Support

Build profiles are supported on all platforms:

| Platform | LTO Support | Notes |
|----------|-------------|-------|
| AVR | Yes | GCC LTO via avr-gcc |
| ESP32 | Yes | GCC LTO via xtensa/riscv-esp-elf-gcc |
| Teensy | Yes | GCC LTO via arm-none-eabi-gcc |
| RP2040 | Yes | GCC LTO via arm-none-eabi-gcc |
| STM32 | Yes | GCC LTO via arm-none-eabi-gcc |

## Build Banner

At the start of each build, fbuild displays the active profile:

```
PROFILE=release OPT=-Os LTO=on GC=lto-dce COMPILER=avr-g++ (7.3.0)
PROFILE=quick OPT=-O2 LTO=off GC=section-level COMPILER=avr-g++ (7.3.0)
```

## Implementation Details

Profile configuration is defined in `src/fbuild/build/build_profiles.py`:

```python
PROFILES = {
    "release": ProfileFlags(
        name="release",
        description="Optimized release build with LTO (default)",
        compile_flags=("-Os", "-ffunction-sections", "-fdata-sections", "-flto", "-fno-fat-lto-objects"),
        link_flags=("-Wl,--gc-sections", "-flto", "-fuse-linker-plugin"),
        controlled_patterns=("-O", "-flto", "-fno-fat-lto-objects", "-fuse-linker-plugin", "-ffunction-sections", "-fdata-sections", "-Wl,--gc-sections"),
    ),
    "quick": ProfileFlags(
        name="quick",
        description="Fast development build (no LTO)",
        compile_flags=("-O2", "-ffunction-sections", "-fdata-sections"),
        link_flags=("-Wl,--gc-sections",),
        controlled_patterns=("-O", "-flto", "-fno-fat-lto-objects", "-fuse-linker-plugin", "-ffunction-sections", "-fdata-sections", "-Wl,--gc-sections"),
    ),
}
```

The system is profile-agnostic - profiles declare all their flags explicitly and fbuild doesn't interpret them. The flag builder (`flag_builder.py`) and linkers (`linker.py`, `configurable_linker.py`) use these profiles to:

1. Filter out controlled flags from platform configs (using `controlled_patterns`)
2. Apply profile-specific `compile_flags` and `link_flags`
3. Always use `--whole-archive` for library archives
