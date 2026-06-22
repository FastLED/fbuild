# `manager/`

Sibling-directory submodules for `manager.rs` (the `SharedSerialManager`
implementation). Today this is just `tests.rs` — the original
`#[cfg(test)] mod tests { ... }` block was lifted out of the parent
file as a separate file so the parent stays under the 1000-LOC gate
(see `.github/workflows/loc-gate.yml`).

When growing this module, prefer cohesive per-domain submodules
(`session.rs`, `readers.rs`, `preemption.rs`, …) over letting the
parent file balloon back over the limit. The split convention is
documented in the root `CLAUDE.md`.
