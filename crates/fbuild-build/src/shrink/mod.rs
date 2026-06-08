//! Flash-size reduction for embedded firmware (FastLED/fbuild#493).
//!
//! This module is a scaffold for the `fbuild build --shrink[=MODE]` flag and
//! its per-platform applier registry. Real CLI plumbing, the libc probe, the
//! auto-resolver, the per-platform shrinker registry, and the spec-file /
//! shadow-archive / wrap-fallback link strategies all land in later phases
//! (#493 phases 1+).
//!
//! Until then this module exposes nothing and is wired into the crate only so
//! that subsequent phases can land without touching `lib.rs` again.
