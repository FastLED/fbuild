# fbuild-header-scan

Line-oriented C/C++ `#include` scanner plus a transitive include-graph walker.

The scanner is a pure function from source text to a list of `#include` directives.
It tokenizes by line while tracking comment and string-literal state so it does not
match `#include` inside `// ...`, `/* ... */`, `"..."`, `R"(...)"`, or character
literals. It deliberately does **not** evaluate `#if` / `#ifdef` — both branches of a
conditional are scanned. False positives in the include set are acceptable; false
negatives are not.

The walker resolves includes against an ordered list of search paths (project →
framework → toolchain), follows quoted-include same-directory resolution first,
deduplicates via a visited set, and returns the transitive set of reached files
plus any unresolved include strings. Output is sorted for deterministic cache keys.

This crate has no fbuild dependencies and is independently testable.
