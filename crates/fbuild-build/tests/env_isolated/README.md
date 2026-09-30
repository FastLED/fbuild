# env_isolated integration tests

One test binary (FastLED/fbuild#1577) for `fbuild-build` integration tests
that mutate process-wide environment variables (`FBUILD_*`,
`ZCCACHE_DAEMON_NAMESPACE`). Every test takes `ENV_LOCK` from `main.rs` for
its whole body. Add env-mutating tests here as a module; everything else goes
in `../it`.

Run one test: `soldr cargo test -p fbuild-build --test env_isolated -- --ignored --exact <module>::<test> --nocapture`
