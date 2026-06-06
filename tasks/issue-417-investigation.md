## Investigation report — pre-PR triage

I picked this up via `/clud-fix` to merge a fix end-to-end, but the intake
gate flagged it as a feature request with three different scopes (A, B, C)
and no chosen "definition of fixed." A PR can't be opened until we agree on
which path to land. Below is what I verified in the tree today, where the
mechanics actually live, and a recommended cut-line — so the discussion can
be about scope, not about whether the trap is real (it is) or whether fbuild
has the raw materials to address it (it does).

### What fbuild has today (verified against current `main`)

- **No `size` / `analyze` / `bloat` / `symbols` top-level subcommand.** Confirmed
  in `crates/fbuild-cli/src/cli/args.rs:65-389` — the subcommand enum is
  `Build / Deploy / Monitor / Reset / Purge / Daemon / Show / Device / Mcp /
  ClangTidy / Iwyu / TestEmu / ClangQuery / Lnk / LibSelect / CompileMany / Ci`.
  Your `fbuild --help` claim in the issue matches the source.
- **`Build` *does* already expose `--symbol-analysis [PATH]`** at
  `crates/fbuild-cli/src/cli/args.rs:90-93` ("Run per-symbol memory analysis
  after building; optionally write report to PATH instead of streaming to
  console"). So fbuild already has a foothold for post-build symbol reporting
  — it just operates on the ELF via `nm`, not on the `.map` file.
- **`SymbolMap`** (`crates/fbuild-core/src/lib.rs:300-383`) is the existing
  analyzer: parses `nm --print-size --size-sort --reverse-sort` output and
  classifies symbols into Flash/RAM regions. **ELF-as-ground-truth path —
  immune to the `0x00000000` trap by construction**, which is exactly the
  property the issue advocates for.
- **Map files *are* emitted** by every platform's linker via
  `-Wl,-Map=firmware.map` (AVR `crates/fbuild-build/src/avr/avr_linker.rs:111`
  and the equivalent in CH32V, ESP8266, generic ARM, NRF52, Renesas, SAM,
  SiliconLabs, Teensy). So the map file is sitting on disk next to the ELF
  in every build — it's available, just not parsed anywhere in fbuild.
- **No map-file parser** anywhere in the workspace. No tests or examples
  consume `firmware.map`. The "trap" you describe is therefore not present
  in fbuild today because fbuild doesn't parse map files at all — but it
  also doesn't prevent *consumers* (FastLED `ci/` scripts) from falling
  into the trap when they reach for the map directly.
- **`build_info.json`** (issue #297) lives in `crates/fbuild-build/src/build_info.rs:28-61`
  (`BuildInfo` struct, `emit_build_info()` at line 136). It already publishes
  `prog_path`, `cc_path`, `cxx_path`, `ar_path`, `objcopy_path`, `size_path`,
  plus flags/defines/includes/libs. Adding a `map_path` field next to
  `prog_path` would be a one-line schema change with high downstream value.
- **Docs convention:** architecture docs live in `docs/architecture/*.md`
  (overview, data-flow, deploy-preemption, library-selection, portability,
  pyo3-bindings, runtime, serial). A "size analysis gotchas" page would fit
  there or as a peer doc at `docs/SIZE_ANALYSIS.md`.
- **Diagnostic-subcommand exception** is precedent for option A. Per
  `crates/CLAUDE.md`, `clang-tidy / clang-query / iwyu / mcp / lnk / lib-select`
  run in-process and intentionally bypass the daemon because they're
  read-only diagnostics. A hypothetical `fbuild size` / `fbuild bloat`
  fits that pattern cleanly — no HTTP round-trip needed.

### The trap, restated to make sure I have it right

GNU `ld` `--gc-sections` keeps the dropped section row in the `.map` for
debugging, but writes its placement address as `0x00000000`. A naive
sum-by-archive over `.rodata.*` rows in the map double-counts those dead
sections. ELF-side tools (`nm`, `readelf`, `objdump`) never see them
because the linker really did drop them, which is why
`SymbolMap`-via-`nm` is already trap-immune. The risk surface is purely
*map-file analyzers*. Your reproduction (`x509_crt_bundle 69,880 B`,
`mesh_parent 11,108 B`, `huffTable 8,484 B`, `PLM_AUDIO_SYNTHESIS 2,048 B`,
~95 KB total phantom-bloat) matches what I'd expect from xtensa-esp32s3-elf
14.2.0 with `--gc-sections` on. I have no reason to doubt the numbers.

### Scope read on the three options

- **Option C (`fbuild_size_helpers.py`) — declined as written.** The repo's
  language policy (`CLAUDE.md`: *"Python is only for CI scripts, packaging,
  hooks, and PyO3 bindings. All tests, benchmarks, and application logic
  must be written in Rust"*) puts a vendored Python helper module
  out-of-bounds. The PyO3 surface (`fbuild-python`) is locked to
  `SerialMonitor` / `Daemon`. Closest in-policy variant: ship a Rust
  library (or expose it via the existing `--symbol-analysis` machinery)
  that consumers shell out to via `fbuild` — i.e. option A in a thinner
  jacket.
- **Option B (docs only) — small, well-bounded, lossy.** ~1 doc page in
  `docs/architecture/` or `docs/SIZE_ANALYSIS.md`, plus a one-line
  callout in `crates/fbuild-build/src/build_info.rs` near where ELF
  paths are emitted. Catches no analyzers automatically; relies on
  reviewers remembering the rule. Total work: <100 LoC of markdown.
- **Option A (`fbuild size` / `fbuild bloat`) — the goal-shaped fix,
  but a real chunk of work.** Concretely it's: a map-file parser
  with the `address != 0x00000000` filter, an ELF cross-check that
  routes through the existing `SymbolMap`/`nm` plumbing, archive
  attribution, JSON output for CI, optionally a `--diff PRIOR.json`
  comparator. Realistic estimate: a new `fbuild-bloat` crate (or
  a module under `fbuild-build/src/size/`) + a `Size` variant on
  `Commands` + wiring through `fbuild-cli`, plus tests against
  vendored map/ELF fixtures. Probably 600–1200 LoC of Rust + tests.
  Fits the diagnostic-subcommand exception, so no daemon work.

### Recommendation

**Land B + a deliberately minimal slice of A in one PR; defer the
full A to a follow-up.** Concretely:

1. New `docs/SIZE_ANALYSIS.md` with the `0x00000000` filter rule, a
   worked example using your `x509_crt_bundle` case, the vetted awk
   one-liner, and a pointer at `build_info.json` as the
   ELF-truth contract.
2. Add `map_path` to `BuildInfo` (`crates/fbuild-build/src/build_info.rs`)
   so downstream tooling has a single supported way to locate the map
   without duplicating fbuild's pathing logic. One field, one assignment,
   no behavior change.
3. Defer the `fbuild size` subcommand to a follow-up issue with a
   bounded design (map parser + filter + ELF cross-check via existing
   `SymbolMap`).

This makes step 7 of `/clud-fix` (validate the reproduction) tractable:
the doc + `map_path` makes it possible for someone running the issue's
exact awk snippet to land on the correct version on the first try, and
the follow-up issue is what closes the door entirely.

### What I need from you before opening a PR

1. **Which path?** B-plus-thin-A as recommended, full A in one PR, or
   pure-B with no schema change?
2. **Doc location**: `docs/SIZE_ANALYSIS.md` (peer to ROADMAP/WHY) or
   `docs/architecture/size-analysis.md` (with the other architecture
   docs)? Repo convention slightly prefers the latter but the topic is
   more "guide" than "architecture."
3. **If A is in scope**: should the subcommand be named `size`, `bloat`,
   or `analyze`? `size` collides with the GNU `size` binary; `bloat`
   matches `cargo-bloat` precedent and is unambiguous.
4. **Anything I'm wrong about above** — particularly: is the existing
   `Build --symbol-analysis` flag the place this should hook in, or
   should it be a standalone subcommand from day one?

I'll wait for direction here before opening a PR. If you'd rather just
say "ship option A as one big PR," that's also fine — I just don't want
to guess the scope on a feature request with three explicit branches.
