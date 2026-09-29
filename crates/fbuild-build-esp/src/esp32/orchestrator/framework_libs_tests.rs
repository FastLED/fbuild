//! Tests for the framework-library prepare / compile / finish split
//! (FastLED/fbuild#1559).

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use fbuild_core::path::NormalizedPath;
use fbuild_packages::library::FrameworkLibrary;
use fbuild_packages::library::library_compiler::{LibCompileOutcome, job_gate};

use super::*;

/// Install an executable script via staging + rename (avoids ETXTBSY when
/// tests run in parallel).
pub(crate) fn install_script(path: &Path, script: &str) {
    let staging = path.with_extension("staging");
    std::fs::write(&staging, script).unwrap();
    fbuild_core::platform::fs::set_executable(&staging).unwrap();
    std::fs::rename(&staging, path).unwrap();
}

/// A fake `ar` that creates its 2nd argument (the archive).
pub(crate) fn install_fake_ar(path: &Path) {
    install_script(
        path,
        "#!/bin/sh\n# argv: rcs <archive> <objs...>\ntouch \"$2\"\n",
    );
}

/// A [`LibCompileBackend`] that tracks in-flight compiles on a counter it may
/// share with other fakes, sleeps, writes the `-o` object, and fails sources
/// whose file name starts with `bad`.
pub(crate) struct FakeBackend {
    pub in_flight: Arc<AtomicUsize>,
    pub max_in_flight: Arc<AtomicUsize>,
    pub calls: AtomicUsize,
}

impl FakeBackend {
    pub(crate) fn new(in_flight: Arc<AtomicUsize>, max_in_flight: Arc<AtomicUsize>) -> Self {
        Self {
            in_flight,
            max_in_flight,
            calls: AtomicUsize::new(0),
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        let after = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_default()
        };
        let (source, object) = (after("-c"), after("-o"));
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(40)).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);

        let name = Path::new(&source)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if name.starts_with("bad") {
            return Ok(LibCompileOutcome {
                exit_code: 1,
                stdout: Vec::new(),
                stderr: b"boom".to_vec(),
            });
        }
        let object = cwd.as_path().join(object);
        std::fs::create_dir_all(object.parent().unwrap()).unwrap();
        std::fs::write(&object, "obj").unwrap();
        Ok(LibCompileOutcome {
            exit_code: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
}

pub(crate) struct Fixture {
    pub tmp: tempfile::TempDir,
    pub backend: Arc<FakeBackend>,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        Self::with_backend(Arc::new(FakeBackend::new(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        )))
    }

    pub(crate) fn with_backend(backend: Arc<FakeBackend>) -> Self {
        let tmp = tempfile::TempDir::new().unwrap();
        install_fake_ar(&tmp.path().join("ar"));
        std::fs::create_dir_all(tmp.path().join("framework")).unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        Self { tmp, backend }
    }

    pub(crate) fn build_dir(&self) -> NormalizedPath {
        NormalizedPath::new(self.tmp.path().join("build").join("fw_libs"))
    }

    fn ctx(&self) -> FwLibsContext {
        let root = self.tmp.path();
        FwLibsContext {
            build_dir: self.build_dir().into_path_buf(),
            include_dirs: Vec::new(),
            gcc_path: root.join("gcc"),
            gxx_path: root.join("g++"),
            ar_path: root.join("ar"),
            c_flags: Vec::new(),
            cpp_flags: Vec::new(),
            signature: "sig".to_string(),
            verbose: false,
            compiler_cache: None,
            compile_cwd: None,
            backend: self.backend.clone(),
            cache: FrameworkLibraryCache::with_cache_root(
                &root.join("project"),
                &root.join("cache"),
                fbuild_core::BuildProfile::Release,
                "sig",
                &root.join("framework"),
            ),
        }
    }

    /// A framework library `name` with one source per entry of `sources`.
    pub(crate) fn library(&self, name: &str, sources: &[&str]) -> FrameworkLibrary {
        let dir = self.tmp.path().join("src").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let source_files = sources
            .iter()
            .map(|source| {
                let path = dir.join(source);
                std::fs::write(&path, "int x;").unwrap();
                path
            })
            .collect();
        FrameworkLibrary {
            name: name.to_string(),
            dir: dir.clone(),
            include_dirs: vec![dir],
            source_files,
        }
    }

    pub(crate) fn plan(
        &self,
        libraries: &[FrameworkLibrary],
        already_compiled: &HashSet<String>,
        log: &mut fbuild_core::BuildLog,
    ) -> FwLibsPlan {
        let mut perf = PerfTimer::new("fw-libs-test");
        FwLibsPlan::new(
            self.ctx(),
            libraries,
            already_compiled,
            false,
            false,
            &mut perf,
            log,
        )
        .unwrap()
    }

    async fn build(
        &self,
        libraries: &[FrameworkLibrary],
        already_compiled: &HashSet<String>,
        jobs: usize,
    ) -> (Vec<String>, Vec<String>) {
        let mut log = fbuild_core::BuildLog::new();
        let plan = self.plan(libraries, already_compiled, &mut log);
        let compiled = plan.compile(&job_gate(jobs)).await;
        let mut perf = PerfTimer::new("fw-libs-test");
        let archives = plan.finish(compiled, &mut perf, &mut log);
        let names = archives
            .iter()
            .map(|archive| archive.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        (names, log.into_lines())
    }
}

/// Cache hits and fresh compiles come back interleaved in selection order,
/// exactly as the serial loop pushed them; a failing library is recorded and
/// left out without failing the build; a header-only library and one a
/// `lib_deps` archive already provides are skipped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finish_keeps_selection_order_and_tolerates_failures() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let fx = Fixture::new();
    let libraries = vec![
        fx.library("Alpha", &["a1.cpp", "a2.cpp"]),
        fx.library("Beta", &["b.cpp"]),
        fx.library("Delta", &["d.cpp", "bad.cpp"]),
        fx.library("Empty", &[]),
        fx.library("FastLED", &["f.cpp"]),
        fx.library("Gamma", &["g.c"]),
    ];
    // Beta's archive is already in the build dir (a hydrated cache hit).
    std::fs::create_dir_all(fx.build_dir()).unwrap();
    std::fs::write(fx.build_dir().join("libbeta.a"), "cached").unwrap();
    let already: HashSet<String> = ["fastled".to_string()].into();

    let (archives, log) = fx.build(&libraries, &already, 2).await;
    assert_eq!(archives, ["libalpha.a", "libbeta.a", "libgamma.a"]);
    assert!(
        log.iter()
            .any(|l| l.ends_with("framework-libs cache: stored 2")),
        "{log:?}"
    );
    let marker = framework_failure_marker(fx.build_dir().as_path(), "delta");
    assert!(marker.is_file(), "failure marker recorded");
    assert!(fx.ctx().cache.has_failed("delta"));
    let calls = fx.backend.calls.load(Ordering::SeqCst);
    assert_eq!(calls, 2 + 2 + 1, "Alpha, Delta and Gamma compiled");

    // Next build: everything is a cache hit or a remembered failure, so
    // nothing compiles and the order is unchanged.
    let (again, _) = fx.build(&libraries, &already, 2).await;
    assert_eq!(again, archives);
    assert_eq!(fx.backend.calls.load(Ordering::SeqCst), calls);
}

