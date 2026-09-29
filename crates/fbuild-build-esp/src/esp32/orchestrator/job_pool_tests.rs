//! Tests for the ESP32 build's single job pool (FastLED/fbuild#1559).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fbuild_packages::library::library_compiler::{
    LibCompileBackend, compile_library_gated, job_gate,
};

use super::super::framework_libs::tests::{FakeBackend, Fixture};
use super::*;
use crate::flag_overlay::LanguageExtraFlags;

#[test]
fn link_order_is_lib_deps_project_framework_then_local() {
    let order = link_order(
        vec!["libfastled.a", "libzlib.a"],
        Some("libproject.a"),
        vec!["libfs.a", "libwifi.a"],
        vec!["libmine.a"],
    );
    assert_eq!(
        order,
        [
            "libfastled.a",
            "libzlib.a",
            "libproject.a",
            "libfs.a",
            "libwifi.a",
            "libmine.a"
        ]
    );
    assert_eq!(
        link_order(Vec::new(), None, vec!["libfs.a"], Vec::new()),
        ["libfs.a"]
    );
}

#[test]
fn already_compiled_holds_lib_deps_and_project_library_names() {
    let names = already_compiled(
        vec!["FastLED".to_string(), "zlib".to_string()],
        Some("myproject".to_string()),
    );
    let expected: HashSet<String> = ["FastLED", "zlib", "myproject"].map(String::from).into();
    assert_eq!(names, expected);
    assert!(already_compiled(Vec::new(), None).is_empty());
}

/// A [`Compiler`] for core/sketch TUs counting in-flight compiles on the same
/// counters as the library [`FakeBackend`], so one maximum covers the pool.
struct FakeCompiler {
    gcc: fbuild_core::path::NormalizedPath,
    backend: Arc<FakeBackend>,
}

#[async_trait::async_trait]
impl Compiler for FakeCompiler {
    async fn compile_one(
        &self,
        _compiler_path: &Path,
        _source: &Path,
        output: &Path,
        _flags: &[String],
        _extra_flags: &[String],
    ) -> Result<crate::compiler::CompileResult> {
        let now = self.backend.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.backend.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.backend.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(60)).await;
        self.backend.in_flight.fetch_sub(1, Ordering::SeqCst);
        std::fs::write(output, "obj").unwrap();
        Ok(crate::compiler::CompileResult {
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

/// Runs a pool of core (2 TUs), sketch (1), one framework library (1), one
/// `lib_deps` library (1) and a project library (none) on `jobs` permits.
/// Returns the pool's max in-flight compiles and the archives in link order.
async fn run_pool(jobs: usize) -> (usize, Vec<String>) {
    let backend = Arc::new(FakeBackend::new(
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    ));
    let fx = Fixture::with_backend(backend.clone());
    let root = fx.tmp.path();
    let compiler = FakeCompiler {
        gcc: fbuild_core::path::NormalizedPath::new(root.join("gcc")),
        backend: backend.clone(),
    };
    let write = |name: &str| {
        let path = root.join("src").join(name);
        std::fs::write(&path, "int x;").unwrap();
        path
    };
    let core_sources = vec![write("core1.cpp"), write("core2.cpp")];
    let sketch_sources = vec![write("main.cpp")];
    let dep_sources = vec![write("dep.cpp")];
    let (core_dir, sketch_dir) = (root.join("core"), root.join("sketch"));
    std::fs::create_dir_all(&core_dir).unwrap();
    std::fs::create_dir_all(&sketch_dir).unwrap();
    let overlay = LanguageExtraFlags::default();

    let mut log = BuildLog::new();
    let libraries = vec![fx.library("WiFi", &["wifi.cpp"])];
    let fw_libs = fx.plan(&libraries, &HashSet::new(), &mut log);
    let build_log = Mutex::new(log);
    let gate = job_gate(jobs);
    let lib_backend: Arc<dyn LibCompileBackend> = backend.clone();
    let (ar, deps_dir) = (root.join("ar"), root.join("libs").join("dep"));
    let jobs = LibraryJobs {
        lib_deps: async {
            let archive = compile_library_gated(
                "dep",
                &dep_sources,
                &[],
                &root.join("gcc"),
                &root.join("g++"),
                &ar,
                &[],
                &[],
                &deps_dir,
                false,
                &gate,
                None,
                None,
                Some(lib_backend),
                None,
            )
            .await?;
            Ok(archive.into_iter().collect())
        },
        project_library: async { Ok(None) },
        local_libs: async { Ok(Vec::new()) },
        boot_artifacts: async { Ok(Duration::ZERO) },
    };
    let mut perf = PerfTimer::new("job-pool-test");
    let pool = run(
        &mut perf,
        &gate,
        &compiler,
        CompileTarget {
            sources: &core_sources,
            build_dir: &core_dir,
            overlay: &overlay,
        },
        CompileTarget {
            sources: &sketch_sources,
            build_dir: &sketch_dir,
            overlay: &overlay,
        },
        &build_log,
        &fw_libs,
        jobs,
    )
    .await
    .unwrap();
    assert_eq!(pool.core.objects.len(), 2);
    assert_eq!(pool.sketch.objects.len(), 1);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 5);

    let mut log = build_log.into_inner().unwrap_or_else(|e| e.into_inner());
    let framework = fw_libs.finish(pool.framework_libs, &mut perf, &mut log);
    let archives = link_order(
        pool.lib_deps,
        pool.project_library,
        framework,
        pool.local_libs,
    )
    .iter()
    .map(|archive| archive.file_name().unwrap().to_string_lossy().into_owned())
    .collect();
    (backend.max_in_flight.load(Ordering::SeqCst), archives)
}

/// Every compile of the build draws from the caller's one gate: core +
/// sketch, framework libraries and `lib_deps` together never exceed it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pool_never_exceeds_the_shared_gate() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let (max, archives) = run_pool(2).await;
    assert!(
        max <= 2,
        "max in-flight {max} exceeded the gate's 2 permits"
    );
    assert_eq!(archives, ["libdep.a", "libwifi.a"]);
}

/// With enough permits, TUs from different phases are in flight at once: the
/// largest phase (core) has two TUs, so three or more means phases overlap
/// instead of running one after another.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pool_overlaps_every_phase() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let (max, archives) = run_pool(8).await;
    assert!(
        (3..=5).contains(&max),
        "max in-flight {max}: core, sketch, framework and lib_deps TUs overlap"
    );
    assert_eq!(archives, ["libdep.a", "libwifi.a"]);
}
