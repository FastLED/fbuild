//! Size-parity smoke test for FastLED/fbuild#1432: an ESP32-S3 Blink built by
//! fbuild must not produce a larger flash image than PlatformIO builds from the
//! same `platformio.ini`.
//!
//! Both builds pin the same pioarduino platform release, so the comparison
//! measures fbuild's build pipeline rather than a framework version change.
//!
//! `#[ignore]`-marked because it needs PlatformIO (`pio` on PATH, or the path
//! in `FBUILD_PARITY_PIO`) and downloads the ESP32 toolchain and framework for
//! both tools. `.github/workflows/esp32s3-size-parity.yml` runs it once a day:
//!
//! ```text
//! soldr cargo test -p fbuild-build --test it esp32s3_size_parity:: -- --ignored --nocapture
//! ```

use std::fs;
use std::path::Path;

use fbuild_build::{BuildOrchestrator, BuildParams, compile_backend};
use fbuild_core::BuildProfile;
use fbuild_core::path::NormalizedPath;

const ENV_NAME: &str = "esp32s3";

/// The pioarduino release both builds pin (FastLED's `generic-esp` pin).
const PLATFORM_URL: &str = "https://github.com/pioarduino/platform-espressif32/releases/download/55.03.35/platform-espressif32.zip";

/// 15-min wall-clock cap for `--ignored` real-toolchain tests (FastLED/fbuild#806).
const REAL_BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

/// The orchestrator compiles through the process-wide compile backend
/// (FastLED/fbuild#800), which only the daemon wires at startup.
async fn install_test_compile_backend() {
    static INSTALL: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    INSTALL
        .get_or_init(|| async {
            let backend = compile_backend::CompileBackend::start()
                .await
                .expect("compile backend starts for size parity test");
            compile_backend::install_global(backend);
        })
        .await;
}

fn write_blink_project(project_dir: &Path) {
    fs::write(
        project_dir.join("platformio.ini"),
        format!(
            "[env:{ENV_NAME}]\nplatform = {PLATFORM_URL}\nboard = esp32-s3-devkitc-1\nframework = arduino\n"
        ),
    )
    .unwrap();

    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("main.cpp"),
        "\
#include <Arduino.h>

void setup() {
  pinMode(2, OUTPUT);
}

void loop() {
  digitalWrite(2, HIGH);
  delay(1000);
  digitalWrite(2, LOW);
  delay(1000);
}
",
    )
    .unwrap();
}

/// How many times PlatformIO is attempted before the parity check gives up.
const PLATFORMIO_ATTEMPTS: u32 = 3;

/// Backoff between PlatformIO attempts, multiplied by the attempt number.
const RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

/// True when PlatformIO's combined output shows a transient download/network
/// fault -- a release-CDN 5xx, a reset connection, a timeout -- rather than a
/// build defect.
///
/// The 2026-11-08 nightly (run 42464103663) failed with `Got the unrecognized
/// status code '504'` while fetching `esp_install-v5.3.4.zip` from GitHub, so a
/// CDN hiccup read as a size-parity regression and turned `main` red. Such
/// failures are retried; a compiler error or a `.pio` build error is not.
fn is_transient_download_failure(output: &str) -> bool {
    const TRANSIENT_MARKERS: &[&str] = &[
        "unrecognized status code '5",
        "HTTP Error 50",
        "Gateway Timeout",
        "Bad Gateway",
        "Service Unavailable",
        "Connection reset",
        "Connection aborted",
        "Read timed out",
        "timed out",
        "Temporary failure in name resolution",
        "No route to host",
        "IncompleteRead",
        "RemoteDisconnected",
    ];
    TRANSIENT_MARKERS
        .iter()
        .any(|marker| output.contains(marker))
}

/// One PlatformIO invocation's result.
struct AttemptOutcome {
    success: bool,
    exit_code: i32,
    output: String,
}

/// Run `attempt_fn` until it succeeds, it fails for a non-transient reason, or
/// the attempt budget is spent. Returns the attempts made and the last outcome.
///
/// Split out of [`build_with_platformio`] so the retry policy itself is
/// testable without PlatformIO, a toolchain download, or a network fault.
async fn run_with_transient_retry<F, Fut>(
    attempts: u32,
    backoff: std::time::Duration,
    mut attempt_fn: F,
) -> (u32, AttemptOutcome)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = AttemptOutcome>,
{
    let mut attempt: u32 = 1;
    loop {
        let outcome = attempt_fn().await;
        if outcome.success {
            return (attempt, outcome);
        }
        if attempt >= attempts || !is_transient_download_failure(&outcome.output) {
            return (attempt, outcome);
        }
        let delay = backoff * attempt;
        println!(
            "PlatformIO attempt {attempt}/{attempts} failed on a transient download fault; \
             retrying in {}s",
            delay.as_secs()
        );
        tokio::time::sleep(delay).await;
        attempt += 1;
    }
}

