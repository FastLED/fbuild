//! Phase accounting for the concurrent core/sketch compile region
//! (FastLED/fbuild#1414 item 2).

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use fbuild_core::BuildLog;
use fbuild_core::path::NormalizedPath;
use tokio::sync::Semaphore;

use super::*;
use crate::compiler::CompileResult;

/// Compiler that fabricates a success for every TU after `delay`, so the test
/// measures the region's bookkeeping rather than a toolchain.
struct SlowCompiler {
    gcc: NormalizedPath,
    delay: Duration,
}

#[async_trait::async_trait]
impl Compiler for SlowCompiler {
    async fn compile_one(
        &self,
        _compiler_path: &Path,
        _source: &Path,
        output: &Path,
        _flags: &[String],
        _extra_flags: &[String],
    ) -> Result<CompileResult> {
        tokio::time::sleep(self.delay).await;
        std::fs::write(output, "obj").unwrap();
        Ok(CompileResult {
            success: true,
            object_file: output.to_path_buf(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        })
    }

    fn gcc_path(&self) -> &Path {
        self.gcc.as_path()
    }

    fn gxx_path(&self) -> &Path {
        self.gcc.as_path()
    }

    fn c_flags(&self) -> Vec<String> {
        Vec::new()
    }

    fn cpp_flags(&self) -> Vec<String> {
        Vec::new()
    }

    fn rebuild_signature(&self, source: &Path, _extra: &[String], _out: &Path) -> String {
        source.to_string_lossy().into_owned()
    }
}

/// `(core_sources, sketch_sources)` on disk under a fresh temp tree.
fn fixture(
    core: usize,
    sketch: usize,
) -> (tempfile::TempDir, Vec<NormalizedPath>, Vec<NormalizedPath>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("core")).unwrap();
    std::fs::create_dir_all(root.join("sketch")).unwrap();
    let write = |set: &str, name: &str| {
        let path = root.join(set).join(name);
        std::fs::write(&path, "int x;").unwrap();
        NormalizedPath::new(path)
    };
    let core_sources = (0..core)
        .map(|i| write("core", &format!("core{i}.cpp")))
        .collect();
    let sketch_sources = (0..sketch)
        .map(|i| write("sketch", &format!("sketch{i}.cpp")))
        .collect();
    (dir, core_sources, sketch_sources)
}

/// A hydrated core cache compiles zero core TUs. The core leg must then
/// record ~0 ms instead of inheriting the sketch's compile time from the
/// region both share.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_core_leg_does_not_inherit_the_sketch_time() {
    let (dir, core_sources, sketch_sources) = fixture(0, 1);
    let root = dir.path();
    let compiler = SlowCompiler {
        gcc: NormalizedPath::new(root.join("gcc")),
        delay: Duration::from_millis(60),
    };
    let overlay = LanguageExtraFlags::default();
    let build_log = Mutex::new(BuildLog::new());
    let gate = Arc::new(Semaphore::new(4));
    let mut perf = PerfTimer::new("compile-phases-test");
    perf.activate();

    let (core, sketch) = compile_core_and_sketch(
        &compiler,
        &mut perf,
        &gate,
        CompileTarget {
            sources: &core_sources,
            build_dir: &root.join("core"),
            overlay: &overlay,
        },
        CompileTarget {
            sources: &sketch_sources,
            build_dir: &root.join("sketch"),
            overlay: &overlay,
        },
        &build_log,
    )
    .await
    .unwrap();

    let core_phase = perf
        .recorded("compile-core")
        .expect("compile-core recorded");
    let sketch_phase = perf
        .recorded("compile-sketch")
        .expect("compile-sketch recorded");
    let region = perf
        .recorded("compile-core-variant")
        .expect("region recorded");

    assert!(core.objects.is_empty(), "no core TU was compiled");
    assert_eq!(sketch.objects.len(), 1);
    assert!(
        core_phase < Duration::from_millis(30),
        "an empty core leg must not inherit the sketch's time; got {core_phase:?}"
    );
    assert!(
        sketch_phase >= Duration::from_millis(40),
        "the sketch TU compiles against the fake's 60 ms delay; got {sketch_phase:?}"
    );
    assert!(
        region >= sketch_phase.saturating_sub(Duration::from_millis(10)),
        "the region must span the sketch leg; region {region:?} sketch {sketch_phase:?}"
    );
}

/// With core TUs present, the core leg is its own measurable cost and both
/// legs are recorded independently.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn core_leg_is_recorded_independently_of_the_sketch() {
    let (dir, core_sources, sketch_sources) = fixture(2, 1);
    let root = dir.path();
    let compiler = SlowCompiler {
        gcc: NormalizedPath::new(root.join("gcc")),
        delay: Duration::from_millis(40),
    };
    let overlay = LanguageExtraFlags::default();
    let build_log = Mutex::new(BuildLog::new());
    let gate = Arc::new(Semaphore::new(4));
    let mut perf = PerfTimer::new("compile-phases-test");
    perf.activate();

    let (core, sketch) = compile_core_and_sketch(
        &compiler,
        &mut perf,
        &gate,
        CompileTarget {
            sources: &core_sources,
            build_dir: &root.join("core"),
            overlay: &overlay,
        },
        CompileTarget {
            sources: &sketch_sources,
            build_dir: &root.join("sketch"),
            overlay: &overlay,
        },
        &build_log,
    )
    .await
    .unwrap();

    let core_phase = perf
        .recorded("compile-core")
        .expect("compile-core recorded");
    let sketch_phase = perf
        .recorded("compile-sketch")
        .expect("compile-sketch recorded");

    assert_eq!(core.objects.len(), 2);
    assert_eq!(sketch.objects.len(), 1);
    assert!(
        core_phase >= Duration::from_millis(30),
        "two core TUs each take 40 ms; got {core_phase:?}"
    );
    assert!(
        sketch_phase >= Duration::from_millis(30),
        "the sketch TU takes 40 ms; got {sketch_phase:?}"
    );
}
