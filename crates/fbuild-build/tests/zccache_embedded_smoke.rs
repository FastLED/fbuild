//! Smoke test for the embedded zccache service. Originally added
//! for FastLED/fbuild#790; reframed in #800 once the embedded
//! backend became mandatory (the `embedded` Cargo feature was
//! deleted, the `embedded` cfg gate at the top of this file went
//! with it).

use fbuild_build::zccache_embedded::FbuildZccacheService;
use zccache::embedded::ShutdownMode;

fn find_c_compiler() -> std::path::PathBuf {
    let path_dirs: Vec<_> =
        std::env::split_paths(&std::env::var_os("PATH").expect("PATH should be set")).collect();
    let on_path = |name: &str| {
        path_dirs
            .iter()
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    };
    if cfg!(windows) {
        if let Some(candidate) = on_path("clang.exe") {
            return candidate;
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            let candidate = std::path::PathBuf::from(program_files)
                .join("LLVM")
                .join("bin")
                .join("clang.exe");
            if candidate.is_file() {
                return candidate;
            }
        }
        if let Some(candidate) = on_path("gcc.exe") {
            return candidate;
        }
        panic!("clang.exe or gcc.exe must be installed for this smoke test");
    }
    for name in ["cc", "clang", "gcc"] {
        if let Some(candidate) = path_dirs
            .iter()
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
        {
            return candidate;
        }
    }
    panic!("cc, clang, or gcc must be installed for this smoke test");
}

/// `FbuildZccacheService::start_in` produces a working service
/// handle: the cache root exists on disk, the identity carries our
/// product tag, and a graceful shutdown returns cleanly.
///
/// Uses `tokio::test(flavor = "multi_thread")` to match the daemon's
/// runtime shape — `ZccacheService` spawns background tasks via
/// `tokio::spawn`, and a current-thread runtime would serialize
/// them behind the test future.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn embedded_service_starts_and_shuts_down() {
    // Per-test cache root so we don't collide with a running daemon
    // or with parallel test invocations against `~/.fbuild/`.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("zccache");

    let svc = FbuildZccacheService::start_in(cache_root.clone())
        .await
        .expect("embedded service should start cleanly under a fresh cache root");

    assert!(
        svc.cache_root().is_dir(),
        "cache root should exist on disk: {}",
        svc.cache_root().display()
    );
    assert_eq!(
        svc.cache_root(),
        cache_root.as_path(),
        "cache root should match the explicit path we passed in"
    );
    assert_eq!(svc.identity().product, "fbuild");

    svc.shutdown(ShutdownMode::Graceful)
        .await
        .expect("graceful shutdown should succeed");
}

/// A real compile traverses fbuild's embedded zccache boundary twice: the
/// first invocation populates a fresh cache and the second must materialize
/// the object from that cache. This is also a link-time guard against loading
/// two copies of running-process's unmangled `rp_*_public` exports.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn embedded_compilation_cold_miss_then_warm_hit() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("zccache");
    let source = tmp.path().join("smoke.c");
    let object = tmp.path().join(if cfg!(windows) {
        "smoke.obj"
    } else {
        "smoke.o"
    });
    std::fs::write(&source, "int smoke(void) { return 42; }\n").expect("write source");

    let svc = FbuildZccacheService::start_in(cache_root)
        .await
        .expect("embedded service should start");
    let compiler = find_c_compiler();
    let args = vec![
        "-c".to_string(),
        source.to_string_lossy().into_owned(),
        "-o".to_string(),
        object.to_string_lossy().into_owned(),
    ];
    let mut compile_env = fbuild_core::subprocess::compile_env_for_build(tmp.path())
        .expect("prepare the same hermetic compile environment used in production");
    compile_env.push((
        "ZCCACHE_WORKTREE_ROOT".to_string(),
        tmp.path().to_string_lossy().into_owned(),
    ));

    let cold = svc
        .compile(
            &compiler,
            args.clone(),
            tmp.path().to_path_buf(),
            compile_env.clone(),
        )
        .await
        .expect("cold embedded compile should succeed");
    assert_eq!(
        cold.exit_code,
        0,
        "cold compile stderr: {}",
        String::from_utf8_lossy(&cold.stderr)
    );
    assert!(!cold.cached, "fresh cache unexpectedly reported a hit");
    assert!(object.is_file(), "cold compile should create an object");

    svc.flush()
        .await
        .expect("flush cold compile into the cache");
    std::fs::remove_file(&object).expect("remove cold object before warm materialization");

    let warm = svc
        .compile(&compiler, args, tmp.path().to_path_buf(), compile_env)
        .await
        .expect("warm embedded compile should succeed");
    assert_eq!(
        warm.exit_code,
        0,
        "warm compile stderr: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
    assert!(
        warm.cached,
        "second identical compile should be a cache hit"
    );
    assert!(object.is_file(), "warm hit should materialize the object");

    svc.shutdown(ShutdownMode::Graceful)
        .await
        .expect("graceful shutdown should succeed");
}
