# fbuild-core integration tests

- **`dep_identity.rs`** -- regression guard for FastLED/fbuild#1239. fbuild
  embeds zccache and also depends directly on `running-process`, whose
  unmangled `rp_*_public` native symbols mean that resolving two package
  identities would link two copies of those symbols. The test parses the
  workspace `Cargo.lock` and fails if more than one `running-process`
  package identity/source/revision is resolved, and mechanically checks
  that the workspace `Cargo.toml` pin matches the locked revision (i.e.
  fbuild's direct pin equals the revision zccache resolved transitively).
