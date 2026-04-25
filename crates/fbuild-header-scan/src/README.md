# fbuild-header-scan sources

- `lib.rs` — public re-exports and `SCANNER_VERSION`.
- `scanner.rs` — line-oriented tokenizer that extracts `#include` directives.
- `walker.rs` — BFS over the include graph with quoted-first resolution.
