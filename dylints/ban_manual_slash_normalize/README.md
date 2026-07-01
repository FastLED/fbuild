# `ban_manual_slash_normalize`

This lint bans hand-rolled `.replace('\\', "/")` rewrites in workspace
code and directs developers to
`fbuild_core::path::NormalizedPath::display_slash()` instead.

## Why

Every hand-rolled `path.to_string_lossy().replace('\\', "/")` (and the
`to_str().unwrap().replace('\\', "/")` variant) is a re-implementation of
what `NormalizedPath::display_slash()` already does — including the UNC
prefix strip and the correct `cfg!(windows)` gate.

The bugs #875 → #885 → #890 → #912 were all the same class: some path
argument reached the compiler / linker / spec-file with backslashes
still in it, GCC's driver parsed `\` as an escape, and the compile
failed with a file-not-found. Each fix added yet another hand-rolled
`.replace('\\', "/")` at yet another call site. This lint closes that
loop by making the anti-pattern impossible to introduce.

## Rollout

The lint is ON and denies new hand-rolled `.replace('\\', "/")` calls by
default. The workspace primitive at
`crates/fbuild-core/src/path.rs` is the ONE place that owns the
transformation; every other call site delegates to
`NormalizedPath::display_slash()`.

The `src/allowlist.txt` file lists the small set of legitimate
exceptions (primitive definition, glob-pattern normalization helper,
lint UI fixtures). New entries require a written justification and
should not paper over "I don't want to plumb `NormalizedPath` through
here" — thread the type instead.

## Detection

The lint matches the exact anti-pattern shape:

- `.replace('\\', "/")` (single-char + single-str arguments), regardless
  of the receiver expression.

The receiver is not inspected — this is a syntactic match. That yields
some false positives (any `.replace('\\', "/")` on a non-path string)
but the whole workspace grep confirms the pattern is only ever used to
slash-normalize paths, so the false-positive rate is expected to stay
near zero. Any true false-positive lands on `src/allowlist.txt`.

## See also

- FastLED/fbuild#911 — the tracking issue that motivated this lint,
  filed after PR #890 + PR #912 both had to patch the same anti-pattern
  at yet another call site.
- FastLED/fbuild#826 — the parent "candidate lints" gotcha sweep.
- FastLED/fbuild#437 — the Phase 1 issue that introduced
  `NormalizedPath` and `ban_std_pathbuf` but never finished the
  migration.
- `crates/fbuild-core/src/path.rs::NormalizedPath::display_slash` —
  the canonical primitive every caller now delegates to.
