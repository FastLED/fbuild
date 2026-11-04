//! FastLED/fbuild#1885: assembly TUs belong to the C driver.
//!
//! Kept in a sibling module: `library_compiler_tests.rs` is close to the
//! 1000-line per-file cap, and the compile-dispatch contract is its own
//! topic (driver choice + the rebuild signature that must describe it).

use super::tests::{FakeBackend, install_fake_ar};
use super::*;

/// `.S` (assembler-with-cpp) and `.s` (raw) must be compiled by the C driver
/// with the target C flags. Dispatching them through g++ defines
/// `__cplusplus`, switches the `#ifdef __cplusplus` blocks in vendored
/// assembly sources, and hands the assembler a `-std=gnu++*` recipe it was
/// never written for. `.c` stays on the C driver; `.cpp` stays on C++.
#[tokio::test]
async fn assembly_tus_compile_with_the_c_driver_and_c_flags() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let ar_path = tmp.path().join("ar");
    install_fake_ar(&ar_path);

    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let sources = write_sources(&src_dir, &["a.c", "b.S", "c.s", "d.cpp"]);

    let c_flags = vec!["-std=gnu17".to_string(), "-Os".to_string()];
    let cpp_flags = vec!["-std=gnu++17".to_string(), "-fno-rtti".to_string()];
    let gcc = Path::new("/fake/gcc");
    let gxx = Path::new("/fake/g++");

    let backend = std::sync::Arc::new(FakeBackend::new());
    let backend_dyn: std::sync::Arc<dyn LibCompileBackend> = backend.clone();
    let out = tmp.path().join("out");

    compile_library_gated(
        "libx",
        &sources,
        &[],
        gcc,
        gxx,
        &ar_path,
        &c_flags,
        &cpp_flags,
        &out,
        false,
        &job_gate(1),
        None,
        None,
        Some(backend_dyn),
        Some(tmp.path()),
    )
    .await
    .unwrap()
    .unwrap();

    let calls = backend.calls();
    let by_source = |name: &str| {
        calls
            .iter()
            .find(|(_, args)| {
                args.iter()
                    .position(|a| a == "-c")
                    .and_then(|i| args.get(i + 1))
                    .is_some_and(|s| s.ends_with(name))
            })
            .unwrap_or_else(|| panic!("no compile dispatched for {name}: {calls:?}"))
            .clone()
    };

    for name in ["a.c", "b.S", "c.s"] {
        let (compiler, args) = by_source(name);
        assert_eq!(
            compiler, "/fake/gcc",
            "{name} must use the C driver; got {calls:?}"
        );
        assert!(
            args.iter().any(|a| a == "-std=gnu17"),
            "{name} must get the target C flags; got {args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|a| a.starts_with("-std=gnu++") || a == "-fno-rtti"),
            "{name} must not get C++ flags; got {args:?}"
        );
    }

    let (compiler, args) = by_source("d.cpp");
    assert_eq!(compiler, "/fake/g++", "C++ sources keep the C++ driver");
    assert!(args.iter().any(|a| a == "-std=gnu++17"), "got {args:?}");
}

/// The rebuild signature must describe the *same* invocation the compile will
/// run: an assembly TU signed as a C++ compile would be re-signed on every
/// flag change and would invalidate the wrong set of objects.
#[test]
fn compile_signature_of_assembly_describes_the_c_driver_invocation() {
    let gcc = Path::new("/fake/gcc");
    let gxx = Path::new("/fake/g++");
    let c_flags = vec!["-std=gnu17".to_string()];
    let cpp_flags = vec!["-std=gnu++17".to_string()];
    let includes = vec!["-I/x".to_string()];

    let as_c = build_rebuild_signature(gcc, &c_flags, &includes);
    let as_cpp = build_rebuild_signature(gxx, &cpp_flags, &includes);

    for source in ["/src/vector.S", "/src/vector.s", "/src/plain.c"] {
        assert_eq!(
            compile_signature(Path::new(source), gcc, gxx, &c_flags, &cpp_flags, &includes),
            as_c,
            "{source} must be signed as a C-driver invocation"
        );
    }
    for source in ["/src/plain.cpp", "/src/plain.cc", "/src/plain.cxx"] {
        assert_eq!(
            compile_signature(Path::new(source), gcc, gxx, &c_flags, &cpp_flags, &includes),
            as_cpp,
            "{source} must be signed as a C++-driver invocation"
        );
    }
}
