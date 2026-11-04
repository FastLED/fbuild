//! Smoke test for the embedded zccache service. Originally added
//! for FastLED/fbuild#790; reframed in #800 once the embedded
//! backend became mandatory (the `embedded` Cargo feature was
//! deleted, the `embedded` cfg gate at the top of this file went
//! with it).

use fbuild_build::zccache_embedded::FbuildZccacheService;
use fbuild_core::path::NormalizedPath;
use zccache::embedded::ShutdownMode;

/// Dump zccache's own account of the session so a failed hit assertion names
/// its bucket instead of only reporting `cached: false` (FastLED/fbuild#1757).
///
/// `compile_journal.jsonl` records `outcome` and `miss_reason` per compile in
/// the closed vocabulary of `zccache_daemon_core::daemon::compile_journal`;
/// the lifecycle log records `destination_write_failed` with the raw errno.
fn zccache_diagnostics(cache_root: &std::path::Path) -> String {
    /// An embedded service keeps its journals under
    /// `daemon-state/embedded-*/<version>/logs/`, not directly under the root,
    /// so the walk is bounded by depth rather than by a fixed path.
    fn collect(dir: &std::path::Path, depth: usize, out: &mut String) {
        const LIMIT: usize = 200_000;
        if depth == 0 || out.len() > LIMIT {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                collect(&path, depth - 1, out);
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                let Ok(body) = std::fs::read_to_string(&path) else {
                    continue;
                };
                out.push_str(&format!("--- {} ---\n{body}", path.display()));
                if !body.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
    let mut out = String::new();
    collect(cache_root, 6, &mut out);
    if out.is_empty() {
        out.push_str(&format!(
            "<no zccache jsonl logs under {}>\n",
            cache_root.display()
        ));
    }
    out
}

fn find_c_compiler() -> NormalizedPath {
    let path_dirs: Vec<_> =
        std::env::split_paths(&std::env::var_os("PATH").expect("PATH should be set")).collect();
    let on_path = |name: &str| {
        path_dirs
            .iter()
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
            .map(NormalizedPath::from)
    };
    if fbuild_core::platform::host::is_windows() {
        for name in fbuild_core::platform::executable::path_candidate_names("clang") {
            if let Some(candidate) = on_path(&name) {
                return candidate;
            }
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            let candidate = NormalizedPath::new(std::path::Path::new(&program_files))
                .join("LLVM")
                .join("bin")
                .join(fbuild_core::platform::executable::native_name("clang"));
            if candidate.is_file() {
                return candidate;
            }
        }
        for name in fbuild_core::platform::executable::path_candidate_names("gcc") {
            if let Some(candidate) = on_path(&name) {
                return candidate;
            }
        }
        panic!("clang.exe or gcc.exe must be installed for this smoke test");
    }
    for name in ["cc", "clang", "gcc"] {
        if let Some(candidate) = on_path(name) {
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
    let object = tmp
        .path()
        .join(if fbuild_core::platform::host::is_windows() {
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
        "second identical compile should be a cache hit\n\
         warm stderr: {}\n\
         zccache journal (miss_reason names the bucket):\n{}",
        String::from_utf8_lossy(&warm.stderr),
        zccache_diagnostics(svc.cache_root()),
    );
    assert!(object.is_file(), "warm hit should materialize the object");

    svc.shutdown(ShutdownMode::Graceful)
        .await
        .expect("graceful shutdown should succeed");
}

/// FastLED/fbuild#1414: an identical compilation in a *different* project
/// directory must be a cache hit when both compiles pin
/// `ZCCACHE_WORKTREE_ROOT` to their own project root.
///
/// This is the acceptance test for the cross-project reuse claim: the pinned
/// root makes `src/main.c` normalize to the same `$ZCCACHE_WORKTREE_ROOT/src/main.c`
/// key in both trees, so the second tree never recompiles. It exercises the
/// exact production seam — [`FbuildZccacheService::compile`] with the env vec
/// that `fbuild-build-engine`'s `compiler.rs` builds — so a failure here is
/// either a lost env entry or zccache ignoring the pin, never an fbuild build
/// orchestrator artifact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn embedded_compilation_is_shared_across_project_dirs() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("zccache");
    let object_name = if fbuild_core::platform::host::is_windows() {
        "main.obj"
    } else {
        "main.o"
    };

    let project = |tag: &str| {
        let root = tmp.path().join(tag);
        std::fs::create_dir_all(root.join("src")).expect("create project src dir");
        std::fs::write(
            root.join("src").join("main.c"),
            "int cross(void) { return 7; }\n",
        )
        .expect("write source");
        root
    };
    let dir_a = project("proj_a");
    let dir_b = project("proj_b");

    let svc_cache_root = cache_root.clone();
    let svc = FbuildZccacheService::start_in(cache_root)
        .await
        .expect("embedded service should start");
    let compiler = find_c_compiler();

    // Both trees compile the same workspace-relative path to the same
    // workspace-relative output: only the pinned root differs.
    let build = |root: &std::path::Path| {
        let mut env = fbuild_core::subprocess::compile_env_for_build(root)
            .expect("prepare the hermetic compile environment used in production");
        env.push((
            "ZCCACHE_WORKTREE_ROOT".to_string(),
            root.to_string_lossy().into_owned(),
        ));
        let mut args = vec![
            "-c".to_string(),
            "src/main.c".to_string(),
            "-o".to_string(),
            object_name.to_string(),
        ];
        // The production spelling, not a hand-rolled one: a shared cache only
        // reuses the object across project dirs when every path channel is
        // remapped, so this test fails if fbuild's flags regress to the
        // debug-only variant (FastLED/fbuild#1414).
        args.extend(fbuild_paths::portable_prefix_map_args(root, root));
        (args, env)
    };

    let (args_a, env_a) = build(&dir_a);
    let cold = svc
        .compile(&compiler, args_a, dir_a.clone(), env_a)
        .await
        .expect("cold embedded compile in project A should succeed");
    assert_eq!(
        cold.exit_code,
        0,
        "cold compile stderr: {}",
        String::from_utf8_lossy(&cold.stderr)
    );
    assert!(!cold.cached, "fresh cache unexpectedly reported a hit");
    svc.flush()
        .await
        .expect("flush cold compile into the cache");

    // The durable layer is the on-disk dep graph: restart the service so the
    // in-memory request cache is gone and only persisted context keys can
    // serve project B. This is the reported "warm cache, fresh project dir"
    // shape.
    svc.shutdown(ShutdownMode::Graceful)
        .await
        .expect("shutdown after project A");
    let svc = FbuildZccacheService::start_in(svc_cache_root.clone())
        .await
        .expect("embedded service should restart over the warm cache");

    let (args_b, env_b) = build(&dir_b);
    let warm = svc
        .compile(&compiler, args_b, dir_b.clone(), env_b)
        .await
        .expect("embedded compile in project B should succeed");
    assert_eq!(
        warm.exit_code,
        0,
        "project B stderr: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
    assert!(
        warm.cached,
        "an identical compile in a sibling project dir should hit the cache\n\
         project B stderr: {}\n\
         zccache journal (miss_reason names the bucket):\n{}",
        String::from_utf8_lossy(&warm.stderr),
        zccache_diagnostics(svc.cache_root()),
    );
    assert!(
        dir_b.join(object_name).is_file(),
        "a cross-project hit should materialize the object in project B"
    );

    svc.shutdown(ShutdownMode::Graceful)
        .await
        .expect("graceful shutdown should succeed");
}