/// Build with PlatformIO and return the directory holding its artifacts.
///
/// A transient download failure is retried (`PLATFORMIO_ATTEMPTS`, backoff
/// `RETRY_BACKOFF * attempt`): the ESP32 platform and toolchain are fetched
/// from GitHub release assets on every run, so an external CDN fault must not
/// be reported as a firmware-size regression.
async fn build_with_platformio(project_dir: &Path) -> NormalizedPath {
    let pio = std::env::var("FBUILD_PARITY_PIO").unwrap_or_else(|_| "pio".to_string());
    let project = project_dir.to_str().expect("temp project path is UTF-8");
    let (attempt, outcome) =
        run_with_transient_retry(PLATFORMIO_ATTEMPTS, RETRY_BACKOFF, || async {
            // Same budget as the fbuild build: a stalled PlatformIO build must
            // not hold the job past it (FastLED/fbuild#806).
            let output = fbuild_core::subprocess::run_command(
                &[pio.as_str(), "run", "-e", ENV_NAME, "-d", project],
                None,
                None,
                Some(REAL_BUILD_TIMEOUT),
            )
            .await
            .unwrap_or_else(|e| panic!("failed to run PlatformIO ({pio}): {e}"));
            AttemptOutcome {
                success: output.success(),
                exit_code: output.exit_code,
                output: format!("{}\n{}", output.stdout, output.stderr),
            }
        })
        .await;
    assert!(
        outcome.success,
        "PlatformIO build failed (attempt {attempt}/{PLATFORMIO_ATTEMPTS}, exit {}):\n{}",
        outcome.exit_code, outcome.output
    );
    NormalizedPath::from(project_dir.join(".pio/build").join(ENV_NAME))
}

/// Build with fbuild's ESP32 orchestrator and return its artifact directory.
async fn build_with_fbuild(project_dir: &Path) -> NormalizedPath {
    let build_dir = project_dir.join(format!(
        "{}/{}/{ENV_NAME}/release",
        fbuild_paths::FBUILD_DIR_NAME,
        fbuild_paths::BUILD_DIR_NAME
    ));
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        shrink: Default::default(),
        env_name: ENV_NAME.to_string(),
        clean_all: false,
        clean_only: false,
        clean: true,
        profile: BuildProfile::Release,
        build_dir: build_dir.clone(),
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
        caller_path: None,
    };
    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = tokio::time::timeout(REAL_BUILD_TIMEOUT, orchestrator.build(&params))
        .await
        .expect("fbuild build exceeded the real-toolchain budget (FastLED/fbuild#806)")
        .expect("fbuild build should succeed");
    assert!(result.success, "fbuild build should report success");
    NormalizedPath::from(build_dir)
}

/// Non-debug ELF sections with their sizes, for a readable failure report.
fn section_sizes(elf: &Path) -> Vec<(String, u64)> {
    use object::{Object, ObjectSection};
    let bytes = fs::read(elf).unwrap_or_else(|e| panic!("read {}: {e}", elf.display()));
    let file = object::File::parse(&*bytes).expect("parse ELF");
    file.sections()
        .filter(|s| s.size() > 0)
        .filter_map(|s| {
            let name = s.name().ok()?;
            (!name.starts_with(".debug") && !name.is_empty()).then(|| (name.to_string(), s.size()))
        })
        .collect()
}

