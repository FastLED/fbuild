# `ban_raw_fbuild_path` — cargo config

[`config.toml`](config.toml) points the linker at `dylint-link`, which
is what turns this crate's `cdylib` into a loadable Dylint library.
Every lint crate under `dylints/` carries the same two lines; without
them `cargo build` produces a shared object the Dylint driver cannot
load.
