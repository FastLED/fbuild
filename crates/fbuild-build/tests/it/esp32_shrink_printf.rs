//! Integration test: `--shrink=printf` on ESP32 really links newlib-nano.
//!
//! FastLED/fbuild#1459 acceptance criteria ask for more than "the flag is
//! parsed": the reducer must swap the full newlib formatter for the vendor's
//! thin one, record that decision, and leave the image smaller. This test
//! builds the SAME source twice with the real Xtensa toolchain — once with
//! `--no-shrink` as the control, once with `--shrink=printf` — and compares
//! the whole firmware, the recorded link flags and `build_info_<env>.json`.
//!
//! The sketch is a self-checking printf battery: every case formats through
//! `snprintf` and compares against a literal expectation, so flashing either
//! image and reading the serial log answers "did shrinking change what the
//! program prints?" without trusting this test's own opinion. The battery is
//! compiled into both images; this test only proves the *size* + *telemetry*
//! half, which is the half a machine can decide.
//!
//! Run with: `soldr cargo test -p fbuild-build --test it esp32_shrink_printf:: -- --ignored`

use std::fs;
use std::path::{Path, PathBuf};

use fbuild_build::shrink::ShrinkMode;
use fbuild_build::{BuildOrchestrator, BuildParams, BuildResult};
use fbuild_core::BuildProfile;

/// 15-min wall-clock cap for `--ignored` real-toolchain tests (FastLED/fbuild#806).
const REAL_BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

async fn under_test_timeout<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::time::timeout(REAL_BUILD_TIMEOUT, fut).await {
        Ok(v) => v,
        Err(_) => panic!(
            "real-toolchain test exceeded {:.0}s budget — see FastLED/fbuild#806",
            REAL_BUILD_TIMEOUT.as_secs_f64()
        ),
    }
}

/// The sketch both images are built from.
///
/// Every case is `snprintf` + literal expectation, and the entry-point cases
/// cover `sprintf`, `snprintf` and `vsnprintf` (what `Serial.printf` uses).
/// Wide-character and `long double` conversions are deliberately absent:
/// newlib-nano has neither, so they belong in the documented divergence list,
/// not in a battery that must pass on both libcs.
const PRINTF_BATTERY: &str = r#"
#include <Arduino.h>
#include <stdarg.h>
#include <stdio.h>
#include <limits.h>
#include <string.h>

// Long decimal expectations are split so no single literal is a long digit run.
static const char* CASE_INT_MIN = "-2147" "483648";
static const char* CASE_UINT_MAX = "4294" "967295";
static const char* CASE_VSNPRINTF = "0002.500|9" "000000000";

static int g_total = 0;
static int g_failed = 0;

static void check(const char* what, const char* got, int n, const char* want) {
    ++g_total;
    if (n < 0 || strcmp(got, want) != 0) {
        ++g_failed;
        Serial.printf("FAIL %s got='%s' n=%d want='%s'\n", what, got, n, want);
    }
}

