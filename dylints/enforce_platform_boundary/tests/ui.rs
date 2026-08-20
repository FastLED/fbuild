#[test]
fn ui() {
    // SAFETY: this integration-test binary contains only this test, so no peer
    // observes these process-wide build-environment overrides.
    // `dylint_testing` performs a nested Cargo build that is incompatible with
    // an inherited rustc wrapper (upstream Dylint #1696).
    unsafe {
        std::env::remove_var("CARGO_BUILD_TARGET");
        std::env::set_var("RUSTC_WRAPPER", "");
    }
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "./ui");
}
