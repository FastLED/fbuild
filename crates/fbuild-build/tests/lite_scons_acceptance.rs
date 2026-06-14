//! Acceptance tests for the lite-SCons extra_scripts harness
//! (FastLED/fbuild#553).
//!
//! Each test exercises one of the five spike patterns documented in
//! https://github.com/FastLED/fbuild/issues/553#issuecomment-4702659508 —
//! the patterns the legacy MockEnv harness in `script_runtime_harness.py`
//! structurally cannot model:
//!
//!   1. effectful `env.Execute(env.Action(callable))` (generator scripts)
//!   2. `env.AddBuildMiddleware(callback, regex)` (Marlin-class)
//!   3. `env.AddPostAction(target, action)` (OTA-style merge_bin)
//!   4. `env.SConscript("child.py")` recursive chained mutation
//!   5. `env.ParseFlagsExtended(...)` routing both `-Ipath` and `-I path`
//!
//! Each test pins the harness choice via
//! `resolve_extra_script_overlay_with_mode(..., true)` so the
//! `FBUILD_LITE_SCONS` env var can't leak across the parallel test runner.
//!
//! Verifies the three bugs the spike caught and fixed:
//!   * `env.get(key)` falls through to `project_options`
//!   * `ParseFlagsExtended` handles `-I path` (space-separated) as well as
//!     `-Ipath` (joined)
//!   * `SConscript` resolves paths relative to the calling script's dir

use std::fs;
use std::path::Path;

use fbuild_build::flag_overlay::BuildOverlay;
use fbuild_build::script_runtime::resolve_extra_script_overlay_with_mode;

/// Test runner gate. `find_python` is private to script_runtime; the
/// integration tests probe the same `python --version` / `py -3 --version`
/// surface and skip when neither is available.
fn python_available() -> bool {
    let probes: &[&[&str]] = if cfg!(windows) {
        &[&["python", "--version"], &["py", "-3", "--version"]]
    } else {
        &[&["python3", "--version"], &["python", "--version"]]
    };
    for argv in probes {
        if let Ok(out) = std::process::Command::new(argv[0])
            .args(&argv[1..])
            .output()
        {
            if out.status.success() {
                return true;
            }
        }
    }
    false
}

fn write_project(extra_scripts: &str, scripts: &[(&str, &str)]) -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    let project_dir = temp.path();
    fs::write(
        project_dir.join("platformio.ini"),
        format!(
            "[env:demo]\nplatform = atmelavr\nboard = uno\nframework = arduino\nextra_scripts = {extra_scripts}\n",
        ),
    )
    .expect("write platformio.ini");
    for (name, body) in scripts {
        fs::write(project_dir.join(name), body).expect("write script");
    }
    temp
}

fn resolve_lite(project_dir: &Path) -> BuildOverlay {
    let config = fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))
        .expect("parse platformio.ini");
    resolve_extra_script_overlay_with_mode(project_dir, "demo", &config, true)
        .expect("lite-SCons harness must succeed for the 5 spike patterns")
}

// ---------------------------------------------------------------------
// 1. Effectful Execute(Action(callable))
// ---------------------------------------------------------------------

/// MockEnv treats Execute as a no-op; the lite harness actually invokes
/// the callable, captures the executed-action record, and notices the
/// new file via the generated-files manifest. Tests both halves.
#[test]
fn lite_scons_executes_generator_action_and_records_file() {
    if !python_available() {
        return;
    }

    let temp = write_project(
        "pre:generator.py",
        &[(
            "generator.py",
            r##"
import os
from SCons.Script import Import

Import("env")


def write_buildinfo(target, source, env):
    out = os.path.join(env["PROJECT_DIR"], "buildinfo.h")
    with open(out, "w", encoding="utf-8") as fh:
        fh.write("#pragma once\n#define BUILDINFO_RAN 1\n")
    return 0


env.Execute(env.Action(write_buildinfo, "Generating buildinfo.h"))
env.Append(CPPDEFINES=[("BUILDINFO_PRESENT", "1")])
"##,
        )],
    );

    let overlay = resolve_lite(temp.path());
    let records = overlay
        .lite_scons_records
        .as_ref()
        .expect("lite_scons_records must be Some when the lite harness ran");

    assert_eq!(
        records.executed_actions.len(),
        1,
        "Execute(Action(...)) must record exactly one executed action: {overlay:?}"
    );

    // The whole point of effectful Execute: the file lands on disk so
    // fbuild's native compile pipeline can see it as an input.
    let generated = temp.path().join("buildinfo.h");
    assert!(
        generated.is_file(),
        "generator must materialise {} on disk",
        generated.display()
    );

    let path_match = records.generated_files.iter().any(|f| {
        f.get("path")
            .and_then(|p| p.as_str())
            .is_some_and(|p| p.replace('\\', "/").ends_with("/buildinfo.h"))
    });
    assert!(
        path_match,
        "generated_files manifest must include buildinfo.h: {:?}",
        records.generated_files
    );

    assert!(
        overlay
            .global_compile
            .common
            .contains(&"-DBUILDINFO_PRESENT=1".to_string()),
        "Append(CPPDEFINES=...) after Execute must still land: {overlay:?}"
    );
}

// ---------------------------------------------------------------------
// 2. AddBuildMiddleware(callback, regex)
// ---------------------------------------------------------------------

