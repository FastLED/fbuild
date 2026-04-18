# Why fbuild?

## Key Benefits

### Transparency
Direct URLs and hash-based caching mean you know exactly what you're downloading. No hidden package registries or opaque dependency resolution.

### Reliability
Real downloads with checksum verification ensure consistent, reproducible builds. No mocks in production code.

### Speed
Optimized incremental builds complete in under 1 second, with intelligent caching for full rebuilds in 2-5 seconds.

### Code Quality
100% type-safe (mypy), PEP 8 compliant, and comprehensive test coverage ensure a maintainable and reliable codebase.

### Clear Error Messages
Actionable error messages with suggestions help you quickly identify and fix issues without requiring forum searches.

## Performance

**Benchmarks** (Arduino Uno Blink sketch):

| Build Type | Time | Description |
|------------|------|-------------|
| First build | 19.25s | Includes toolchain download (50MB) |
| Full build | 3.06s | All packages cached |
| Incremental | 0.76s | No source changes |
| Clean build | 2.58s | Rebuild from cache |

**Firmware Size** (Blink):
- Program: 1,058 bytes (3.3% of 32KB flash)
- RAM: 9 bytes (0.4% of 2KB RAM)

## Why fbuild exists

Both the Arduino CLI and PlatformIO build chains have lagged in their development. While PlatformIO represented a meaningful improvement over the original Arduino build system, it continues to exhibit significant architectural and reliability issues that have remained unresolved despite repeated community reports.

One persistent problem is PlatformIO's tendency to corrupt its own global installation state. In practice, this often requires users to manually delete `~/.platformio/packages` to restore functionality. This behavior is particularly harmful to new developers, as the failure mode is opaque and recovery is undocumented. In addition, PlatformIO's package management is frequently slow and unreliable: large toolchains for modern targets (e.g., ESP, STM, Raspberry Pi-class boards) are regularly invalidated and re-downloaded in full, sometimes consuming multiple gigabytes of bandwidth. Even trivial changes to `platformio.ini`—including non-functional edits such as comments—can trigger a full revalidation and reinstall cycle, especially when using the VS Code extension with autosave enabled. This makes the development experience unpredictable and fragile.

More critically, both Arduino CLI and PlatformIO fail to reliably enable essential compiler and linker features for embedded systems, most notably `--gc-sections` and link-time optimization (LTO). These features are fundamental for producing minimal binaries on memory-constrained devices, as they allow dead code elimination and cross–translation unit optimization. Their absence leads to substantial binary bloat. For FastLED, this limitation persisted for years and forced the project to rely on aggressive inlining strategies as a workaround—an approach that increases compile times and code complexity while still falling short of what proper LTO provides.

Compounding these technical issues, conflicts between PlatformIO and Espressif resulted in incomplete or delayed support for newer ESP targets. Boards such as the ESP32-C2, C5, and C6 required external workaround repositories to function correctly with the IDF v5 toolchain, despite being officially supported by the vendor. This further increased maintenance burden and slowed development.

Collectively, these issues cost the FastLED project months of developer time. PlatformIO serves as FastLED's testing infrastructure, and repeated build failures, slow installs, and corrupted environments significantly reduced iteration speed. To enable concurrent builds and isolate failures, FastLED ultimately resorted to encapsulating the entire build chain inside Docker containers—solely to sandbox PlatformIO's global state and avoid cross-contamination between builds.

FastLED attempted to address these shortcomings through feature requests and pull requests to PlatformIO; all were declined. Meanwhile, emerging low-cost, high-capability platforms—such as CH-series RISC-V microcontrollers, which are cheaper and more powerful than legacy ATtiny-class devices—remain effectively inaccessible under these legacy build systems.

Given this landscape, the cost to FastLED developers became untenable. It proved more efficient to rebuild the entire compile and deployment stack from first principles. The result is **fbuild**, the FastLED build system. With fbuild, builds are deterministic, fast, and scalable, and advanced compiler and linker features such as LTO can finally be enabled—both for modern targets and retroactively for legacy platforms where the tooling has supported them in theory for over a decade but failed to use them in practice.

## Design Goals

- Replaces `platformio` in `FastLED` repo builders
- Correct and blazing parallel package management system
  - locking is done through a daemon process
  - packages are fingerprinted to their version and cached, download only once
  - zccache for caching compiles
- Easily add features via AI
  - This codebase is designed and implemented by AI; fork it and ask AI to make your change. Please send us a PR!
- Supports new build chains easily
- Supports wasm builds natively

## See also

- [DESIGN_DECISIONS.md](DESIGN_DECISIONS.md) — ADR-style decisions with rationale
- [architecture/overview.md](architecture/overview.md) — system architecture
- [ROADMAP.md](ROADMAP.md) — implementation phases
