//! Acceptance gate for #205 AC#6: teensy41 cold library-selection budget.
//!
//! AC#6 verbatim from the #205 issue body:
//!
//! > Cold run of library selection on `teensy41` project <= 200 ms on a
//! > typical CI runner.
//!
//! This test materializes a real Teensyduino install via
//! `fbuild_packages::library::TeensyCores::ensure_installed`, enumerates its
//! ~40 framework libraries with `get_framework_libraries`, and times a SINGLE
//! call to the uncached resolver
//! `fbuild_library_select::resolve(seeds, search_paths, libraries)` against a
//! minimal teensy41 Blink fixture. The test asserts the elapsed wall-clock
//! time is <= 200 ms — the 200 ms budget is the AC#6 contract.
//!
//! "Cold" means the resolver itself, NOT the cache layer: AC#6 is about the
//! actual BFS+attribution cost of `resolve()`, the function on top of which
//! `resolve_cached` is built. Hitting the warm cache short-circuits this work
//! and is covered by AC#5 (the bench-fastled-examples warm threshold gate);
//! AC#6 ensures the cold path is itself fast enough that a cache-cold project
//! does not pay an unbounded resolution tax.
//!
//! Uses the stm32_acceptance.rs / teensy30_acceptance.rs inline-tempdir
//! pattern so the committed `tests/platform/teensy41/` fixture stays untouched
//! and no scratch artifacts land in the repo.
//!
//! Run with:
//! `soldr cargo test -p fbuild-build --release --test teensy41_acceptance \
//!     -- --ignored teensy41_cold_library_selection_meets_205_ac6 --nocapture`
//!
//! Marked `#[ignore]` because it downloads Teensyduino on the first run
//! (cached after) — too heavy for default `cargo test`.

use std::time::Instant;

use fbuild_library_select::resolve;
use fbuild_packages::library::TeensyCores;
use fbuild_packages::Package;

#[test]
#[ignore = "downloads Teensyduino + arm-gcc; CI-only"]
fn teensy41_cold_library_selection_meets_205_ac6() {
    // Inline tempdir project — same root-cause-isolation pattern as
    // stm32_acceptance.rs / teensy30_acceptance.rs. AC#6 needs only the
    // sketch on disk; we don't run a full build, only the resolver.
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path();

    std::fs::write(
        project_dir.join("platformio.ini"),
        "[env:teensy41]\n\
         platform = teensy\n\
         board = teensy41\n\
         framework = arduino\n",
    )
    .unwrap();

    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    // Minimal Blink. The resolver only walks #include directives reachable
    // from this seed, so keeping the sketch tiny mirrors the AC#6 statement
    // "Cold run of library selection on teensy41 project" — there's no
    // additional framework-lib reference here, the resolver still has to
    // load + scan every Teensyduino library to decide they're NOT needed.
    std::fs::write(
        src_dir.join("main.ino"),
        "#include <Arduino.h>\n\
         void setup() { pinMode(LED_BUILTIN, OUTPUT); }\n\
         void loop() {\n\
           digitalWrite(LED_BUILTIN, HIGH);\n\
           delay(500);\n\
           digitalWrite(LED_BUILTIN, LOW);\n\
           delay(500);\n\
         }\n",
    )
    .unwrap();

    // Materialize Teensyduino. Idempotent — cached across runs on the
    // CI runner once the package has been downloaded once.
    let teensy_cores = TeensyCores::new(project_dir);
    let framework_dir =
        Package::ensure_installed(&teensy_cores).expect("Teensyduino must install for AC#6 gate");
    println!(
        "AC#6 teensy41 framework installed at {}",
        framework_dir.display()
    );

    // Real teensy41 framework library set (~40 libraries: SPI, Wire,
    // OctoWS2811, FNET, Snooze, RadioHead, mbedtls, ...). Listing them is
    // an O(libraries-dir) directory walk; that work is NOT what AC#6
    // measures, so it happens outside the timed window below.
    let libraries = teensy_cores.get_framework_libraries();
    assert!(
        !libraries.is_empty(),
        "AC#6: TeensyCores::get_framework_libraries must return at least one \
         library after install — got 0, which means the Teensyduino install \
         layout has changed"
    );
    println!(
        "AC#6 teensy41 framework libraries discovered: {}",
        libraries.len()
    );

    // Seeds + search paths: match what the teensy orchestrator passes to
    // resolve() in normal operation. The orchestrator's path
    // (framework_libs.rs -> resolve_framework_library_sources_from_libraries)
    // walks the project's src/include/lib roots and uses every source file
    // as a seed. For AC#6 we just have the single Blink .ino, and the
    // project search paths are the src/ dir.
    let seeds = vec![src_dir.join("main.ino")];
    let search_paths = vec![src_dir.clone(), project_dir.to_path_buf()];

    // Time a SINGLE uncached resolve() — this is what AC#6 measures.
    let start = Instant::now();
    let selection = resolve(&seeds, &search_paths, &libraries);
    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_secs_f64() * 1_000.0;

    println!(
        "AC#6 cold resolve: {:.2} ms (budget 200.00 ms), selected {} library(ies)",
        elapsed_ms,
        selection.required_libraries.len()
    );

    assert!(
        elapsed_ms <= 200.0,
        "AC#6: cold resolve must finish in <= 200 ms; got {:.2} ms",
        elapsed_ms
    );
}