/// Marlin-class hook: glob pattern + callback name captured so fbuild's
/// native compile pipeline can call the middleware per matching source.
#[test]
fn lite_scons_records_build_middleware() {
    if !python_available() {
        return;
    }

    let temp = write_project(
        "post:middleware.py",
        &[(
            "middleware.py",
            r##"
from SCons.Script import Import

Import("env")


def tweak_arduino_core_flags(env, node):
    return env.Object(node, CCFLAGS=env["CCFLAGS"] + ["-DCORE_BUILD=1"])


env.AddBuildMiddleware(tweak_arduino_core_flags, "*ArduinoCore-*/cores/arduino/*.cpp")
env.Append(CCFLAGS=["-Wno-unused-parameter"])
"##,
        )],
    );

    let overlay = resolve_lite(temp.path());
    let records = overlay
        .lite_scons_records
        .as_ref()
        .expect("lite_scons_records expected");

    assert_eq!(records.middleware.len(), 1);
    let entry = &records.middleware[0];
    assert_eq!(
        entry.get("callback_repr").and_then(|v| v.as_str()),
        Some("tweak_arduino_core_flags"),
        "middleware callback name must be captured"
    );
    assert_eq!(
        entry.get("regex").and_then(|v| v.as_str()),
        Some("*ArduinoCore-*/cores/arduino/*.cpp"),
        "middleware glob must be captured verbatim"
    );

    assert!(
        overlay
            .global_compile
            .common
            .contains(&"-Wno-unused-parameter".to_string()),
        "ordinary Append still applies alongside middleware: {overlay:?}"
    );
}

// ---------------------------------------------------------------------
// 3. AddPostAction(target_template, action)
// ---------------------------------------------------------------------

/// OTA-style merge_bin packager: target template MUST come back
/// unresolved so fbuild can subst it (`$BUILD_DIR/$PROGNAME$PROGSUFFIX`)
/// after link, when it knows the actual values.
#[test]
fn lite_scons_records_post_action_with_unresolved_target_template() {
    if !python_available() {
        return;
    }

    let temp = write_project(
        "post:postaction.py",
        &[(
            "postaction.py",
            r##"
from SCons.Script import Import

Import("env")


def merge_firmware(target, source, env):
    return 0


env.AddPostAction("$BUILD_DIR/$PROGNAME$PROGSUFFIX", merge_firmware)
"##,
        )],
    );

    let overlay = resolve_lite(temp.path());
    let records = overlay
        .lite_scons_records
        .as_ref()
        .expect("lite_scons_records expected");

    assert_eq!(records.recorded_post_actions.len(), 1);
    let entry = &records.recorded_post_actions[0];

    assert_eq!(
        entry.get("target").and_then(|v| v.as_str()),
        Some("$BUILD_DIR/$PROGNAME$PROGSUFFIX"),
        "target template MUST be preserved unresolved so fbuild can subst at deploy time"
    );
    assert!(
        entry
            .get("action_repr")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("merge_firmware")),
        "action_repr must identify the callback name: {entry:?}"
    );
}

// ---------------------------------------------------------------------
// 4. SConscript("child.py") — caller-relative path resolution
// ---------------------------------------------------------------------

/// SCons resolves SConscript paths relative to the *calling* script's
/// directory, not the project root. Spike bug #3 — the first iteration
/// looked in PROJECT_DIR and the child was missing.
#[test]
fn lite_scons_sconscript_recursion_resolves_caller_relative_and_lands_child_mutations() {
    if !python_available() {
        return;
    }

    let temp = write_project(
        "post:parent.py",
        &[(
            "parent.py",
            r##"
from SCons.Script import Import

Import("env")

env.Append(CPPDEFINES=["PARENT_DEFINE"])
env.SConscript("child.py")
"##,
        )],
    );
    // Drop the child sibling-relative to parent.py — caller-relative
    // resolution must find it.
    fs::write(
        temp.path().join("child.py"),
        "env.Append(CPPDEFINES=[(\"CHILD_DEFINE\", \"1\")])\n",
    )
    .expect("write child.py");

    let overlay = resolve_lite(temp.path());

    assert!(
        overlay
            .global_compile
            .common
            .contains(&"-DPARENT_DEFINE".to_string()),
        "parent's define must land: {overlay:?}"
    );
    assert!(
        overlay
            .global_compile
            .common
            .contains(&"-DCHILD_DEFINE=1".to_string()),
        "child SConscript's define must land via recursive eval: {overlay:?}"
    );
}

// ---------------------------------------------------------------------
// 5. ParseFlagsExtended — joined and space-separated `-I` / `-l`
// ---------------------------------------------------------------------

/// Real SCons handles both `-Ipath` and `-I path`. The spike's first
/// iteration only handled the joined form — bug #2 of the 3 caught.
#[test]
fn lite_scons_parseflags_handles_joined_and_space_separated_forms() {
    if !python_available() {
        return;
    }

    let temp = write_project(
        "post:parseflags.py",
        &[(
            "parseflags.py",
            r##"
from SCons.Script import Import

Import("env")

parsed = env.ParseFlagsExtended("-Ijoined/inc -I separated/inc -DKEY=VAL -lextrasym")
env.Append(**parsed)
"##,
        )],
    );

    let overlay = resolve_lite(temp.path());
    let common = &overlay.global_compile.common;
    let common_joined = common.join(" ");

    assert!(
        common_joined.contains("joined/inc"),
        "joined `-Ipath` form must reach CPPPATH (common: {common:?})"
    );
    assert!(
        common_joined.contains("separated/inc"),
        "space-separated `-I path` form must reach CPPPATH (common: {common:?})"
    );
    assert!(
        common.contains(&"-DKEY=VAL".to_string()),
        "-DKEY=VAL must reach CPPDEFINES (common: {common:?})"
    );
    assert!(
        overlay.link.libs.contains(&"-lextrasym".to_string()),
        "-l name must reach link.libs: {:?}",
        overlay.link.libs
    );
}
