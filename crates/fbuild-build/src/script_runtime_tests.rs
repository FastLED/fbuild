use super::*;
use crate::flag_overlay::ScriptScopeState;
use std::fs;

fn write_runtime_project(
    extra_scripts: &str,
    script_name: &str,
    script_body: &str,
) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path();
    fs::write(
        project_dir.join("platformio.ini"),
        format!(
            "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
extra_scripts = {}
",
            extra_scripts
        ),
    )
    .unwrap();
    fs::write(project_dir.join(script_name), script_body).unwrap();
    temp
}

fn resolve_runtime_error(project_dir: &Path) -> String {
    let config =
        fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini")).unwrap();
    // Pin to MockEnv (FBUILD_LITE_SCONS off) — these tests assert on
    // MockEnv's hard-fail semantics (Execute / SConscript / unsupported
    // scopes) that the lite-SCons default (#553 step 3) diverges from.
    resolve_extra_script_overlay_with_mode(project_dir, "demo", &config, false)
        .unwrap_err()
        .to_string()
}

#[test]
fn test_cppdefines_to_flags_string_and_kv() {
    let flags = cppdefines_to_flags(&[
        serde_json::Value::String("FOO".to_string()),
        serde_json::json!({"kind": "kv", "key": "BAR", "value": "baz"}),
        serde_json::json!({"kind": "kv", "key": "COUNT", "value": 7}),
    ])
    .unwrap();
    assert_eq!(flags, vec!["-DFOO", "-DBAR=baz", "-DCOUNT=7"]);
}

#[test]
fn test_libs_to_flags_names_and_paths() {
    let project_dir = Path::new("/tmp/project");
    let flags = libs_to_flags(
        project_dir,
        &[
            serde_json::Value::String("m".to_string()),
            serde_json::Value::String("libs/foo.a".to_string()),
        ],
    )
    .unwrap();
    assert_eq!(flags[0], "-lm");
    assert_eq!(
        flags[1],
        absolutize_if_relative(project_dir, "libs/foo.a")
            .to_string_lossy()
            .to_string()
    );
}

#[test]
fn test_scope_to_compile_overlay_maps_common_and_language_flags() {
    let project_dir = Path::new("/tmp/project");
    let scope = ScriptScopeState {
        cppdefines: vec![
            serde_json::Value::String("FOO".to_string()),
            serde_json::json!({"kind": "kv", "key": "BAR", "value": 1}),
        ],
        cpppath: vec![serde_json::Value::String("include".to_string())],
        ccflags: vec![serde_json::Value::String("-Wall".to_string())],
        cflags: vec![serde_json::Value::String("-std=c11".to_string())],
        cxxflags: vec![serde_json::Value::String("-std=gnu++20".to_string())],
        asflags: vec![serde_json::Value::String("-x".to_string())],
        ..Default::default()
    };

    let overlay = scope_to_compile_overlay(project_dir, &scope).unwrap();
    assert!(overlay.common.contains(&"-DFOO".to_string()));
    assert!(overlay.common.contains(&"-DBAR=1".to_string()));
    assert!(overlay.common.contains(&format!(
        "-I{}",
        absolutize_if_relative(project_dir, "include").display()
    )));
    assert!(overlay.common.contains(&"-Wall".to_string()));
    assert_eq!(overlay.c, vec!["-std=c11"]);
    assert_eq!(overlay.cxx, vec!["-std=gnu++20"]);
    assert_eq!(overlay.asm, vec!["-x"]);
}

#[test]
fn test_scope_to_link_overlay_maps_libpath_and_libs() {
    let project_dir = Path::new("/tmp/project");
    let scope = ScriptScopeState {
        linkflags: vec![serde_json::Value::String("-Wl,--gc-sections".to_string())],
        libpath: vec![serde_json::Value::String("lib".to_string())],
        libs: vec![
            serde_json::Value::String("m".to_string()),
            serde_json::Value::String("archives/foo.a".to_string()),
        ],
        ..Default::default()
    };

    let overlay = scope_to_link_overlay(project_dir, &scope).unwrap();
    assert!(overlay.flags.contains(&"-Wl,--gc-sections".to_string()));
    assert!(overlay.flags.contains(&format!(
        "-L{}",
        absolutize_if_relative(project_dir, "lib").display()
    )));
    assert_eq!(overlay.libs[0], "-lm");
    assert_eq!(
        overlay.libs[1],
        absolutize_if_relative(project_dir, "archives/foo.a")
            .to_string_lossy()
            .to_string()
    );
}

