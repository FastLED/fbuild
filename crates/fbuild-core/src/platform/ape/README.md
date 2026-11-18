# `platform::ape` — APE (cosmocc) host-tool support

Runs Actually Portable Executable tools on every host fbuild supports without
any host APE support installed.

- `mod.rs` — APE detection, launch planning (`<loader> <image> <args>`), and
  per-host loader extraction: Linux ELF loader, macOS x86_64 Mach-O loader,
  macOS arm64 loader compiled from the image's `ape-m1.c`.
- `prologue.rs` — parses the image's shell prologue (per-CPU loader blobs,
  the macOS x86_64 header-patched blob, the Apple Silicon loader source).
- `install.rs` — owner-only, exec-capable, atomic, content-addressed installs
  under the fork lock (no `ETXTBSY`).
- `native.rs` — a `/bin/sh` shim on every Unix host for spawners fbuild
  doesn't control (zccache): same file name, the image's identity baked in,
  `ape` first on PATH, `exec <loader> <original image>`.

Host specifics (cache dirs, Linux `memfd` fallback) live in
`../{linux,macos,windows}/ape.rs`.
