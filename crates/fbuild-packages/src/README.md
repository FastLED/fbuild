# Source

## Modules

- **`lib.rs`** -- Crate root; defines `Package`, `Toolchain`, `Framework` traits and `PackageBase` staged-install logic; re-exports `Cache`
- **`cache.rs`** -- Cache directory management with URL stem/hash naming and per-project build directories
- **`downloader.rs`** -- Async HTTP file downloads with SHA256 checksum verification
- **`extractor.rs`** -- Pure-Rust archive extraction for tar.gz, tar.bz2, tar.xz, tar.zst, and zip
- **`library/`** -- Arduino library dependency management (see `library/README.md`)
- **`toolchain/`** -- Platform-specific toolchain packages (see `toolchain/README.md`)
