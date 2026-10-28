//! Pre-flight disk-space checks (FastLED/fbuild#1750).

use std::collections::HashMap;
use std::io;
use std::path::Path;

use fbuild_core::path::NormalizedPath;

use crate::disk_preflight::*;

/// A volume whose free space the test states outright.
struct FakeFreeSpace {
    free: HashMap<NormalizedPath, u64>,
    unmeasurable: Vec<NormalizedPath>,
    measured: std::cell::RefCell<Vec<NormalizedPath>>,
}

impl FakeFreeSpace {
    fn new(free: &[(&str, u64)]) -> Self {
        Self {
            free: free
                .iter()
                .map(|(path, bytes)| (NormalizedPath::new(path), *bytes))
                .collect(),
            unmeasurable: Vec::new(),
            measured: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn unmeasurable(mut self, path: &str) -> Self {
        self.unmeasurable.push(NormalizedPath::new(path));
        self
    }
}

impl FreeSpace for FakeFreeSpace {
    fn available(&self, path: &Path) -> io::Result<u64> {
        let key = NormalizedPath::from(path);
        self.measured.borrow_mut().push(key.clone());
        if self.unmeasurable.contains(&key) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        }
        Ok(*self.free.get(&key).unwrap_or(&0))
    }
}

const GIB: u64 = 1024 * 1024 * 1024;

fn cache_root() -> &'static Path {
    Path::new("/cache-root")
}

#[test]
fn ample_space_is_sufficient_and_prints_one_line() {
    let probe = FakeFreeSpace::new(&[("/cache", 40 * GIB), ("/project", 40 * GIB)]);
    let roots = vec![
        DiskRoot::new("cache", "/cache", 2 * GIB),
        DiskRoot::new("project", "/project", 256 * 1024 * 1024),
    ];
    let report = preflight(&probe, &roots, cache_root()).unwrap();
    assert!(report.is_sufficient());
    let line = report.info_line();
    assert!(
        !line.contains('\n'),
        "healthy build must log one line: {line}"
    );
    assert!(line.contains("cache"), "{line}");
    assert!(line.contains("40.00GB free"), "{line}");
}

#[test]
fn short_root_fails_with_path_required_and_shortfall() {
    let probe = FakeFreeSpace::new(&[("/cache", 512 * 1024 * 1024)]);
    let roots = vec![DiskRoot::new("cache", "/cache", 2 * GIB)];
    let error = preflight(&probe, &roots, cache_root())
        .expect_err("a root below its requirement must fail the build")
        .to_string();
    assert!(error.contains("/cache"), "{error}");
    assert!(error.contains("512.00MB free"), "{error}");
    assert!(error.contains("2.00GB"), "{error}");
    assert!(error.contains("short by 1.50GB"), "{error}");
    assert!(error.contains("FBUILD_MIN_DISK_BYTES=0"), "{error}");
}

#[test]
fn failure_names_reclaimable_caches_largest_first() {
    let dir = tempfile::tempdir().unwrap();
    let small = dir.path().join("core");
    let large = dir.path().join("platforms");
    std::fs::create_dir_all(&small).unwrap();
    std::fs::create_dir_all(&large).unwrap();
    std::fs::write(small.join("one.o"), vec![0u8; 1024]).unwrap();
    std::fs::write(large.join("two.o"), vec![0u8; 4096]).unwrap();
    std::fs::write(dir.path().join("index.sqlite"), vec![0u8; 8192]).unwrap();

    let probe = FakeFreeSpace::new(&[("/cache", 0)]);
    let roots = vec![DiskRoot::new("cache", "/cache", GIB)];
    let error = preflight(&probe, &roots, dir.path())
        .expect_err("a full volume must fail")
        .to_string();

    assert!(error.contains("platforms 4.00KB"), "{error}");
    assert!(error.contains("core 1.00KB"), "{error}");
    let platforms = error.find("platforms").unwrap();
    let core = error.find("core ").unwrap();
    assert!(
        platforms < core,
        "largest cache must be listed first: {error}"
    );
}

#[test]
fn unmeasurable_root_is_reported_but_not_fatal() {
    let probe = FakeFreeSpace::new(&[("/project", 40 * GIB)]).unmeasurable("/cache");
    let roots = vec![
        DiskRoot::new("cache", "/cache", 2 * GIB),
        DiskRoot::new("project", "/project", 256 * 1024 * 1024),
    ];
    let report = preflight(&probe, &roots, cache_root())
        .expect("an unreadable volume must not block the build");
    assert!(report.is_sufficient());
    assert_eq!(report.unchecked().len(), 1);
    assert_eq!(report.unchecked()[0].0, "cache");
}

#[test]
fn zero_requirement_skips_measurement_entirely() {
    let probe = FakeFreeSpace::new(&[]);
    let roots = vec![
        DiskRoot::new("cache", "/cache", 0),
        DiskRoot::new("temp", "/temp", 0),
    ];
    let report = preflight(&probe, &roots, cache_root()).unwrap();
    assert!(report.roots().is_empty());
    assert!(probe.measured.borrow().is_empty());
    assert!(report.info_line().contains("skipped"));
}

#[test]
fn only_the_short_root_is_named_on_failure() {
    let probe = FakeFreeSpace::new(&[("/cache", 40 * GIB), ("/project", 1024)]);
    let roots = vec![
        DiskRoot::new("cache", "/cache", 2 * GIB),
        DiskRoot::new("project", "/project", 256 * 1024 * 1024),
    ];
    let error = preflight(&probe, &roots, cache_root())
        .expect_err("one short root fails the whole check")
        .to_string();
    let project_path = NormalizedPath::new("/project");
    let cache_path = NormalizedPath::new("/cache");
    assert!(
        error.contains(&project_path.display().to_string()),
        "{error}"
    );
    assert!(
        !error.contains(&format!("{} has", cache_path.display())),
        "roomy roots stay out: {error}"
    );
    assert_eq!(error.matches("short by").count(), 1, "{error}");
}

