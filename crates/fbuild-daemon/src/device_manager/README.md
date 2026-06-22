# `device_manager/`

Sibling-directory submodules for `device_manager.rs`. Today this is
just `tests.rs` — the original `#[cfg(test)] mod tests { ... }` block
was lifted out of the parent file as a separate file so the parent
stays under the 1000-LOC gate (see `.github/workflows/loc-gate.yml`).

When growing this module, prefer adding cohesive submodules here (per
the `foo.rs → foo/<sub>.rs` split convention in the root `CLAUDE.md`)
over letting the parent file balloon back over the limit.
