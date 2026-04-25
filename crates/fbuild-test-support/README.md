# fbuild-test-support

Test utilities and fixtures for fbuild workspace crates.

## Key Functions

- `create_test_project(env_name, platform, board)` -- Creates a `tempfile::TempDir` containing a minimal `platformio.ini`, `src/` directory, and `src/main.cpp` with Arduino stubs (`setup`/`loop`)

## CompileDb

`CompileDb` parses clangd-style `compile_commands.json` files for use in
acceptance tests that need to assert properties of a build's translation
units (TU count, presence of files under specific subtrees, etc.). The
spec is documented at
<https://clang.llvm.org/docs/JSONCompilationDatabase.html>.

```rust
use fbuild_test_support::CompileDb;

let db = CompileDb::from_path(".fbuild/compile_commands.json")?;

// Bound TU count (acceptance probes A-20..A-22 for issue #205).
assert!(db.tu_count() <= 250);

// Assert no compile-DB entries point inside forbidden subtrees.
let leaks = db.forbidden_present(&["FNET", "Snooze", "RadioHead", "mbedtls"]);
assert!(leaks.is_empty(), "unexpected libraries compiled: {leaks:?}");

// Drill into matching entries when an assertion fails.
for e in db.entries_matching("FNET") {
    eprintln!("FNET TU still in DB: {}", e.file.display());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Both forms of the spec are accepted:

- `"arguments": [...]` -- taken verbatim.
- `"command": "..."` -- tokenized via the `shell-words` crate (POSIX `sh`
  rules: single quotes, double quotes, backslash escapes).

When both fields are present, `arguments` wins. Relative `file` and
`output` paths are joined onto `directory` (no canonicalization, since
the source files may not exist on test runners).

## MiniFramework

`MiniFramework` is a fluent builder that materializes a fake Teensyduino /
STM32duino / Arduino framework tree under a fresh `tempfile::TempDir`. The
on-disk layout matches what
`fbuild_packages::library::framework_library::discover_framework_libraries`
expects, so anything you build with `MiniFramework` round-trips through the
production walker and the LDF-style resolver in `fbuild-library-select`.

Layout:

```text
<tmp>/framework/
  libraries/
    SPI/src/SPI.h
<tmp>/project/
  src/
  include/   (created on demand)
```

### API

- `MiniFramework::new()` — create the tree.
- `add_library(name)` — eagerly creates `libraries/<name>/src/<name>.h`
  (empty by default) and returns a `LibraryBuilder`.
- `LibraryBuilder` chain: `.header(s)`, `.cpp(s)`, `.extra(rel, s)`,
  `.example(rel, s)`, `.extras(rel, s)`, `.tests(rel, s)`, `.done()`.
- `add_project_source(rel, s)` / `add_project_include(rel, s)` /
  `sketch(s)` — write project files under `src/` or `include/`.
- `project_seeds()` — every `.c/.cpp/.cc/.cxx/.s` under `project/src/**`,
  sorted, suitable as walker seeds.
- `project_search_paths()` — `[include, src]` in PIO order; `include/`
  appears only when populated.
- `libraries_dir()`, `project_root()`, `framework_root()`, `project_src()`
  for downstream APIs.

### Example

```rust
use fbuild_test_support::MiniFramework;
use fbuild_packages::library::framework_library::discover_framework_libraries;
use fbuild_library_select::resolve;

let mut fx = MiniFramework::new();
fx.add_library("SPI").cpp("// impl\n").done();
fx.add_library("Wire").cpp("// wire\n").done();
fx.sketch("#include <SPI.h>\nvoid setup() {}\nvoid loop() {}\n");

let libs = discover_framework_libraries(&fx.libraries_dir());
let sel = resolve(&fx.project_seeds(), &fx.project_search_paths(), &libs);
assert_eq!(sel.required_libraries, vec!["SPI".to_string()]);
```

The `example()` / `extras()` / `tests()` builder methods exist as fodder for
regression tests that prove `collect_library_sources` excludes those
subtrees.

## Usage

Used by other crates as a `[dev-dependencies]` entry to get realistic temporary project directories for integration tests without manual setup.