#[test]
fn reclaimable_caches_sizes_depth_one_directories_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("nested/deeper")).unwrap();
    std::fs::write(dir.path().join("nested/deeper/big.o"), vec![0u8; 3000]).unwrap();
    std::fs::write(dir.path().join("stray.file"), vec![0u8; 9000]).unwrap();

    let caches = reclaimable_caches(dir.path());
    assert_eq!(caches.len(), 1, "{caches:?}");
    assert_eq!(caches[0].name, "nested");
    assert_eq!(caches[0].bytes, 3000);
}

#[test]
fn missing_cache_root_yields_no_reclaimable_caches() {
    assert!(reclaimable_caches(Path::new("/definitely/not/a/real/cache")).is_empty());
}

#[test]
fn a_tool_enospc_is_translated_and_lists_reclaimable_caches() {
    let dir = tempfile::tempdir().unwrap();
    let platforms = dir.path().join("platforms");
    std::fs::create_dir_all(&platforms).unwrap();
    std::fs::write(platforms.join("espressif32.tar.gz"), vec![0u8; 2048]).unwrap();

    let message = enospc_failure(
        "esptool elf2image",
        "A fatal error occurred: Could not write firmware.bin: No space left on device",
        dir.path(),
    )
    .expect("a full volume must translate into the disk message");

    assert!(message.contains("insufficient disk space"), "{message}");
    assert!(message.contains("esptool elf2image failed"), "{message}");
    assert!(message.contains("platforms 2.00KB"), "{message}");
    assert_eq!(
        message.split('\n').next_back(),
        Some(fbuild_core::disk_full::DISK_FULL_HINT),
        "a translated ENOSPC must end with the same hint as the pre-flight: {message}"
    );
}

#[test]
fn an_ordinary_tool_error_is_never_relabelled_a_disk_failure() {
    assert!(
        enospc_failure(
            "compiling a.cpp",
            "a.cpp:3:1: error: 'x' was not declared in this scope",
            cache_root()
        )
        .is_none()
    );
    assert!(enospc_error("compiling a.cpp", "", cache_root()).is_none());
    let translated = enospc_error(
        "ESP32 link",
        "ld: final link failed: No space left on device",
        cache_root(),
    )
    .expect("ENOSPC must produce an error");
    assert!(translated.to_string().contains("insufficient disk space"));
}

/// A volume where some paths do not exist yet (`NotFound`) and everything else
/// resolves the way a real `statvfs` would.
struct MissingDirsFreeSpace {
    missing: Vec<NormalizedPath>,
    free: HashMap<NormalizedPath, u64>,
    measured: std::cell::RefCell<Vec<NormalizedPath>>,
}

impl MissingDirsFreeSpace {
    fn new(missing: &[&str], free: &[(&str, u64)]) -> Self {
        Self {
            missing: missing.iter().map(NormalizedPath::new).collect(),
            free: free
                .iter()
                .map(|(path, bytes)| (NormalizedPath::new(path), *bytes))
                .collect(),
            measured: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.measured
            .borrow()
            .iter()
            .map(NormalizedPath::display_slash)
            .collect()
    }
}

impl FreeSpace for MissingDirsFreeSpace {
    fn available(&self, path: &Path) -> io::Result<u64> {
        let key = NormalizedPath::from(path);
        self.measured.borrow_mut().push(key.clone());
        if self.missing.contains(&key) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such directory"));
        }
        Ok(*self.free.get(&key).unwrap_or(&0))
    }
}

#[test]
fn a_root_that_does_not_exist_yet_is_measured_on_its_volume() {
    // A fresh host has no `<cache>/tmp`; the scratch root must still report the
    // cache volume's free space instead of "could not be measured".
    let probe = MissingDirsFreeSpace::new(&["/cache/tmp"], &[("/cache", 40 * GIB)]);
    let roots = vec![DiskRoot::new("temp", "/cache/tmp", 64 * 1024 * 1024)];
    let report = measure(&probe, &roots);
    assert_eq!(
        report.unchecked().len(),
        0,
        "a create-me-later root is measurable"
    );
    assert_eq!(report.roots().len(), 1);
    assert_eq!(report.roots()[0].free_bytes(), 40 * GIB);
    assert_eq!(report.roots()[0].label(), "temp");
    assert_eq!(
        probe.asked(),
        vec!["/cache/tmp".to_string(), "/cache".to_string()],
        "the missing root is asked first, then its ancestor"
    );
}

#[test]
fn a_root_nested_under_missing_dirs_walks_to_the_first_existing_ancestor() {
    let probe = MissingDirsFreeSpace::new(
        &["/cache/a/b/c", "/cache/a/b", "/cache/a"],
        &[("/cache", 7 * GIB)],
    );
    let roots = vec![DiskRoot::new("deep", "/cache/a/b/c", 1024)];
    let report = measure(&probe, &roots);
    assert_eq!(report.roots()[0].free_bytes(), 7 * GIB);
}

#[test]
fn a_permission_error_is_still_unmeasurable_and_never_walks_up() {
    let probe = FakeFreeSpace::new(&[("/", 40 * GIB)]).unmeasurable("/cache");
    let roots = vec![DiskRoot::new("cache", "/cache", 2 * GIB)];
    let report = measure(&probe, &roots);
    assert_eq!(report.unchecked().len(), 1, "denied is not a missing dir");
    assert!(report.roots().is_empty());
}
