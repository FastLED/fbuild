//! Tests for `crate::compiler`. Lives next to `compiler.rs` and is wired in
//! via `#[cfg(test)] #[path = "compiler_tests.rs"] mod tests;` so the
//! production module stays under the workspace's 1000-LOC-per-file limit.

use super::*;

/// Regression for #282. `compile_source` promotes `output` to absolute
/// before computing `compile_cwd`. This test exercises the post-fix
/// invariant: when the downstream zccache helpers
/// (`compile_cwd_from_output` + `path_arg_for_compile_cwd`) are fed an
/// absolute output under a `.fbuild` workspace, `cwd.join(arg_for_o)` must
/// resolve to that same absolute output. The pre-fix bug was that
/// `compile_cwd_from_output` canonicalized the workspace to absolute while
/// `path_arg_for_compile_cwd` short-circuited on relative paths and emitted
/// the raw relative string — so gcc received absolute `cwd` + relative `-o`,
/// resolving to a doubled path whose parent directory (`core/`) was never
/// created.
#[tokio::test]
async fn compile_path_contract_pairs_cwd_and_output_arg_for_282() {
    use crate::zccache::{compile_cwd_from_output, path_arg_for_compile_cwd};
    let tmp = tempfile::tempdir().unwrap();
    // Normalize the tempdir to the same form `compile_cwd_from_output` will
    // produce, so the final equality assertion compares like for like.
    // - macOS: `tempfile::tempdir()` returns `/var/folders/...` but
    //   `canonicalize` resolves the symlink to `/private/var/folders/...`,
    //   which is what the helper returns.
    // - Windows: `canonicalize` adds the `\\?\` extended-length prefix, but
    //   `compile_cwd_from_output` runs the result through `strip_unc_prefix`.
    //   Strip the prefix here too so both sides stay on the plain `C:\...`
    //   form.
    let tmp_canon = fbuild_core::path::canonicalize_existing(tmp.path())
        .await
        .unwrap()
        .into_path_buf();
    // Workspace shape mirrors CI: <project>/.fbuild/build/<env>/quick/core
    let workspace = tmp_canon.join("proj_for_282");
    let core = workspace
        .join(".fbuild")
        .join("build")
        .join("x")
        .join("quick")
        .join("core");
    std::fs::create_dir_all(&core).unwrap();
    let abs_output = core.join("HWCDC_57cf.cpp.o");

    let cwd = compile_cwd_from_output(&abs_output)
        .expect("compile_cwd_from_output should locate the workspace");
    let arg = path_arg_for_compile_cwd(&abs_output, &cwd);

    // Contract: gcc's effective output (cwd joined with the `-o` arg) points
    // at the same file as the absolute output.
    let resolved = cwd.join(std::path::Path::new(&arg));
    assert_eq!(
        absolute_from_cwd(&resolved),
        absolute_from_cwd(&abs_output),
        "cwd.join(out_arg) must resolve to the absolute output"
    );
}

#[test]
fn absolute_from_cwd_is_identity_on_absolute_paths() {
    let p = if cfg!(windows) {
        std::path::PathBuf::from(r"C:\some\absolute\path")
    } else {
        std::path::PathBuf::from("/some/absolute/path")
    };
    assert_eq!(absolute_from_cwd(&p), p);
}

#[test]
fn absolute_from_cwd_promotes_relative_paths() {
    let rel = std::path::Path::new("some/rel/path");
    let abs = absolute_from_cwd(rel);
    assert!(abs.is_absolute());
    assert!(abs.ends_with(rel));
}

#[test]
fn test_build_define_flags() {
    let base = CompilerBase {
        mcu: "atmega328p".to_string(),
        f_cpu: "16000000L".to_string(),
        defines: {
            let mut d = HashMap::new();
            d.insert("PLATFORMIO".to_string(), "1".to_string());
            d.insert("F_CPU".to_string(), "16000000L".to_string());
            d
        },
        include_dirs: Vec::new(),
        verbose: false,
    };
    let flags = base.build_define_flags();
    assert!(flags.contains(&"-DPLATFORMIO".to_string()));
    assert!(flags.contains(&"-DF_CPU=16000000L".to_string()));
}

