//! Integration test: build a real AVR sketch using the full pipeline.
//!
//! Downloads avr-gcc + Arduino core (cached after first run), compiles a
//! minimal blink sketch, and validates the output firmware.hex.
//!
//! Run with: `soldr cargo test -p fbuild-build --test avr_build -- --ignored`

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use filetime::{set_file_mtime, FileTime};
use tar::{Archive, Builder};

use fbuild_build::{BuildOrchestrator, BuildParams};
use fbuild_core::BuildProfile;

fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var("USERPROFILE").expect("USERPROFILE not set"))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(std::env::var("HOME").expect("HOME not set"))
    }
}

/// Verify stem/hash cache path format produces readable, unique paths.
#[test]
fn cache_paths_stem_hash() {
    use fbuild_packages::cache::{hash_url, url_stem};

    // Toolchain: stem from base URL
    assert_eq!(
        url_stem("https://downloads.arduino.cc/tools"),
        "arduino-tools"
    );
    assert_eq!(
        hash_url("https://downloads.arduino.cc/tools"),
        "08e1a7271edb2765"
    );

    // Arduino core: stem from GitHub URL
    let core_url = "https://github.com/arduino/ArduinoCore-avr/archive/refs/tags/1.8.6.tar.gz";
    assert_eq!(url_stem(core_url), "arduino-ArduinoCore-avr");
    assert_eq!(hash_url(core_url), "6e608239126ea48b");

    // Full paths: toolchains/arduino-tools/08e1a7271edb2765/7.3.0/
    let tmp = tempfile::TempDir::new().unwrap();
    let cache =
        fbuild_packages::Cache::with_cache_root(tmp.path(), tmp.path().join("cache").as_path());
    let tc_path = cache.get_toolchain_path("https://downloads.arduino.cc/tools", "7.3.0");
    let tc_str = tc_path.to_string_lossy();
    assert!(tc_str.contains("arduino-tools"));
    assert!(tc_str.contains("08e1a7271edb2765"));
    assert!(tc_str.contains("7.3.0"));
}

/// Build the uno_minimal test project from the Python fbuild repo.
///
/// This test requires:
/// - Internet access (first run only, then cached)
/// - ~/dev/fbuild/tests/uno_minimal/ to exist (Python fbuild repo)
#[test]
#[ignore]
fn build_uno_minimal() {
    let project_dir = home_dir().join("dev/fbuild/tests/uno_minimal");

    if !project_dir.exists() {
        eprintln!(
            "SKIP: {} does not exist (need Python fbuild repo)",
            project_dir.display()
        );
        return;
    }

    // Use a temp build dir so we don't pollute the Python project.
    // `params.build_dir` is the resolved env-rooted dir per
    // `BuildLayout::resolve()`.
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join(".fbuild/build/uno/release");

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "uno".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: false,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
    };

    let orchestrator = fbuild_build::avr::orchestrator::AvrOrchestrator;
    let result = orchestrator
        .build(&params)
        .expect("AVR build should succeed");

    assert!(result.success, "build should report success");

    // Verify firmware.hex was produced
    let hex_path = result
        .firmware_path
        .as_ref()
        .expect("should produce hex file");
    assert!(hex_path.exists(), "firmware.hex should exist");

    let hex_content = fs::read_to_string(hex_path).unwrap();

    // Intel HEX format: starts with ':', ends with EOF record
    assert!(
        hex_content.starts_with(':'),
        "hex file should start with ':'"
    );
    assert!(
        hex_content.contains(":00000001FF"),
        "hex file should contain EOF record"
    );

    // Firmware size sanity check (blink sketch is ~900-7000 bytes of flash)
    let hex_size = hex_path.metadata().unwrap().len();
    assert!(hex_size > 100, "hex file too small: {} bytes", hex_size);
    assert!(hex_size < 50_000, "hex file too large: {} bytes", hex_size);

    // Verify ELF was produced
    let elf_path = result.elf_path.as_ref().expect("should produce elf file");
    assert!(elf_path.exists(), "firmware.elf should exist");

    // Verify size info
    let size = result.size_info.as_ref().expect("should have size info");
    assert!(size.text > 0, "text section should be non-zero");
    assert!(size.total_flash > 0, "total flash should be non-zero");
    assert!(
        size.total_flash < 32256,
        "flash usage should be under Uno limit"
    );

    eprintln!("Build succeeded in {:.1}s", result.build_time_secs);
    eprintln!(
        "Size: flash={}/{} ({:.1}%) ram={}/{} ({:.1}%)",
        size.total_flash,
        size.max_flash.unwrap_or(0),
        size.flash_percent().unwrap_or(0.0),
        size.total_ram,
        size.max_ram.unwrap_or(0),
        size.ram_percent().unwrap_or(0.0),
    );
}

