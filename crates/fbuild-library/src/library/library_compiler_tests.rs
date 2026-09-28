use super::*;
use std::time::Duration;

fn test_signature() -> &'static str {
    "test-signature"
}

#[test]
fn test_is_cxx_only_flag() {
    assert!(is_cxx_only_flag("-std=gnu++2b"));
    assert!(is_cxx_only_flag("-std=c++17"));
    assert!(is_cxx_only_flag("-fno-rtti"));
    assert!(is_cxx_only_flag("-fuse-cxa-atexit"));
    assert!(!is_cxx_only_flag("-std=gnu17"));
    assert!(!is_cxx_only_flag("-Os"));
    assert!(!is_cxx_only_flag("-DFOO"));
}

#[test]
fn test_object_path_unique() {
    let obj_dir = Path::new("/tmp/obj");
    let p1 = object_path(Path::new("/src/a/main.cpp"), obj_dir);
    let p2 = object_path(Path::new("/src/b/main.cpp"), obj_dir);
    assert_ne!(
        p1, p2,
        "different source paths should produce different object paths"
    );
}

#[test]
fn test_object_path_extension() {
    let obj_dir = Path::new("/tmp/obj");
    let p = object_path(Path::new("/src/main.cpp"), obj_dir);
    assert_eq!(p.extension().unwrap(), "o");
}

#[tokio::test]
async fn test_build_include_flags_small() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dirs = vec![Path::new("/a").to_path_buf(), Path::new("/b").to_path_buf()];
    let flags = build_include_flags(&dirs, tmp.path()).await.unwrap();
    assert_eq!(flags.len(), 2);
    assert!(flags[0].starts_with("-I"));
}

#[test]
fn test_invocation_response_file_path_makes_relative_path_absolute() {
    let relative = Path::new("build/tmp/test.rsp");
    let absolute = invocation_response_file_path(relative).unwrap();
    assert!(absolute.is_absolute());
    assert!(absolute.ends_with(relative));
}

#[test]
fn test_invocation_response_file_path_preserves_absolute_path() {
    let absolute_input = std::env::current_dir().unwrap().join("build/tmp/test.rsp");
    let absolute = invocation_response_file_path(&absolute_input).unwrap();
    assert_eq!(absolute, absolute_input);
}

#[test]
fn test_object_needs_rebuild_when_object_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("src.cpp");
    std::fs::write(&source, "int x;").unwrap();
    let object = tmp.path().join("src.o");

    assert!(object_needs_rebuild(&source, &object, test_signature()).unwrap());
}

#[test]
fn test_object_needs_rebuild_when_source_newer() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("src.cpp");
    let object = tmp.path().join("src.o");
    std::fs::write(&source, "int x;").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&object, "obj").unwrap();
    std::fs::write(command_hash_path(&object), test_signature()).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&source, "int y;").unwrap();

    assert!(object_needs_rebuild(&source, &object, test_signature()).unwrap());
}

#[test]
fn test_object_needs_rebuild_when_object_current() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("src.cpp");
    let object = tmp.path().join("src.o");
    std::fs::write(&source, "int x;").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&object, "obj").unwrap();
    std::fs::write(command_hash_path(&object), test_signature()).unwrap();

    assert!(!object_needs_rebuild(&source, &object, test_signature()).unwrap());
}

#[test]
fn test_object_needs_rebuild_when_header_dep_is_newer() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("src.cpp");
    let header = tmp.path().join("config.h");
    let object = tmp.path().join("src.o");
    let depfile = tmp.path().join("src.d");

    std::fs::write(&source, "#include \"config.h\"\n").unwrap();
    std::fs::write(&header, "#define X 1\n").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&object, "obj").unwrap();
    std::fs::write(
        &depfile,
        format!(
            "{}: {} {}\n",
            object.display(),
            source.display(),
            header.display()
        ),
    )
    .unwrap();
    std::fs::write(command_hash_path(&object), test_signature()).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&header, "#define X 2\n").unwrap();

    assert!(object_needs_rebuild(&source, &object, test_signature()).unwrap());
}

#[test]
fn test_object_needs_rebuild_when_command_hash_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("src.cpp");
    let object = tmp.path().join("src.o");

    std::fs::write(&source, "int x;").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&object, "obj").unwrap();
    std::fs::write(command_hash_path(&object), "old-signature").unwrap();

    assert!(object_needs_rebuild(&source, &object, test_signature()).unwrap());
}