#[test]
fn test_build_include_flags() {
    let base = CompilerBase {
        mcu: String::new(),
        f_cpu: String::new(),
        defines: HashMap::new(),
        include_dirs: vec![
            PathBuf::from("/usr/include"),
            PathBuf::from("/opt/avr/include"),
        ],
        verbose: false,
    };
    let flags = base.build_include_flags();
    assert_eq!(flags.len(), 2);
    assert!(flags[0].starts_with("-I"));
}

#[test]
fn test_needs_rebuild_missing_object() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = tmp.path().join("test.c");
    std::fs::write(&src, "int main() {}").unwrap();
    let obj = tmp.path().join("test.o");
    assert!(CompilerBase::needs_rebuild(&src, &obj));
}

#[test]
fn test_object_path() {
    let path = CompilerBase::object_path(Path::new("main.cpp"), Path::new("/build"));
    assert!(path.starts_with("/build"));
    assert!(path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .starts_with("main_"));
    assert_eq!(path.extension().unwrap(), "o");
}

#[test]
fn test_object_path_is_unique_per_source_path() {
    let p1 = CompilerBase::object_path(Path::new("/src/a/main.cpp"), Path::new("/build"));
    let p2 = CompilerBase::object_path(Path::new("/src/b/main.cpp"), Path::new("/build"));
    assert_ne!(p1, p2);
}

#[test]
fn test_object_path_preserves_source_extension_before_o() {
    let cpp = CompilerBase::object_path(Path::new("/src/main.cpp"), Path::new("/build"));
    let c = CompilerBase::object_path(Path::new("/src/startup.c"), Path::new("/build"));
    let asm = CompilerBase::object_path(Path::new("/src/vector.S"), Path::new("/build"));

    assert!(cpp.to_string_lossy().ends_with(".cpp.o"));
    assert!(c.to_string_lossy().ends_with(".c.o"));
    assert!(asm.to_string_lossy().ends_with(".S.o"));
}

