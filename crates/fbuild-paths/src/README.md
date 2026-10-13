# Source

## Modules

- **`lib.rs`** -- All path resolution functions: dev/prod root, daemon file paths, port resolution, cache dirs, project build dirs, PlatformIO home, and firmware file discovery
- **`long_paths.rs`** -- Windows long-path policy snapshot and validated short cache, extraction scratch and project build fallback. Keeps the selected automatic cache in a durable marker under the normal fbuild configuration root.
