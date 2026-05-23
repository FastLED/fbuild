# soldr Rust Build Performance

Measured local `soldr` performance for building the Rust `fbuild` CLI and
daemon from this workspace.

## Environment

- Host: Windows, `x86_64-pc-windows-msvc`
- Tool wrapper: `soldr 0.7.4`
- Managed cache: `zccache 1.3.0`
- Rust toolchain: `rustc 1.94.1`
- Command:

```powershell
soldr cargo build --release -p fbuild-cli -p fbuild-daemon
```

The benchmark used temporary target directories under `.cache/` so the normal
workspace `target/` directory was not part of the measurement.

## Results

| Case | Time |
|------|-----:|
| Cold: empty target dir and cleared `soldr` cache | 165.536 s |
| Warm cache: same target dir after `cargo clean`, hot `soldr` cache | 55.898 s |
| No-op rebuild: same target dir, no source changes | 0.787 s |
| Fresh different target dir with hot `soldr` cache | 162.834 s |

## Notes

`soldr` is correctly routing Cargo through its managed `zccache`, but cache
reuse was target-directory sensitive in this local run. Rebuilding the same
target directory after `cargo clean` was about 3x faster than the cold build,
while compiling into a different fresh target directory got effectively no
benefit.

Final cache status after the run reported:

```text
Compilations: 1393 total (216 cached, 849 cold, 311 non-cacheable)
Hit rate:     20.3%
Time saved:   ~9m 21s
Artifacts:    849 (2.5 GB)
```

An initial stale cache-daemon state was observed before the benchmark:
`soldr cargo --version` reported that a daemon PID existed but was not accepting
connections. Running `soldr cargo --version` again started a healthy managed
`zccache` daemon, after which the timed runs completed successfully.