#[test]
fn test_archive_is_up_to_date_when_archive_newer_than_all_objects() {
    let tmp = tempfile::TempDir::new().unwrap();
    let object_a = tmp.path().join("a.o");
    let object_b = tmp.path().join("b.o");
    let archive = tmp.path().join("libx.a");
    std::fs::write(&object_a, "a").unwrap();
    std::fs::write(&object_b, "b").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&archive, "archive").unwrap();

    assert!(archive_is_up_to_date(&archive, &[object_a, object_b]).unwrap());
}

#[test]
fn test_archive_is_not_up_to_date_when_object_newer() {
    let tmp = tempfile::TempDir::new().unwrap();
    let object = tmp.path().join("a.o");
    let archive = tmp.path().join("libx.a");
    std::fs::write(&object, "a").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&archive, "archive").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&object, "newer").unwrap();

    assert!(!archive_is_up_to_date(&archive, &[object]).unwrap());
}

// ---- FastLED/fbuild#1559 / #1560 shared-gate scheduling tests ----

/// Install a fake `ar` shell script at `path` that creates its 2nd arg
/// (the archive) as an empty file. Installed via staging + rename to
/// avoid ETXTBSY under parallel tests.
fn install_fake_ar(path: &Path) {
    let script = "#!/bin/sh\n# argv: rcs <archive> <objs...>\ntouch \"$2\"\n";
    let staging = path.with_extension("staging");
    std::fs::write(&staging, script).unwrap();
    fbuild_core::platform::fs::set_executable(&staging).unwrap();
    std::fs::rename(&staging, path).unwrap();
}