#[test]
fn test_resolve_extra_script_overlay_supports_dump_shim() {
    if find_python().is_none() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path();
    fs::write(
        project_dir.join("platformio.ini"),
        "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
extra_scripts = post:dump_test.py
",
    )
    .unwrap();
    fs::write(
        project_dir.join("dump_test.py"),
        "\
Import(\"env\", \"projenv\")
state = env.Dump()
proj_state = projenv.Dump()
if \"CPPDEFINES\" not in state or \"CPPDEFINES\" not in proj_state:
    raise RuntimeError(\"missing dump scopes\")
env.Append(CPPDEFINES=[\"DUMP_SHIM_OK\"])
",
    )
    .unwrap();

    let config =
        fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini")).unwrap();
    // Pinned to MockEnv (see resolve_runtime_overlay note).
    let overlay =
        resolve_extra_script_overlay_with_mode(project_dir, "demo", &config, false).unwrap();
    assert!(overlay
        .global_compile
        .common
        .contains(&"-DDUMP_SHIM_OK".to_string()));
}

#[test]
fn test_resolve_extra_script_overlay_supports_common_noop_scons_helpers() {
    if find_python().is_none() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path();
    fs::write(
        project_dir.join("platformio.ini"),
        "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
extra_scripts = post:helpers_test.py
",
    )
    .unwrap();
    fs::write(
        project_dir.join("helpers_test.py"),
        "\
Import(\"env\")
if env.IsCleanTarget():
    raise RuntimeError(\"unexpected clean target\")
if env.IsIntegrationDump():
    raise RuntimeError(\"unexpected integration dump\")
flattened = env.Flatten([[\"a\"], [\"b\", [\"c\"]]])
if flattened != [\"a\", \"b\", \"c\"]:
    raise RuntimeError(\"unexpected flatten result\")
env.Execute(env.VerboseAction(\"echo noop\", \"noop\"))
env.Append(CPPDEFINES=[\"HELPERS_SHIM_OK\"])
",
    )
    .unwrap();

    let config =
        fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini")).unwrap();
    // Pinned to MockEnv (see resolve_runtime_overlay note).
    let overlay =
        resolve_extra_script_overlay_with_mode(project_dir, "demo", &config, false).unwrap();
    assert!(overlay
        .global_compile
        .common
        .contains(&"-DHELPERS_SHIM_OK".to_string()));
}

#[test]
fn test_resolve_extra_script_overlay_supports_board_config_shim() {
    if find_python().is_none() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path();
    fs::write(
        project_dir.join("platformio.ini"),
        "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
extra_scripts = post:board_config_test.py
",
    )
    .unwrap();
    fs::write(
        project_dir.join("board_config_test.py"),
        "\
Import(\"env\")
board = env.BoardConfig()
if board.get(\"build.mcu\") != \"atmega328p\":
    raise RuntimeError(\"unexpected board mcu\")
if board.get(\"build.f_cpu\") != \"16000000L\":
    raise RuntimeError(\"unexpected board f_cpu\")
env.Append(CPPDEFINES=[\"BOARD_CONFIG_SHIM_OK\"])
",
    )
    .unwrap();

    let config =
        fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini")).unwrap();
    // Pinned to MockEnv (see resolve_runtime_overlay note).
    let overlay =
        resolve_extra_script_overlay_with_mode(project_dir, "demo", &config, false).unwrap();
    assert!(overlay
        .global_compile
        .common
        .contains(&"-DBOARD_CONFIG_SHIM_OK".to_string()));
}

#[test]
fn test_resolve_extra_script_overlay_supports_pio_platform_shim() {
    if find_python().is_none() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path();
    fs::write(
        project_dir.join("platformio.ini"),
        "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
extra_scripts = post:pio_platform_test.py
",
    )
    .unwrap();
    fs::write(
        project_dir.join("pio_platform_test.py"),
        "\
Import(\"env\")
platform = env.PioPlatform()
if platform.name != \"atmelavr\":
    raise RuntimeError(\"unexpected platform name\")
if not platform.is_embedded():
    raise RuntimeError(\"expected embedded platform\")
pkg = platform.get_package_dir(\"tool-avrdude\")
if not pkg.endswith(\"tool-avrdude\"):
    raise RuntimeError(\"unexpected package path\")
env.Append(CPPDEFINES=[\"PIO_PLATFORM_SHIM_OK\"])
",
    )
    .unwrap();

    let config =
        fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini")).unwrap();
    // Pinned to MockEnv (see resolve_runtime_overlay note).
    let overlay =
        resolve_extra_script_overlay_with_mode(project_dir, "demo", &config, false).unwrap();
    assert!(overlay
        .global_compile
        .common
        .contains(&"-DPIO_PLATFORM_SHIM_OK".to_string()));
}

#[test]
fn test_resolve_extra_script_overlay_rejects_unsupported_import_name() {
    if find_python().is_none() {
        return;
    }

    let temp = write_runtime_project(
        "post:bad_import_test.py",
        "bad_import_test.py",
        "\
Import(\"board\")
",
    );
    let err = resolve_runtime_error(temp.path());
    assert!(err.contains("Import('board') is not supported"), "{err}");
    assert!(err.contains("Recommendation: use --platformio"), "{err}");
}

#[test]
fn test_resolve_extra_script_overlay_rejects_projenv_in_pre_script() {
    if find_python().is_none() {
        return;
    }

    let temp = write_runtime_project(
        "pre:pre_projenv_test.py",
        "pre_projenv_test.py",
        "\
Import(\"env\", \"projenv\")
",
    );
    let err = resolve_runtime_error(temp.path());
    assert!(
        err.contains("projenv is not available in PRE extra_scripts"),
        "{err}"
    );
    assert!(err.contains("Recommendation: use --platformio"), "{err}");
}

#[test]
fn test_resolve_extra_script_overlay_rejects_unsupported_scope_mutation() {
    if find_python().is_none() {
        return;
    }

    let temp = write_runtime_project(
        "post:unsupported_scope_test.py",
        "unsupported_scope_test.py",
        "\
Import(\"env\")
env.Append(FOO=[\"x\"])
",
    );
    let err = resolve_runtime_error(temp.path());
    assert!(
        err.contains("env.append on unsupported scope 'FOO'"),
        "{err}"
    );
    assert!(err.contains("Recommendation: use --platformio"), "{err}");
}

#[test]
fn test_resolve_extra_script_overlay_rejects_unsupported_script_prefix() {
    if find_python().is_none() {
        return;
    }

    let temp = write_runtime_project(
        "mid:prefix_test.py",
        "prefix_test.py",
        "\
Import(\"env\")
",
    );
    let err = resolve_runtime_error(temp.path());
    assert!(
        err.contains("unsupported extra_scripts prefix 'mid'"),
        "{err}"
    );
    assert!(err.contains("Recommendation: use --platformio"), "{err}");
}

/// Write a project whose `platformio.ini` carries extra `[env:demo]` lines
/// (e.g. `build_type = debug`) alongside a single `extra_scripts` entry.
fn write_runtime_project_with_config(
    env_lines: &str,
    extra_scripts: &str,
    script_name: &str,
    script_body: &str,
) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path();
    fs::write(
        project_dir.join("platformio.ini"),
        format!(
            "\
[env:demo]
platform = atmelavr
board = uno
framework = arduino
{env_lines}extra_scripts = {extra_scripts}
"
        ),
    )
    .unwrap();
    fs::write(project_dir.join(script_name), script_body).unwrap();
    temp
}

fn resolve_runtime_overlay(project_dir: &Path) -> BuildOverlay {
    let config =
        fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini")).unwrap();
    // Pinned to MockEnv (see resolve_runtime_error note above).
    resolve_extra_script_overlay_with_mode(project_dir, "demo", &config, false).unwrap()
}

// ---- SIMPLE tier ------------------------------------------------------

/// Marlin `common-cxxflags.py`-style script: language-specific append,
/// `GetBuildType()` gating, in-place `BUILD_FLAGS` append, and a no-op
/// `AddPostAction`. Source: MarlinFirmware/Marlin buildroot scripts.
#[test]
fn test_shim_simple_marlin_cxxflags_style() {
    if find_python().is_none() {
        return;
    }

    let temp = write_runtime_project_with_config(
        "build_type = debug\n",
        "post:common_cxxflags.py",
        "common_cxxflags.py",
        "\
Import(\"env\")
flags = []
if \"teensy\" not in env[\"PIOENV\"]:
    flags.append(\"-Wno-register\")
env.Append(CXXFLAGS=flags)
if env.GetBuildType() == \"debug\":
    env.Append(CPPDEFINES=[\"MARLIN_DEBUG\"])
env[\"BUILD_FLAGS\"].append(\"-DBOARD_F_CPU=16000000\")
env.AddPostAction(\"$PROGPATH\", lambda *a, **k: None)
",
    );

    let overlay = resolve_runtime_overlay(temp.path());
    assert!(
        overlay
            .global_compile
            .cxx
            .contains(&"-Wno-register".to_string()),
        "{overlay:?}"
    );
    assert!(
        overlay
            .global_compile
            .common
            .contains(&"-DMARLIN_DEBUG".to_string()),
        "{overlay:?}"
    );
    assert!(
        overlay
            .global_compile
            .common
            .contains(&"-DBOARD_F_CPU=16000000".to_string()),
        "BUILD_FLAGS should fold into common compile flags: {overlay:?}"
    );
}

/// Tuple-shaped `CPPDEFINES` appended in place via `__getitem__` must still
/// emit `-Dkey=value`, not a malformed array entry.
#[test]
fn test_shim_simple_inplace_tuple_cppdefine() {
    if find_python().is_none() {
        return;
    }

    let temp = write_runtime_project_with_config(
        "",
        "post:tuple_define.py",
        "tuple_define.py",
        "\
Import(\"env\")
env[\"CPPDEFINES\"].append((\"VERSION\", 7))
env.Append(CPPDEFINES=[\"PLAIN\"])
",
    );

    let overlay = resolve_runtime_overlay(temp.path());
    assert!(
        overlay
            .global_compile
            .common
            .contains(&"-DVERSION=7".to_string()),
        "{overlay:?}"
    );
    assert!(
        overlay
            .global_compile
            .common
            .contains(&"-DPLAIN".to_string()),
        "{overlay:?}"
    );
}

// ---- MEDIUM tier ------------------------------------------------------

/// namf `platformio_script.py`-style script: obtains env via
/// `from SCons.Script import DefaultEnvironment`, reads + rewrites
/// `LINKFLAGS`, and registers a no-op post action.
#[test]
fn test_shim_medium_default_environment_linkflags() {
    if find_python().is_none() {
        return;
    }

    let temp = write_runtime_project_with_config(
        "",
        "post:namf_style.py",
        "namf_style.py",
        "\
from SCons.Script import DefaultEnvironment
env = DefaultEnvironment()
flags = \" \".join(env[\"LINKFLAGS\"])
flags = flags.replace(\"-u _printf_float\", \"\")
env.Replace(LINKFLAGS=flags.split())
env.Append(LINKFLAGS=[\"-Wl,--gc-sections\"])
def after_build(*a, **k):
    pass
env.AddPostAction(\"$BUILD_DIR/firmware.bin\", after_build)
",
    );

    let overlay = resolve_runtime_overlay(temp.path());
    assert!(
        overlay
            .link
            .flags
            .contains(&"-Wl,--gc-sections".to_string()),
        "{overlay:?}"
    );
}

/// m5panel `littlefsbuilder.py`-style script: `env.get()` plus a
/// `Replace` on a non-flag tool scope. The tool scope must be recorded as
/// a note (not a hard failure) while the real flag mutation lands.
#[test]
fn test_shim_medium_nonflag_scope_recorded_not_rejected() {
    if find_python().is_none() {
        return;
    }

    let temp = write_runtime_project_with_config(
        "",
        "post:littlefs.py",
        "littlefs.py",
        "\
Import(\"env\")
env.Replace(MKSPIFFSTOOL=env.get(\"PROJECT_DIR\") + \"/tools/mklittlefs\")
env.Append(CPPDEFINES=[\"LFS_OK\"])
",
    );

    let overlay = resolve_runtime_overlay(temp.path());
    assert!(
        overlay
            .global_compile
            .common
            .contains(&"-DLFS_OK".to_string()),
        "{overlay:?}"
    );
    assert!(
        overlay.notes.iter().any(|n| n.contains("MKSPIFFSTOOL")),
        "non-flag scope should be recorded as a note: {overlay:?}"
    );
}

// ---- COMPLEX tier (graceful refusal / no-op) --------------------------

/// amsreader `generate_includes.py`-style script: a no-op `Execute` of a
/// `VerboseAction`. The script makes no flag mutations; the runtime must
/// succeed, collect nothing, and note the ignored action.
#[test]
fn test_shim_complex_execute_verbose_action_is_noop() {
    if find_python().is_none() {
        return;
    }

    let temp = write_runtime_project_with_config(
        "",
        "post:generate_includes.py",
        "generate_includes.py",
        "\
from SCons.Script import DefaultEnvironment
env = DefaultEnvironment()
env.Execute(env.VerboseAction(\"$PYTHONEXE -m pip install css_html_js_minify\", \"Installing\"))
",
    );

    let overlay = resolve_runtime_overlay(temp.path());
    assert!(
        overlay.global_compile.is_empty() && overlay.project_compile.is_empty(),
        "effectful codegen script should contribute no flags: {overlay:?}"
    );
    assert!(
        overlay.notes.iter().any(|n| n.contains("Execute")),
        "{overlay:?}"
    );
}

/// Marlin `common-dependencies.py`-style script: `SConscript` recursion is
/// structurally unshimmable and must bail with a `--platformio` hint
/// rather than silently producing wrong flags.
#[test]
fn test_shim_complex_sconscript_bails() {
    if find_python().is_none() {
        return;
    }

    let temp = write_runtime_project_with_config(
        "",
        "post:common_dependencies.py",
        "common_dependencies.py",
        "\
Import(\"env\")
env.Append(CPPDEFINES=[\"EARLY\"])
env.SConscript(\"feature.py\", exports=\"env\")
",
    );
    let err = resolve_runtime_error(temp.path());
    assert!(err.contains("SConscript is not supported"), "{err}");
    assert!(err.contains("Recommendation: use --platformio"), "{err}");
}

/// Locks down the post-#553-step-3 flip. The env var name + opt-out
/// vocabulary is the user-facing contract; the lite-vs-MockEnv default
/// is the semantic change. Don't rely on the rest of the test suite
/// to catch a future regression on either dimension — that suite
/// pins through `_with_mode(..., false)` and won't notice if the
/// default silently reverts.
#[test]
fn use_lite_scons_default_and_opt_out_vocabulary() {
    use std::sync::Mutex;
    // env vars are process-global; serialise just this one test.
    static GUARD: Mutex<()> = Mutex::new(());
    let _g = GUARD.lock().unwrap();
    let prev = std::env::var("FBUILD_LITE_SCONS").ok();

    std::env::remove_var("FBUILD_LITE_SCONS");
    assert!(use_lite_scons(), "unset → default lite (#553 step 3 flip)");

    for opt_in_or_unrecognised in ["", "1", "true", "yes", "wat"] {
        std::env::set_var("FBUILD_LITE_SCONS", opt_in_or_unrecognised);
        assert!(
            use_lite_scons(),
            "FBUILD_LITE_SCONS={opt_in_or_unrecognised:?} keeps the lite default"
        );
    }

    for opt_out in ["0", "false", "FALSE", "no", "Off", "  0  "] {
        std::env::set_var("FBUILD_LITE_SCONS", opt_out);
        assert!(
            !use_lite_scons(),
            "FBUILD_LITE_SCONS={opt_out:?} opts out to legacy MockEnv"
        );
    }

    match prev {
        Some(v) => std::env::set_var("FBUILD_LITE_SCONS", v),
        None => std::env::remove_var("FBUILD_LITE_SCONS"),
    }
}