/// Compare our build output against the Python fbuild's cached output.
#[test]
#[ignore]
fn compare_with_python_output() {
    let project_dir = home_dir().join("dev/fbuild/tests/uno_minimal");

    let python_hex = project_dir.join(".fbuild/build/uno/release/firmware.hex");
    if !python_hex.exists() {
        eprintln!(
            "SKIP: Python build output not found at {}",
            python_hex.display()
        );
        return;
    }

    // Build with Rust
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join(".fbuild/build/uno/release");

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "uno".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: false,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: false,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
    };

    let orchestrator = fbuild_build::avr::orchestrator::AvrOrchestrator;
    let result = orchestrator.build(&params).expect("build should succeed");
    let rust_hex = result.firmware_path.expect("should produce hex");

    let python_content = fs::read_to_string(&python_hex).unwrap();
    let rust_content = fs::read_to_string(&rust_hex).unwrap();

    if python_content == rust_content {
        eprintln!("firmware.hex is BYTE-IDENTICAL to Python output");
    } else {
        // Even if not identical, compare sizes as a sanity check
        let python_size = python_hex.metadata().unwrap().len();
        let rust_size = rust_hex.metadata().unwrap().len();
        eprintln!(
            "firmware.hex differs: Python={} bytes, Rust={} bytes",
            python_size, rust_size
        );

        // Size should be in the same ballpark (within 20%)
        let ratio = rust_size as f64 / python_size as f64;
        assert!(
            (0.8..1.2).contains(&ratio),
            "size ratio {:.2} is outside acceptable range",
            ratio
        );

        eprintln!("WARNING: hex files differ but sizes are similar — likely different compiler flag ordering");
    }
}

/// Build a self-contained test project (no dependency on Python fbuild repo).
#[test]
#[ignore]
fn build_self_contained_blink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path();

    // Create platformio.ini
    fs::write(
        project_dir.join("platformio.ini"),
        "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
    )
    .unwrap();

    // Create src/blink.ino
    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("blink.ino"),
        "\
void setup() {
  pinMode(13, OUTPUT);
}

void loop() {
  digitalWrite(13, HIGH);
  delay(1000);
  digitalWrite(13, LOW);
  delay(1000);
}
",
    )
    .unwrap();

    let build_dir = project_dir.join(".fbuild/build/uno/release");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "uno".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: false,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
    };

    let orchestrator = fbuild_build::avr::orchestrator::AvrOrchestrator;
    let result = orchestrator
        .build(&params)
        .expect("self-contained build should succeed");

    assert!(result.success);
    let hex_path = result.firmware_path.expect("should produce hex");
    assert!(hex_path.exists());

    let content = fs::read_to_string(&hex_path).unwrap();
    assert!(content.starts_with(':'));
    assert!(content.contains(":00000001FF"));

    let size = result.size_info.expect("should have size info");
    eprintln!(
        "Self-contained build: flash={} ram={} time={:.1}s",
        size.total_flash, size.total_ram, result.build_time_secs
    );
}

const UNO_PLATFORMIO_INI: &str =
    "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n";

const UNO_BLINK_INO: &str = "\
void setup() {
  pinMode(13, OUTPUT);
}

void loop() {
  digitalWrite(13, HIGH);
  delay(1000);
  digitalWrite(13, LOW);
  delay(1000);
}
";

fn scaffold_uno_blink(project_dir: &Path) {
    fs::write(project_dir.join("platformio.ini"), UNO_PLATFORMIO_INI).unwrap();
    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("blink.ino"), UNO_BLINK_INO).unwrap();
}

fn uno_build_params(project_dir: &Path, build_dir: PathBuf, clean: bool) -> BuildParams {
    BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "uno".to_string(),
        clean,
        profile: BuildProfile::Release,
        build_dir,
        verbose: false,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: false,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
    }
}

fn tar_directory(root: &Path) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    builder.follow_symlinks(false);
    builder.append_dir_all("proj", root).unwrap();
    builder.into_inner().unwrap()
}

fn untar_into(bytes: &[u8], dest: &Path) {
    fs::create_dir_all(dest).unwrap();
    let mut archive = Archive::new(Cursor::new(bytes));
    archive.set_preserve_mtime(false);
    archive.unpack(dest).unwrap();
}

fn stomp_mtimes(root: &Path, mtime: FileTime) {
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if entry.file_type().is_file() {
            // Best-effort: some restored files (e.g. read-only artifacts on Windows)
            // may refuse mtime updates; the test's correctness does not depend on every
            // file being stomped, only on enough source-tree files having mtimes that
            // differ from the originals.
            let _ = set_file_mtime(entry.path(), mtime);
        }
    }
}

fn fingerprint_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".fbuild/build/uno/release/build_fingerprint.json")
}

/// RAII guard for an env var: sets it on construction, restores the previous
/// value on drop. Test-only helper so the env var leak does not pollute other
/// tests that share this process.
#[allow(dead_code)]
struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

#[allow(dead_code)]
impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