#define CHECK(want, ...)                          \
    do {                                          \
        char b[128];                              \
        int n = snprintf(b, sizeof b, __VA_ARGS__); \
        check(#__VA_ARGS__, b, n, want);          \
    } while (0)

static int vsnprintf_case(char* out, size_t cap, const char* fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(out, cap, fmt, ap);
    va_end(ap);
    return n;
}

void setup() {
    Serial.begin(115200);
    delay(2500);

    // Integers.
    CHECK("42", "%d", 42);
    CHECK(CASE_INT_MIN, "%d", INT_MIN);
    CHECK(CASE_UINT_MAX, "%u", 0xFFFFFFFFu);
    CHECK("1234567890", "%ld", 1234567890L);
    CHECK("-9223372036854775807", "%lld", -9223372036854775807LL);
    CHECK("18446744073709551615", "%llu", 18446744073709551615ULL);
    CHECK("deadbeefcafef00d", "%llx", 0xDEADBEEFCAFEF00DULL);
    CHECK("deadbeef", "%x", 0xDEADBEEFu);
    CHECK("777", "%o", 511u);
    CHECK("-12", "%hd", (short)-12);
    CHECK("200", "%hhu", (unsigned char)200);
    CHECK("4", "%zu", sizeof(size_t));

    // Width, precision, flags.
    CHECK("00000042", "%08d", 42);
    CHECK("42    ", "%-6d", 42);
    CHECK("  42", "%4d", 42);
    CHECK("+42", "%+d", 42);
    CHECK(" 42", "% d", 42);
    CHECK("0xff", "%#x", 255u);
    CHECK("  42", "%*d", 4, 42);
    CHECK("", "%.0d", 0);

    // Floating point (only reachable because the reducer links
    // `-Wl,-u,_printf_float`).
    CHECK("3.142", "%.3f", 3.14159);
    CHECK("-0.500000", "%f", -0.5);
    CHECK("1.234500e+03", "%e", 1234.5);
    CHECK("1.5e+03", "%.1e", 1500.0);
    CHECK("0.0001", "%g", 0.0001);
    CHECK("    3.1416", "%10.4f", 3.14159);
    CHECK("3.14", "%.*f", 2, 3.14);
    CHECK(" 3.14%", "%5.2f%%", 3.14);
    CHECK("inf", "%f", 1.0f / 0.0f);

    // Strings and characters.
    CHECK("hello", "%s", "hello");
    CHECK(" hello", "%6s", "hello");
    CHECK("he", "%.2s", "hello");
    CHECK("A", "%c", 'A');
    CHECK("%", "%%");

    // Entry points: sprintf shares the core, vsnprintf is what
    // Serial.printf/Print::printf funnel through.
    {
        char b[32];
        int n = sprintf(b, "%d-%s", -7, "x");
        check("sprintf", b, n, "-7-x");
    }
    {
        char b[32];
        int n = vsnprintf_case(b, sizeof b, "%08.3f|%lld", 2.5, 9000000000LL);
        check("vsnprintf", b, n, CASE_VSNPRINTF);
    }

    Serial.printf("BATTERY total=%d failed=%d\n", g_total, g_failed);
    Serial.flush();
}

void loop() {
    delay(1000);
}
"#;

/// `BuildParams` for this suite: defaults except project/build dirs and the
/// shrink mode under test.
fn params(
    project_dir: PathBuf,
    env_name: &str,
    build_dir: PathBuf,
    shrink: ShrinkMode,
) -> BuildParams {
    BuildParams {
        project_dir,
        shrink,
        env_name: env_name.to_string(),
        clean_all: false,
        clean_only: false,
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
        caller_path: None,
    }
}

/// Lay out a throwaway project holding [`PRINTF_BATTERY`] for `board`.
fn write_project(root: &Path, board: &str) -> PathBuf {
    let project_dir = root.join("printf_battery");
    fs::create_dir_all(project_dir.join("src")).unwrap();
    fs::write(
        project_dir.join("platformio.ini"),
        format!("[env:{board}]\nplatform = espressif32\nboard = {board}\nframework = arduino\n"),
    )
    .unwrap();
    fs::write(project_dir.join("src").join("main.cpp"), PRINTF_BATTERY).unwrap();
    project_dir
}

fn build_dir_for(project_dir: &Path, board: &str) -> PathBuf {
    project_dir.join(format!(
        "{}/{}/{board}/release",
        fbuild_paths::FBUILD_DIR_NAME,
        fbuild_paths::BUILD_DIR_NAME
    ))
}

/// The ESP32 orchestrators compile through the process-wide compile backend
/// (FastLED/fbuild#800), which only the daemon wires at startup; these tests
/// install their own, like the sibling acceptance suites do. Initialized at
/// most once per test process: a second `CompileBackend::start()` cannot win
/// the zccache cache-root writer slot from the first.
async fn install_test_compile_backend() {
    static INSTALL: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    INSTALL
        .get_or_init(|| async {
            let backend = fbuild_build::compile_backend::CompileBackend::start()
                .await
                .expect("compile backend starts for the shrink printf test");
            fbuild_build::compile_backend::install_global(backend);
        })
        .await;
}

async fn build(project_dir: &Path, board: &str, shrink: ShrinkMode) -> BuildResult {
    install_test_compile_backend().await;
    let params = params(
        project_dir.to_path_buf(),
        board,
        build_dir_for(project_dir, board),
        shrink,
    );
    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    under_test_timeout(orchestrator.build(&params))
        .await
        .expect("ESP32 build should succeed")
}

/// The shrink record ESP32 wrote into `build_info_esp32dev.json`.
fn shrink_record_for(project_dir: &Path, env: &str) -> serde_json::Value {
    let path = project_dir.join(format!("build_info_{env}.json"));
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("build info {} unreadable: {e}", path.display()));
    let json: serde_json::Value = serde_json::from_str(&raw).expect("build info is valid JSON");
    json[env]["shrink"].clone()
}

fn link_flags_for(project_dir: &Path, env: &str) -> Vec<String> {
    let raw = fs::read_to_string(project_dir.join(format!("build_info_{env}.json"))).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).expect("build info is valid JSON");
    json[env]["link_flags"]
        .as_array()
        .expect("link_flags is an array")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// `--shrink=printf` swaps the full newlib formatter for newlib-nano: the