/// Pending libraries compile concurrently: with one TU each, two in flight is
/// only possible across libraries, and the gate is never exceeded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_libraries_share_one_gate() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let fx = Fixture::new();
    let libraries: Vec<_> = ["A", "B", "C", "D", "E"]
        .iter()
        .map(|name| fx.library(name, &["one.cpp"]))
        .collect();

    let (archives, _) = fx.build(&libraries, &HashSet::new(), 3).await;
    assert_eq!(archives, ["liba.a", "libb.a", "libc.a", "libd.a", "libe.a"]);
    let max = fx.backend.max_in_flight.load(Ordering::SeqCst);
    assert!((2..=3).contains(&max), "max in-flight {max} with 3 permits");
}

/// `clean_only` plans nothing (no hydrate); `clean_all` evicts the cache.
#[tokio::test]
async fn clean_only_plans_nothing_and_clean_all_evicts() {
    let fx = Fixture::new();
    let archive = fx.tmp.path().join("libalpha.a");
    std::fs::write(&archive, "a").unwrap();
    fx.ctx().cache.store_archive(&archive).unwrap();
    let libraries = vec![fx.library("Alpha", &["a.cpp"])];

    let mut log = fbuild_core::BuildLog::new();
    let mut perf = PerfTimer::new("fw-libs-test");
    let plan = FwLibsPlan::new(
        fx.ctx(),
        &libraries,
        &HashSet::new(),
        true,
        true,
        &mut perf,
        &mut log,
    )
    .unwrap();
    assert!(plan.ctx.is_none() && plan.entries.is_empty());
    assert!(log.is_empty(), "clean_only must not hydrate");
    assert_eq!(fx.ctx().cache.hydrate(&fx.build_dir()).unwrap(), 0);
    let compiled = plan.compile(&job_gate(1)).await;
    assert!(plan.finish(compiled, &mut perf, &mut log).is_empty());
    assert_eq!(fx.backend.calls.load(Ordering::SeqCst), 0);
}
