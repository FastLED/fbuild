# `platform::ape` — APE (cosmocc) host-tool support

Runs Actually Portable Executable tools on every host fbuild supports without
any host APE support installed.

- `mod.rs` — APE detection, launch planning (`<loader> <image> <args>`), and
  per-host loader extraction: Linux ELF loader, macOS x86_64 Mach-O loader,
  macOS arm64 loader compiled from the image's `ape-m1.c`.
- `prologue.rs` — parses the image's shell prologue (loader blobs, loader
  source, `--assimilate` headers).
- `install.rs` — owner-only, exec-capable, atomic, content-addressed installs
  under the fork lock (no `ETXTBSY`).
- `native.rs` — host-native stand-ins (assimilated copy, or a `/bin/sh` shim
  on macOS arm64) for spawners fbuild doesn't control, such as zccache.

Host specifics (cache dirs, Linux `memfd` fallback) live in
`../{linux,macos,windows}/ape.rs`.