/// A fake [`LibCompileBackend`] that tracks concurrency, start order, and
/// writes the object file, failing for sources whose file name starts
/// with "bad".
struct FakeBackend {
    in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    max_in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    start_order: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl FakeBackend {
    fn new() -> Self {
        Self {
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            max_in_flight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            start_order: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}

#[async_trait::async_trait]
impl LibCompileBackend for FakeBackend {
    async fn compile(
        &self,
        _compiler: &Path,
        args: Vec<String>,
        cwd: NormalizedPath,
        _env: Vec<(String, String)>,
    ) -> Result<LibCompileOutcome> {
        self.call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Locate the source arg (before "-c" wouldn't help; source is the
        // arg right after "-c", obj is the arg right after "-o").
        let source_arg = args
            .iter()
            .position(|a| a == "-c")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default();
        let obj_arg = args
            .iter()
            .position(|a| a == "-o")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default();
        let source_name = Path::new(&source_arg)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let current = self
            .in_flight
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.max_in_flight
            .fetch_max(current, std::sync::atomic::Ordering::SeqCst);
        self.start_order.lock().unwrap().push(source_name.clone());

        tokio::time::sleep(Duration::from_millis(40)).await;

        self.in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);

        if source_name.starts_with("bad") {
            return Ok(LibCompileOutcome {
                exit_code: 1,
                stdout: Vec::new(),
                stderr: b"boom".to_vec(),
            });
        }

        let obj_path = Path::new(&obj_arg);
        let obj_path = if obj_path.is_absolute() {
            obj_path.to_path_buf()
        } else {
            cwd.as_path().join(obj_path)
        };
        if let Some(parent) = obj_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&obj_path, "obj").unwrap();

        Ok(LibCompileOutcome {
            exit_code: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_gate_shared_across_two_libraries_caps_concurrency() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let ar_path = tmp.path().join("ar");
    install_fake_ar(&ar_path);

    let gate = job_gate(2);
    let backend = std::sync::Arc::new(FakeBackend::new());

    let src_dir_a = tmp.path().join("a_src");
    let src_dir_b = tmp.path().join("b_src");
    std::fs::create_dir_all(&src_dir_a).unwrap();
    std::fs::create_dir_all(&src_dir_b).unwrap();
    let sources_a = write_sources(&src_dir_a, &["a1.cpp", "a2.cpp", "a3.cpp"]);
    let sources_b = write_sources(&src_dir_b, &["b1.cpp", "b2.cpp", "b3.cpp"]);

    let out_a = tmp.path().join("out_a");
    let out_b = tmp.path().join("out_b");

    let backend_a: std::sync::Arc<dyn LibCompileBackend> = backend.clone();
    let backend_b: std::sync::Arc<dyn LibCompileBackend> = backend.clone();

    let fut_a = compile_library_gated(
        "liba",
        &sources_a,
        &[],
        Path::new("/fake/gcc"),
        Path::new("/fake/g++"),
        &ar_path,
        &[],
        &[],
        &out_a,
        false,
        &gate,
        None,
        None,
        Some(backend_a),
    );
    let fut_b = compile_library_gated(
        "libb",
        &sources_b,
        &[],
        Path::new("/fake/gcc"),
        Path::new("/fake/g++"),
        &ar_path,
        &[],
        &[],
        &out_b,
        false,
        &gate,
        None,
        None,
        Some(backend_b),
    );

    let (res_a, res_b) = tokio::join!(fut_a, fut_b);
    let archive_a = res_a.unwrap().unwrap();
    let archive_b = res_b.unwrap().unwrap();
    assert!(archive_a.exists());
    assert!(archive_b.exists());

    let max = backend
        .max_in_flight
        .load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        max <= 2,
        "max in-flight compiles ({max}) exceeded the shared gate's 2 permits"
    );
    assert!(max >= 1);
}

#[tokio::test]
async fn test_dispatch_order_cpp_before_c_before_asm() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let ar_path = tmp.path().join("ar");
    install_fake_ar(&ar_path);

    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let sources = write_sources(&src_dir, &["a.c", "b.S", "c.cpp", "d.c", "e.cpp"]);

    let gate = job_gate(1);
    let backend = std::sync::Arc::new(FakeBackend::new());
    let backend_dyn: std::sync::Arc<dyn LibCompileBackend> = backend.clone();
    let out = tmp.path().join("out");

    let archive = compile_library_gated(
        "libx",
        &sources,
        &[],
        Path::new("/fake/gcc"),
        Path::new("/fake/g++"),
        &ar_path,
        &[],
        &[],
        &out,
        false,
        &gate,
        None,
        None,
        Some(backend_dyn),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(archive.exists());

    let order = backend.start_order.lock().unwrap().clone();
    assert_eq!(order, vec!["c.cpp", "e.cpp", "a.c", "d.c", "b.S"]);
}

#[tokio::test]
async fn test_failure_stops_spawning_and_releases_permit() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let ar_path = tmp.path().join("ar");
    install_fake_ar(&ar_path);

    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let sources = write_sources(&src_dir, &["bad.cpp", "ok1.cpp", "ok2.cpp"]);

    let gate = job_gate(1);
    let backend = std::sync::Arc::new(FakeBackend::new());
    let backend_dyn: std::sync::Arc<dyn LibCompileBackend> = backend.clone();
    let out = tmp.path().join("out");

    let result = compile_library_gated(
        "libx",
        &sources,
        &[],
        Path::new("/fake/gcc"),
        Path::new("/fake/g++"),
        &ar_path,
        &[],
        &[],
        &out,
        false,
        &gate,
        None,
        None,
        Some(backend_dyn),
    )
    .await;

    assert!(result.is_err(), "expected an error from the failed compile");
    let order = backend.start_order.lock().unwrap().clone();
    assert_eq!(
        order,
        ["bad.cpp"],
        "nothing may start once a compile has failed, got order {order:?}"
    );
    assert_eq!(
        gate.available_permits(),
        1,
        "the permit held by the failed task must be released back to the gate"
    );
}

#[tokio::test]
async fn test_up_to_date_skips_recompile() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let ar_path = tmp.path().join("ar");
    install_fake_ar(&ar_path);

    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let sources = write_sources(&src_dir, &["only.cpp"]);

    let gate = job_gate(1);
    let backend = std::sync::Arc::new(FakeBackend::new());
    let out = tmp.path().join("out");

    let backend_dyn1: std::sync::Arc<dyn LibCompileBackend> = backend.clone();
    let archive1 = compile_library_gated(
        "libx",
        &sources,
        &[],
        Path::new("/fake/gcc"),
        Path::new("/fake/g++"),
        &ar_path,
        &[],
        &[],
        &out,
        false,
        &gate,
        None,
        None,
        Some(backend_dyn1),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(archive1.exists());
    let first_count = backend.call_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(first_count, 1);

    let backend_dyn2: std::sync::Arc<dyn LibCompileBackend> = backend.clone();
    let archive2 = compile_library_gated(
        "libx",
        &sources,
        &[],
        Path::new("/fake/gcc"),
        Path::new("/fake/g++"),
        &ar_path,
        &[],
        &[],
        &out,
        false,
        &gate,
        None,
        None,
        Some(backend_dyn2),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(archive2, archive1);
    let second_count = backend.call_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        second_count, first_count,
        "second compile of an unchanged library should not call the backend again"
    );
}