fn report(label: &str, artifacts: &Path) -> u64 {
    let bin = artifacts.join("firmware.bin");
    let size = fs::metadata(&bin)
        .unwrap_or_else(|e| panic!("{label} firmware.bin missing at {}: {e}", bin.display()))
        .len();
    println!("{label}: firmware.bin = {size} B");
    for (name, bytes) in section_sizes(&artifacts.join("firmware.elf")) {
        println!("  {name:<24} {bytes:>10}");
    }
    size
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires PlatformIO and downloads the ESP32 toolchain (~hundreds of MB) twice"]
async fn esp32s3_blink_is_no_larger_than_platformio() {
    install_test_compile_backend().await;
    // Separate project dirs so neither tool sees the other's build output.
    let pio_tmp = tempfile::TempDir::new().unwrap();
    let fbuild_tmp = tempfile::TempDir::new().unwrap();
    write_blink_project(pio_tmp.path());
    write_blink_project(fbuild_tmp.path());

    let pio_artifacts = build_with_platformio(pio_tmp.path()).await;
    let fbuild_artifacts = build_with_fbuild(fbuild_tmp.path()).await;

    let pio_size = report("PlatformIO", &pio_artifacts);
    let fbuild_size = report("fbuild", &fbuild_artifacts);
    assert!(
        fbuild_size <= pio_size,
        "fbuild's ESP32-S3 Blink firmware.bin is {} B larger than PlatformIO's \
         (fbuild={fbuild_size} B, PlatformIO={pio_size} B); see the section tables above",
        fbuild_size - pio_size
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        AttemptOutcome, PLATFORMIO_ATTEMPTS, RETRY_BACKOFF, is_transient_download_failure,
        run_with_transient_retry,
    };

    fn outcome(success: bool, output: &str) -> AttemptOutcome {
        AttemptOutcome {
            success,
            exit_code: if success { 0 } else { 1 },
            output: output.to_string(),
        }
    }

    /// Verbatim tail of the 2026-11-08 nightly failure (run 42464103663).
    const PIO_504: &str = "Error installing tool-esp_install: Got the unrecognized status code \
                           '504' when downloaded https://github.com/pioarduino/esp_install/\
                           releases/download/v5.3.4/esp_install-v5.3.4.zip";

    #[test]
    fn classifies_the_nightly_504_as_transient() {
        assert!(is_transient_download_failure(PIO_504));
    }

    #[test]
    fn classifies_transport_faults_as_transient() {
        for output in [
            "ConnectionResetError: Connection reset by peer",
            "requests.exceptions.ConnectionError: Read timed out.",
            "urllib.error.URLError: <urlopen error [Errno -3] Temporary failure in name resolution>",
            "HTTP Error 503: Service Unavailable",
            "http.client.RemoteDisconnected: Remote end closed connection without response",
        ] {
            assert!(is_transient_download_failure(output), "{output}");
        }
    }

    #[test]
    fn does_not_retry_a_compiler_error() {
        let output = "src/main.cpp:4:3: error: 'pinMode' was not declared in this scope";
        assert!(!is_transient_download_failure(output));
    }

    #[test]
    fn does_not_retry_an_unknown_environment() {
        let output = "Error: Unknown environment names 'esp32s2'. Valid names are 'esp32s3'";
        assert!(!is_transient_download_failure(output));
    }

    /// The policy that matters: an outage the retry absorbs still yields a
    /// green parity check instead of a red `main`.
    #[tokio::test]
    async fn retries_a_transient_504_and_then_succeeds() {
        let mut calls = 0u32;
        let (attempts, last) =
            run_with_transient_retry(PLATFORMIO_ATTEMPTS, Duration::from_millis(1), || {
                calls += 1;
                let this = calls;
                async move {
                    if this == 1 {
                        outcome(false, PIO_504)
                    } else {
                        outcome(true, "PlatformIO build finished")
                    }
                }
            })
            .await;
        assert_eq!(attempts, 2, "one retry should have absorbed the 504");
        assert!(last.success);
    }

    #[tokio::test]
    async fn exhausts_the_budget_on_a_persistent_outage() {
        let mut calls = 0u32;
        let (attempts, last) =
            run_with_transient_retry(PLATFORMIO_ATTEMPTS, Duration::from_millis(1), || {
                calls += 1;
                async move { outcome(false, PIO_504) }
            })
            .await;
        assert_eq!(attempts, PLATFORMIO_ATTEMPTS);
        assert!(!last.success);
        assert_eq!(calls, PLATFORMIO_ATTEMPTS);
    }

    #[tokio::test]
    async fn does_not_retry_a_build_failure() {
        let mut calls = 0u32;
        let (attempts, last) =
            run_with_transient_retry(PLATFORMIO_ATTEMPTS, Duration::from_millis(1), || {
                calls += 1;
                async move {
                    outcome(
                        false,
                        "src/main.cpp:4:3: error: 'pinMode' was not declared in this scope",
                    )
                }
            })
            .await;
        assert_eq!(
            attempts, 1,
            "a compiler error must fail on the first attempt"
        );
        assert_eq!(calls, 1);
        assert!(!last.success);
    }

    /// The job's own 60-min timeout is the real budget: one attempt would
    /// reintroduce the flake, an unbounded one could push the run past it.
    const _: () = assert!(PLATFORMIO_ATTEMPTS >= 2 && PLATFORMIO_ATTEMPTS <= 3);
    const _: () = assert!(RETRY_BACKOFF.as_secs() >= 10);
}
