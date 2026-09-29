fn main() {
    if std::env::var_os("CARGO_FEATURE_EXTENSION_MODULE").is_some() {
        pyo3_build_config::add_extension_module_link_args();
        return;
    }
    // Embedded-CPython test binaries link the shared `libpython` of the exact
    // interpreter PyO3 resolved (`PYO3_PYTHON`). Bake that interpreter's
    // library directory into their RUNPATH so they start without a hand-made
    // `LD_LIBRARY_PATH` (FastLED/fbuild#1487). `-tests` scopes the flag to test
    // targets; the production cdylib and other builds are untouched.
    let unix = std::env::var("CARGO_CFG_TARGET_FAMILY").is_ok_and(|f| f == "unix");
    if unix {
        if let Some(lib_dir) = pyo3_build_config::get().lib_dir() {
            println!("cargo:rustc-link-arg-tests=-Wl,-rpath,{lib_dir}");
        }
    }
}