#[test]
fn test_prepare_flags_for_exec_strips_escaped_quotes() {
    let flags = vec![
        r#"-DARDUINO_BOARD=\"ESP32_DEV\""#.to_string(),
        r#"-DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\""#.to_string(),
        r#"-DIDF_VER=\"v5.3.2\""#.to_string(),
    ];
    let result = prepare_flags_for_exec(flags);
    assert_eq!(result[0], r#"-DARDUINO_BOARD="ESP32_DEV""#);
    assert_eq!(result[1], r#"-DMBEDTLS_CONFIG_FILE="mbedtls/esp_config.h""#);
    assert_eq!(result[2], r#"-DIDF_VER="v5.3.2""#);
}

#[test]
fn test_prepare_flags_for_exec_preserves_normal_flags() {
    let flags = vec![
        "-DPLATFORMIO".to_string(),
        "-DF_CPU=16000000L".to_string(),
        "-I/usr/include".to_string(),
        "-c".to_string(),
        "-Wall".to_string(),
    ];
    let result = prepare_flags_for_exec(flags.clone());
    assert_eq!(result, flags);
}

#[test]
fn test_prepare_flags_for_exec_empty() {
    let result = prepare_flags_for_exec(Vec::new());
    assert!(result.is_empty());
}

#[test]
fn test_prepare_flags_and_response_file_produce_same_define_value() {
    // Both paths must produce the same define value for GCC.
    // Given input: -DFOO=\"bar\"
    // - prepare_flags_for_exec → -DFOO="bar" (argv: GCC sees FOO = "bar")
    // - write_response_file → '-DFOO="bar"' (response file: GCC sees FOO = "bar")
    let input = r#"-DFOO=\"bar\""#.to_string();

    // Direct exec path
    let exec_result = prepare_flags_for_exec(vec![input.clone()]);
    assert_eq!(exec_result[0], r#"-DFOO="bar""#);

    // Response file path — sync test uses the blocking bridge.
    let tmp = tempfile::TempDir::new().unwrap();
    let rsp =
        fbuild_core::response_file::write_response_file_blocking(&[input], tmp.path(), "test")
            .unwrap();
    let content = std::fs::read_to_string(rsp).unwrap();
    // Response file wraps in single quotes with unescaped "
    assert_eq!(content, r#"'-DFOO="bar"'"#);
}

#[test]
fn test_response_file_preserves_bare_quoted_define_value() {
    // nRF52 MCU config provides ARDUINO_BSP_VERSION as a bare quoted JSON
    // value. Windows response files must preserve those quotes, otherwise
    // GCC sees 1.6.1 as a numeric token and fails with "too many decimal
    // points in number" in Adafruit nRF52 debug.cpp.
    let input = r#"-DARDUINO_BSP_VERSION="1.6.1""#.to_string();

    let tmp = tempfile::TempDir::new().unwrap();
    let rsp =
        fbuild_core::response_file::write_response_file_blocking(&[input], tmp.path(), "test")
            .unwrap();
    let content = std::fs::read_to_string(rsp).unwrap();
    assert_eq!(content, r#"'-DARDUINO_BSP_VERSION="1.6.1"'"#);
}

#[test]
fn test_response_file_dir_prefers_output_sibling_tmp() {
    let output = Path::new("C:/work/build/src/main.cpp.o");
    let fallback = Path::new("C:/temp");
    assert_eq!(
        response_file_dir(output, fallback),
        PathBuf::from("C:/work/build/src/tmp")
    );
}

#[test]
fn test_response_file_dir_falls_back_without_output_parent() {
    let output = Path::new("main.o");
    let fallback = Path::new("C:/temp");
    assert_eq!(
        response_file_dir(output, fallback),
        PathBuf::from("C:/temp")
    );
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
fn test_needs_rebuild_when_depfile_dependency_is_newer() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = tmp.path().join("main.cpp");
    let header = tmp.path().join("config.h");
    let obj = tmp.path().join("main.cpp.o");
    let dep = tmp.path().join("main.cpp.d");

    std::fs::write(&src, "#include \"config.h\"\n").unwrap();
    std::fs::write(&header, "#define X 1\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&obj, "obj").unwrap();
    std::fs::write(
        &dep,
        format!(
            "{}: {} {}\n",
            obj.display(),
            src.display(),
            header.display()
        ),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&header, "#define X 2\n").unwrap();

    assert!(CompilerBase::needs_rebuild(&src, &obj));
}

#[test]
fn test_needs_rebuild_uses_depfile_when_dependencies_are_current() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = tmp.path().join("main.cpp");
    let header = tmp.path().join("config.h");
    let obj = tmp.path().join("main.cpp.o");
    let dep = tmp.path().join("main.cpp.d");

    std::fs::write(&src, "#include \"config.h\"\n").unwrap();
    std::fs::write(&header, "#define X 1\n").unwrap();
    std::fs::write(
        &dep,
        format!(
            "{}: {} {}\n",
            obj.display(),
            src.display(),
            header.display()
        ),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&obj, "obj").unwrap();

    assert!(!CompilerBase::needs_rebuild(&src, &obj));
}

/// FastLED/fbuild#951: compiles run with cwd = the project workspace
/// (see `zccache::compile_cwd_from_output`), so gcc's `-MMD` depfiles
/// list *relative* prerequisites. The staleness walk runs inside the
/// long-lived daemon whose process cwd is unrelated — resolving those
/// prerequisites against the process cwd made `metadata()` fail and
/// `.unwrap_or(true)` marked every TU stale, recompiling the whole
/// sketch + core variant on no-change rebuilds (~103 s per build).
#[test]
fn test_needs_rebuild_resolves_relative_depfile_deps_against_workspace() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ws = tmp.path();
    let src_dir = ws.join("src");
    let build_dir = ws.join(".fbuild/build/demo/release/src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&build_dir).unwrap();

    let src = src_dir.join("main.cpp");
    let header = src_dir.join("config.h");
    let obj = build_dir.join("main.cpp.o");
    let dep = build_dir.join("main.cpp.d");

    std::fs::write(&src, "#include \"config.h\"\n").unwrap();
    std::fs::write(&header, "#define X 1\n").unwrap();
    // Relative prerequisites, exactly as gcc -MMD emits them when the
    // compile cwd is the workspace root.
    std::fs::write(
        &dep,
        ".fbuild/build/demo/release/src/main.cpp.o: src/main.cpp src/config.h\n",
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&obj, "obj").unwrap();

    // Everything is current — must NOT rebuild even though the deps are
    // relative and the test process cwd is nowhere near the workspace.
    assert!(!CompilerBase::needs_rebuild(&src, &obj));

    // And a genuinely newer relative dep must still trigger a rebuild.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&header, "#define X 2\n").unwrap();
    assert!(CompilerBase::needs_rebuild(&src, &obj));
}

#[test]
fn test_needs_rebuild_when_command_hash_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = tmp.path().join("main.cpp");
    let obj = tmp.path().join("main.cpp.o");
    let stamp = tmp.path().join("main.cpp.cmdhash");

    std::fs::write(&src, "int main() { return 0; }\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&obj, "obj").unwrap();
    std::fs::write(&stamp, "old-signature").unwrap();

    assert!(CompilerBase::needs_rebuild_with_signature(
        &src,
        &obj,
        Some("new-signature")
    ));
}

#[test]
fn test_build_rebuild_signature_ignores_absolute_compiler_path() {
    let flags = vec!["-Os".to_string(), "-mmcu=atmega328p".to_string()];
    let sig_a =
        build_rebuild_signature(Path::new("/opt/toolchains/a/bin/avr-gcc"), &flags, &[], &[]);
    let sig_b = build_rebuild_signature(
        Path::new("/home/runner/.fbuild/packages/toolchain-atmelavr/bin/avr-gcc"),
        &flags,
        &[],
        &[],
    );

    assert_eq!(sig_a, sig_b);
}

#[test]
fn test_build_rebuild_signature_changes_when_compiler_name_changes() {
    let flags = vec!["-Os".to_string()];
    let sig_a =
        build_rebuild_signature(Path::new("/tmp/toolchains/a/bin/avr-gcc"), &flags, &[], &[]);
    let sig_b = build_rebuild_signature(
        Path::new("/tmp/toolchains/a/bin/xtensa-gcc"),
        &flags,
        &[],
        &[],
    );

    assert_ne!(sig_a, sig_b);
}

#[test]
fn test_build_rebuild_signature_ignores_attached_include_root() {
    let flags_a = vec![
        "-I/tmp/ws-a/project/include".to_string(),
        "-I/home/runner/.fbuild/packages/framework-arduinoavr/cores/arduino".to_string(),
    ];
    let flags_b = vec![
        "-I/tmp/ws-b/project/include".to_string(),
        "-I/Users/runner/.fbuild/packages/framework-arduinoavr/cores/arduino".to_string(),
    ];

    let sig_a =
        build_rebuild_signature(Path::new("/tmp/ws-a/tool/bin/avr-gcc"), &flags_a, &[], &[]);
    let sig_b =
        build_rebuild_signature(Path::new("/tmp/ws-b/tool/bin/avr-gcc"), &flags_b, &[], &[]);

    assert_eq!(sig_a, sig_b);
}

#[test]
fn test_build_rebuild_signature_ignores_split_path_flag_values() {
    let flags_a = vec![
        "-isystem".to_string(),
        "/tmp/ws-a/project/sdk/include".to_string(),
    ];
    let flags_b = vec![
        "-isystem".to_string(),
        "/tmp/ws-b/project/sdk/include".to_string(),
    ];

    let sig_a = build_rebuild_signature(
        Path::new("/tmp/ws-a/tool/bin/xtensa-gcc"),
        &flags_a,
        &[],
        &[],
    );
    let sig_b = build_rebuild_signature(
        Path::new("/tmp/ws-b/tool/bin/xtensa-gcc"),
        &flags_b,
        &[],
        &[],
    );

    assert_eq!(sig_a, sig_b);
}

#[test]
fn test_build_rebuild_signature_changes_when_include_suffix_changes() {
    let flags_a = vec!["-I/tmp/ws-a/project/include".to_string()];
    let flags_b = vec!["-I/tmp/ws-a/project/generated".to_string()];

    let sig_a =
        build_rebuild_signature(Path::new("/tmp/ws-a/tool/bin/avr-gcc"), &flags_a, &[], &[]);
    let sig_b =
        build_rebuild_signature(Path::new("/tmp/ws-a/tool/bin/avr-gcc"), &flags_b, &[], &[]);

    assert_ne!(sig_a, sig_b);
}

#[test]
fn test_build_rebuild_signature_changes_when_non_path_flag_changes() {
    let flags_a = vec!["-Os".to_string()];
    let flags_b = vec!["-O2".to_string()];

    let sig_a =
        build_rebuild_signature(Path::new("/tmp/ws-a/tool/bin/avr-gcc"), &flags_a, &[], &[]);
    let sig_b =
        build_rebuild_signature(Path::new("/tmp/ws-a/tool/bin/avr-gcc"), &flags_b, &[], &[]);

    assert_ne!(sig_a, sig_b);
}

#[test]
fn test_depfile_own_mtime_does_not_force_rebuild() {
    // Regression for FastLED/fbuild#957: gcc writes the `.d` AFTER the `.o`, so
    // on a cold build the depfile is always slightly newer than its object.
    // That must NOT be treated as stale — only the real prerequisites the
    // depfile lists (source + headers) determine staleness. Before the fix,
    // this returned `true` and forced every TU to recompile once on the first
    // rebuild after a cold build.
    use filetime::{set_file_mtime, FileTime};

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let src = dir.join("src.cpp");
    let hdr = dir.join("hdr.h");
    let obj = dir.join("obj.o");
    let dep = dir.join("obj.d");
    std::fs::write(&src, "int main(){}").unwrap();
    std::fs::write(&hdr, "#define X 1").unwrap();
    std::fs::write(&obj, b"obj").unwrap();
    // Relative prereqs (resolved against `base`) — avoids Windows drive-colon
    // ambiguity in the depfile parser.
    std::fs::write(&dep, "obj.o: src.cpp hdr.h\n").unwrap();

    // Prerequisites OLDEST, object newer, depfile NEWEST (exactly as gcc emits).
    let base = FileTime::from_unix_time(1_000_000, 0);
    set_file_mtime(&src, base).unwrap();
    set_file_mtime(&hdr, base).unwrap();
    set_file_mtime(&obj, FileTime::from_unix_time(1_000_100, 0)).unwrap();
    set_file_mtime(&dep, FileTime::from_unix_time(1_000_200, 0)).unwrap();

    let object_time = std::fs::metadata(&obj).unwrap().modified().unwrap();
    let stale = dependency_is_newer_than_object(&dep, object_time, Some(dir)).unwrap();
    assert!(
        !stale,
        "depfile newer than object must not force a rebuild (#957)"
    );
}

#[test]
fn test_depfile_newer_prerequisite_still_forces_rebuild() {
    // Complement: a real header edit (a prerequisite newer than the object) IS
    // stale — the fix must not weaken genuine staleness detection.
    use filetime::{set_file_mtime, FileTime};

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let hdr = dir.join("hdr.h");
    let obj = dir.join("obj.o");
    let dep = dir.join("obj.d");
    std::fs::write(&hdr, "#define X 2").unwrap();
    std::fs::write(&obj, b"obj").unwrap();
    std::fs::write(&dep, "obj.o: hdr.h\n").unwrap();

    set_file_mtime(&obj, FileTime::from_unix_time(1_000_100, 0)).unwrap();
    // Header edited AFTER the object was built.
    set_file_mtime(&hdr, FileTime::from_unix_time(1_000_300, 0)).unwrap();

    let object_time = std::fs::metadata(&obj).unwrap().modified().unwrap();
    let stale = dependency_is_newer_than_object(&dep, object_time, Some(dir)).unwrap();
    assert!(
        stale,
        "a prerequisite newer than the object must force a rebuild"
    );
}