/// End-to-end regression gate for #147: build → tar → restore at a different parent →
/// rebuild must hit the warm fast path. Integration-level companion to the hermetic
/// unit test in `tests/cache_survives_tar_extract.rs`.
///
/// Failure modes this catches that the unit test cannot:
///   * Orchestrator state persisted outside the watch set (e.g., a side file the
///     fast-path predicate forgot about) that gets invalidated by tar-extract.
///   * Fast-path predicate bugs that pass the per-layer unit tests but reject a
///     legitimately-cached `BuildResult`.
///   * Absolute paths baked into a build artifact that the fast-path check actually
///     reads (compile DB, build_fingerprint.json) but the unit test does not cover.
///
/// Pre-FastLED/fbuild#800 the test set `FBUILD_NO_ZCCACHE=1` to skip the
/// wrapper-binary path; that env var is gone now. The embedded zccache
/// service runs unconditionally — the fast-path predicate this test
/// covers (build_fingerprint.json + watch-set stamps) is independent
/// of zccache and still owned by fbuild itself.
///
/// Gated `#[ignore]` because it downloads avr-gcc + Arduino-AVR core (cached globally
/// after first run, but still adds 30s+ to first invocation).
#[test]
#[ignore]
fn cache_survives_tar_extract_uno() {
    let tmp_a = tempfile::TempDir::new().unwrap();
    let proj_a = tmp_a.path().join("proj");
    fs::create_dir_all(&proj_a).unwrap();
    scaffold_uno_blink(&proj_a);

    let orchestrator = fbuild_build::avr::orchestrator::AvrOrchestrator;

    let cold_result = orchestrator
        .build(&uno_build_params(
            &proj_a,
            proj_a.join(".fbuild/build/uno/release"),
            true,
        ))
        .expect("cold AVR build should succeed");
    assert!(cold_result.success, "cold build should report success");
    assert!(
        !cold_result.message.contains("reused cached artifacts"),
        "cold build hit fast path unexpectedly; test setup invariant broken: {}",
        cold_result.message,
    );
    assert!(
        fingerprint_path(&proj_a).exists(),
        "cold build did not persist build_fingerprint.json at {} -- \
         orchestrator never reached persist_fast_path_success.",
        fingerprint_path(&proj_a).display()
    );
    let cold_hex = fs::read(
        cold_result
            .firmware_path
            .as_ref()
            .expect("cold build should produce hex"),
    )
    .unwrap();
    let cold_time = cold_result.build_time_secs;
    eprintln!("cold build: {:.2}s", cold_time);

    // Sanity gate: a same-project warm rebuild MUST hit the fast path before we
    // bother testing the tar-extract case. If this asserts, the test is failing
    // because of an orchestrator/fast-path bug unrelated to tar restoration.
    let same_project_warm = orchestrator
        .build(&uno_build_params(
            &proj_a,
            proj_a.join(".fbuild/build/uno/release"),
            false,
        ))
        .expect("same-project warm build should succeed");
    assert!(
        same_project_warm
            .message
            .contains("reused cached artifacts"),
        "same-project warm rebuild did not hit fast path -- the regression is in the \
         fast-path predicate itself, not in tar-restore handling. message: {}",
        same_project_warm.message,
    );
    eprintln!(
        "same-project warm: {:.2}s (fast-path hit confirmed)",
        same_project_warm.build_time_secs
    );

    let tarball = tar_directory(&proj_a);

    let tmp_b = tempfile::TempDir::new().unwrap();
    let relocation_root = tmp_b.path().join("nested").join("run-b").join("deeper");
    fs::create_dir_all(&relocation_root).unwrap();
    untar_into(&tarball, &relocation_root);
    let proj_b = relocation_root.join("proj");
    assert!(
        proj_b.join("src/blink.ino").exists(),
        "tar restore left no src/blink.ino at {}",
        proj_b.display()
    );
    assert!(
        proj_b.join(".fbuild/build").exists(),
        "tar restore left no .fbuild/build/ at {}",
        proj_b.display()
    );
    assert_ne!(
        proj_a.parent(),
        proj_b.parent(),
        "test setup invariant: restored project must live under a different parent path"
    );

    stomp_mtimes(&proj_b, FileTime::from_unix_time(1_577_836_800, 0)); // 2020-01-01 UTC

    let warm_result = orchestrator
        .build(&uno_build_params(
            &proj_b,
            proj_b.join(".fbuild/build/uno/release"),
            false,
        ))
        .expect("warm AVR build (post tar-extract) should succeed");
    assert!(warm_result.success, "warm build should report success");
    assert!(
        warm_result.message.contains("reused cached artifacts"),
        "warm build did NOT hit fast path -- this is the regression #147 was supposed to \
         prevent. message: {}",
        warm_result.message,
    );

    let warm_hex = fs::read(
        warm_result
            .firmware_path
            .as_ref()
            .expect("warm build should still report a hex path"),
    )
    .unwrap();
    assert_eq!(
        cold_hex, warm_hex,
        "warm build returned a different firmware.hex than the cold build; \
         fast-path artifacts diverged across tar-restore."
    );

    let warm_time = warm_result.build_time_secs;
    eprintln!(
        "warm build (post tar-extract relocation): {:.2}s (cold was {:.2}s)",
        warm_time, cold_time
    );
    assert!(
        warm_time < cold_time,
        "warm build ({:.2}s) was not faster than cold build ({:.2}s); fast-path is not \
         actually short-circuiting the compile/link stack.",
        warm_time,
        cold_time,
    );
}