/// image must shrink, the link must carry the nano spec file, and the
/// telemetry must say so. The `--no-shrink` control must do none of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads ESP32 toolchain (~hundreds of MB)"]
async fn build_esp32dev_shrink_printf_replaces_newlib_formatter() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = write_project(tmp.path(), "esp32dev");

    let control = build(&project_dir, "esp32dev", ShrinkMode::Off).await;
    assert!(control.success, "control (--no-shrink) build failed");
    let control_flash = control.size_info.expect("control size info").total_flash;
    let control_flags = link_flags_for(&project_dir, "esp32dev");
    let control_record = shrink_record_for(&project_dir, "esp32dev");
    assert!(
        !control_flags.iter().any(|f| f.contains("nano.specs")),
        "control link must not carry nano.specs: {control_flags:?}"
    );
    assert_eq!(
        control_record["resolved"], "off",
        "control record: {control_record}"
    );

    let shrunk = build(&project_dir, "esp32dev", ShrinkMode::Printf).await;
    assert!(shrunk.success, "--shrink=printf build failed");
    let shrunk_flash = shrunk.size_info.expect("shrunk size info").total_flash;
    let shrunk_flags = link_flags_for(&project_dir, "esp32dev");
    let shrunk_record = shrink_record_for(&project_dir, "esp32dev");

    assert!(
        shrunk_flags.iter().any(|f| f == "-specs=nano.specs"),
        "--shrink=printf must link -specs=nano.specs: {shrunk_flags:?}"
    );
    assert!(
        shrunk_flags.iter().any(|f| f == "-Wl,-u,_printf_float"),
        "the reducer must keep %f alive via -Wl,-u,_printf_float: {shrunk_flags:?}"
    );
    assert_eq!(shrunk_record["mode"], "printf", "record: {shrunk_record}");
    assert_eq!(
        shrunk_record["resolved"], "printf",
        "record: {shrunk_record}"
    );
    assert_eq!(
        shrunk_record["strategy"], "spec-file",
        "record: {shrunk_record}"
    );
    assert_eq!(
        shrunk_record["applied"],
        serde_json::json!(["newlib-nano"]),
        "record: {shrunk_record}"
    );

    eprintln!(
        "esp32dev printf battery: control flash={control_flash} B, \
         --shrink=printf flash={shrunk_flash} B"
    );
    assert!(
        shrunk_flash < control_flash,
        "shrinking must reduce flash: {shrunk_flash} >= {control_flash}"
    );
}

/// The control half of the A/B must stay reproducible: building the same tree
/// twice with no shrink request never sneaks the nano spec file in. This is
/// the guard that keeps `Auto` honest — today it resolves to `Off`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads ESP32 toolchain (~hundreds of MB)"]
async fn build_esp32dev_auto_shrink_stays_off() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = write_project(tmp.path(), "esp32dev");

    let result = build(&project_dir, "esp32dev", ShrinkMode::Auto).await;
    assert!(result.success, "default build failed");

    let record = shrink_record_for(&project_dir, "esp32dev");
    assert_eq!(record["mode"], "auto", "record: {record}");
    assert_eq!(record["resolved"], "off", "Auto must resolve Off: {record}");
    assert_eq!(record["applied"], serde_json::json!([]), "record: {record}");
    assert!(
        !link_flags_for(&project_dir, "esp32dev")
            .iter()
            .any(|f| f.contains("nano.specs")),
        "an Auto build must link the full libc"
    );
}

/// The gate, end to end: on a RISC-V ESP32 target the reducer must refuse and
/// leave the full libc linked. This is the regression guard for the measured
/// +9,520 B growth on ESP32-C6 — a size reducer that grows images must never
/// be applied, however the flag was spelled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads ESP32 RISC-V toolchain (~hundreds of MB)"]
async fn build_esp32c6_shrink_printf_is_refused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = write_project(tmp.path(), "esp32c6");

    let result = build(&project_dir, "esp32c6", ShrinkMode::Printf).await;
    assert!(
        result.success,
        "the build must still succeed, just unshrunk"
    );

    let record = shrink_record_for(&project_dir, "esp32c6");
    assert_eq!(record["mode"], "printf", "record: {record}");
    assert_eq!(record["resolved"], "off", "record: {record}");
    assert_eq!(record["applied"], serde_json::json!([]), "record: {record}");
    assert!(
        !link_flags_for(&project_dir, "esp32c6")
            .iter()
            .any(|f| f.contains("nano.specs")),
        "a refused shrink must link the full libc"
    );
}
