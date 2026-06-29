//! Fluent builder for fake Teensyduino / STM32duino / Arduino framework
//! trees backed by a `tempfile::TempDir`.
//!
//! Phases 1–3 of <https://github.com/FastLED/fbuild/issues/205> shipped a
//! header scanner ([`fbuild_header_scan::scan`]), include-graph walker
//! ([`fbuild_header_scan::walk`]) and a PlatformIO-LDF-style resolver
//! ([`fbuild_library_select::resolve`]). Their unit tests inline their own
//! tempdir scaffolding. Future phase tests (#205 §2/§3/§5) need a reusable
//! fixture so the per-orchestrator integration tests can stop reinventing the
//! same Arduino-library directory layout.
//!
//! [`MiniFramework`] writes its trees in the layout
//! [`fbuild_packages::library::framework_library::discover_framework_libraries`]
//! recognizes — that is the contract: anything you build with this fixture
//! must be discoverable by the production walk.
//!
//! # Layout
//!
//! Two roots live under a fresh `TempDir`:
//!
//! ```text
//! <tmp>/framework/
//!   libraries/
//!     SPI/
//!       src/
//!         SPI.h
//! <tmp>/project/
//!   src/
//!   include/   (created on demand by add_project_include / sketch)
//! ```
//!
//! # Example
//!
//! ```no_run
//! use fbuild_test_support::MiniFramework;
//!
//! let mut fx = MiniFramework::new();
//! fx.add_library("SPI")
//!     .header("// SPI header\n")
//!     .cpp("// SPI impl\n")
//!     .done();
//! fx.sketch("#include <SPI.h>\nvoid setup() {}\nvoid loop() {}\n");
//!
//! let libs = fbuild_packages::library::framework_library::discover_framework_libraries(
//!     &fx.libraries_dir(),
//! );
//! assert_eq!(libs[0].name, "SPI");
//! ```

use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// Fake Arduino-style framework + project tree backed by a `TempDir`.
///
/// See module docs for the on-disk layout. The `TempDir` is dropped (and
/// scrubbed) when this struct is dropped, so callers should keep the fixture
/// alive for the duration of the test.
pub struct MiniFramework {
    /// Owning handle for the temp tree. Held for its `Drop` side effect.
    _tmp: TempDir,
    framework_root: PathBuf,
    project_root: PathBuf,
}

impl MiniFramework {
    /// Create a new fake framework tree under a fresh `TempDir`.
    ///
    /// `framework_root/libraries/`, `project_root/src/` are created eagerly.
    /// `project_root/include/` is created lazily by [`add_project_include`]
    /// or [`project_search_paths`]'s callers.
    ///
    /// [`add_project_include`]: MiniFramework::add_project_include
    /// [`project_search_paths`]: MiniFramework::project_search_paths
    pub fn new() -> Self {
        // Root scratch dirs under `~/.fbuild/{dev|prod}/tmp/mini-framework/`
        // — FastLED/fbuild#844 bridge pair 10.
        let tmp = tempfile::tempdir_in(fbuild_paths::temp_subdir("mini-framework"))
            .expect("MiniFramework: failed to create temp dir");
        let framework_root = tmp.path().join("framework");
        let project_root = tmp.path().join("project");

        std::fs::create_dir_all(framework_root.join("libraries"))
            .expect("MiniFramework: failed to create framework/libraries");
        std::fs::create_dir_all(project_root.join("src"))
            .expect("MiniFramework: failed to create project/src");

        Self {
            _tmp: tmp,
            framework_root,
            project_root,
        }
    }

    /// Begin adding a framework library named `name`.
    ///
    /// `<framework_root>/libraries/<name>/src/` is created eagerly and a
    /// default empty `<name>.h` is written so trivial cases — "library `SPI`
    /// exists with header `SPI.h`" — don't need any builder methods.
    pub fn add_library(&mut self, name: &str) -> LibraryBuilder<'_> {
        let lib_dir = self.framework_root.join("libraries").join(name);
        let src_dir = lib_dir.join("src");
        std::fs::create_dir_all(&src_dir)
            .unwrap_or_else(|e| panic!("MiniFramework: failed to create {src_dir:?}: {e}"));

        // Default empty header so callers don't need .header("") for trivial
        // libs.
        let default_header = src_dir.join(format!("{name}.h"));
        std::fs::write(&default_header, "")
            .unwrap_or_else(|e| panic!("MiniFramework: failed to write {default_header:?}: {e}"));

        LibraryBuilder {
            name: name.to_string(),
            lib_dir,
            src_dir,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Write a project source file relative to `<project_root>/src/`.
    pub fn add_project_source(&mut self, rel_path: &str, contents: &str) -> &mut Self {
        let dst = self.project_root.join("src").join(rel_path);
        write_file(&dst, contents);
        self
    }

    /// Write a project include header relative to `<project_root>/include/`.
    pub fn add_project_include(&mut self, rel_path: &str, contents: &str) -> &mut Self {
        let dst = self.project_root.join("include").join(rel_path);
        write_file(&dst, contents);
        self
    }

    /// Convenience: write `<project_root>/src/main.cpp` with `contents`.
    pub fn sketch(&mut self, contents: &str) -> &mut Self {
        self.add_project_source("main.cpp", contents)
    }

    /// Absolute path to the project root (`<tmp>/project`).
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Absolute path to the framework root (`<tmp>/framework`).
    pub fn framework_root(&self) -> &Path {
        &self.framework_root
    }

    /// Absolute path to the framework's `libraries/` dir.
    pub fn libraries_dir(&self) -> PathBuf {
        self.framework_root.join("libraries")
    }

    /// Absolute path to the project's `src/` dir.
    pub fn project_src(&self) -> PathBuf {
        self.project_root.join("src")
    }

    /// Walk `<project_root>/src/**` recursively for compilable source files
    /// and return them as walker seeds.
    ///
    /// Extensions match the set used by
    /// [`fbuild_packages::library::framework_library::collect_library_sources`]
    /// (`.c`, `.cpp`, `.cc`, `.cxx`, `.s`). Headers under `src/` are not
    /// seeds — they are reached via `#include`. Returned paths are sorted for
    /// determinism.
    pub fn project_seeds(&self) -> Vec<PathBuf> {
        let mut seeds = Vec::new();
        collect_seeds(&self.project_root.join("src"), &mut seeds);
        seeds.sort();
        seeds
    }

    /// Project-level include search paths, in PlatformIO order.
    ///
    /// Returns `[<project>/include, <project>/src]` if `include/` exists on
    /// disk, otherwise just `[<project>/src]`. Both returned paths are
    /// guaranteed to exist.
    pub fn project_search_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let include = self.project_root.join("include");
        if include.is_dir() {
            paths.push(include);
        }
        paths.push(self.project_root.join("src"));
        paths
    }
}

impl Default for MiniFramework {
    fn default() -> Self {
        Self::new()
    }
}

/// Fluent builder for content of a single framework library.
///
/// Returned by [`MiniFramework::add_library`]; finish with [`done`] (or just
/// drop the builder).
///
/// [`done`]: LibraryBuilder::done
#[must_use = "LibraryBuilder is a fluent builder; call .done() or chain methods"]
pub struct LibraryBuilder<'a> {
    name: String,
    lib_dir: PathBuf,
    src_dir: PathBuf,
    /// Borrow-checker tether so each library builder is bounded to its parent
    /// `MiniFramework`'s lifetime.
    _phantom: std::marker::PhantomData<&'a mut MiniFramework>,
}

impl<'a> LibraryBuilder<'a> {
    /// Overwrite the default `<lib>/src/<name>.h` with `contents`.
    pub fn header(self, contents: &str) -> Self {
        let dst = self.src_dir.join(format!("{}.h", self.name));
        write_file(&dst, contents);
        self
    }

    /// Write `<lib>/src/<name>.cpp` with `contents`.
    pub fn cpp(self, contents: &str) -> Self {
        let dst = self.src_dir.join(format!("{}.cpp", self.name));
        write_file(&dst, contents);
        self
    }

    /// Write any additional file under `<lib>/src/<rel_path>`.
    ///
    /// `rel_path` may contain subdirectories — they are created as needed.
    pub fn extra(self, rel_path: &str, contents: &str) -> Self {
        let dst = self.src_dir.join(rel_path);
        write_file(&dst, contents);
        self
    }

    /// Write `<lib>/examples/<rel_path>` — exists so resolver tests can prove
    /// `examples/` content is excluded from the compile set.
    pub fn example(self, rel_path: &str, contents: &str) -> Self {
        let dst = self.lib_dir.join("examples").join(rel_path);
        write_file(&dst, contents);
        self
    }

    /// Write `<lib>/extras/<rel_path>` — exists so resolver tests can prove
    /// `extras/` content is excluded.
    pub fn extras(self, rel_path: &str, contents: &str) -> Self {
        let dst = self.lib_dir.join("extras").join(rel_path);
        write_file(&dst, contents);
        self
    }

    /// Write `<lib>/tests/<rel_path>` — exists so resolver tests can prove
    /// `tests/` content is excluded.
    pub fn tests(self, rel_path: &str, contents: &str) -> Self {
        let dst = self.lib_dir.join("tests").join(rel_path);
        write_file(&dst, contents);
        self
    }

    /// Finish the library (drop the builder).
    pub fn done(self) {}
}

fn write_file(dst: &Path, contents: &str) {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("MiniFramework: failed to create {parent:?}: {e}"));
    }
    std::fs::write(dst, contents)
        .unwrap_or_else(|e| panic!("MiniFramework: failed to write {dst:?}: {e}"));
}

fn collect_seeds(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_seeds(&path, out);
        } else {
            let ext = path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if matches!(ext.as_str(), "c" | "cpp" | "cc" | "cxx" | "s") {
                out.push(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fbuild_header_scan::walk;
    use fbuild_library_select::resolve;
    use fbuild_packages::library::framework_library::{
        collect_library_sources, discover_framework_libraries,
    };

    #[test]
    fn mini_framework_new_creates_expected_dir_layout() {
        let fx = MiniFramework::new();
        assert!(fx.libraries_dir().is_dir(), "framework/libraries/ missing");
        assert!(fx.project_src().is_dir(), "project/src/ missing");
        // include/ is lazy.
        assert!(!fx.project_root().join("include").exists());
    }

    #[test]
    fn add_library_creates_default_header() {
        let mut fx = MiniFramework::new();
        fx.add_library("SPI").done();
        let header = fx.libraries_dir().join("SPI").join("src").join("SPI.h");
        assert!(header.is_file(), "default SPI.h missing: {header:?}");
        let bytes = std::fs::read(&header).unwrap();
        assert!(bytes.is_empty(), "default header should be empty");
    }

    #[test]
    fn library_builder_chaining() {
        let mut fx = MiniFramework::new();
        fx.add_library("SPI")
            .header("// hi\n")
            .cpp("// impl\n")
            .done();
        let src = fx.libraries_dir().join("SPI").join("src");
        assert_eq!(
            std::fs::read_to_string(src.join("SPI.h")).unwrap(),
            "// hi\n"
        );
        assert_eq!(
            std::fs::read_to_string(src.join("SPI.cpp")).unwrap(),
            "// impl\n",
        );
    }

    #[test]
    fn extra_writes_nested_path() {
        let mut fx = MiniFramework::new();
        fx.add_library("SPI").extra("utility/foo.h", "x").done();
        let nested = fx
            .libraries_dir()
            .join("SPI")
            .join("src")
            .join("utility")
            .join("foo.h");
        assert_eq!(std::fs::read_to_string(&nested).unwrap(), "x");
    }

    #[test]
    fn examples_extras_tests_are_excluded_from_collect_library_sources() {
        let mut fx = MiniFramework::new();
        fx.add_library("SPI")
            .cpp("// real impl\n")
            .example("Demo.cpp", "// demo\n")
            .extras("tool.cpp", "// tool\n")
            .tests("test_spi.cpp", "// test\n")
            .done();
        let lib_dir = fx.libraries_dir().join("SPI");
        let sources = collect_library_sources(&lib_dir);
        // Only the real .cpp under src/ should appear.
        assert_eq!(
            sources,
            vec![lib_dir.join("src").join("SPI.cpp")],
            "examples/extras/tests must be excluded",
        );
    }

    #[test]
    fn discover_framework_libraries_finds_all_libs() {
        let mut fx = MiniFramework::new();
        fx.add_library("SPI").done();
        fx.add_library("Wire").done();
        fx.add_library("EEPROM").done();

        let libs = discover_framework_libraries(&fx.libraries_dir());
        let names: Vec<_> = libs.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["EEPROM", "SPI", "Wire"]);
    }

    #[test]
    fn project_seeds_returns_all_src_sources() {
        let mut fx = MiniFramework::new();
        fx.add_project_source("main.cpp", "// main\n");
        fx.add_project_source("helpers/util.cpp", "// util\n");

        let seeds = fx.project_seeds();
        let expected = {
            let mut v = vec![
                fx.project_src().join("helpers").join("util.cpp"),
                fx.project_src().join("main.cpp"),
            ];
            v.sort();
            v
        };
        assert_eq!(seeds, expected);
    }

    #[test]
    fn project_seeds_skips_headers() {
        let mut fx = MiniFramework::new();
        fx.add_project_source("main.cpp", "");
        fx.add_project_source("local.h", "");
        let seeds = fx.project_seeds();
        assert_eq!(seeds, vec![fx.project_src().join("main.cpp")]);
    }

    #[test]
    fn project_search_paths_includes_dir_only_when_present() {
        let mut fx = MiniFramework::new();
        // No include/ yet.
        let paths = fx.project_search_paths();
        assert_eq!(paths, vec![fx.project_src()]);

        // Populate include/.
        fx.add_project_include("Project.h", "// project header\n");
        let paths = fx.project_search_paths();
        assert_eq!(
            paths,
            vec![fx.project_root().join("include"), fx.project_src()],
        );
    }

    #[test]
    fn sketch_helper_writes_main_cpp() {
        let mut fx = MiniFramework::new();
        fx.sketch("// sketch\n");
        let main = fx.project_src().join("main.cpp");
        assert_eq!(std::fs::read_to_string(&main).unwrap(), "// sketch\n");
    }

    #[test]
    fn walker_round_trip() {
        let mut fx = MiniFramework::new();
        fx.add_library("SPI").done();
        fx.sketch("#include <SPI.h>\n");

        let libs = discover_framework_libraries(&fx.libraries_dir());
        // Search paths: project search paths + every lib's include dirs.
        let mut search_paths = fx.project_search_paths();
        for lib in &libs {
            for d in &lib.include_dirs {
                search_paths.push(d.clone());
            }
        }

        let res = walk(&fx.project_seeds(), &search_paths);
        let spi_h = std::fs::canonicalize(fx.libraries_dir().join("SPI").join("src").join("SPI.h"))
            .unwrap();
        assert!(
            res.reached.contains(&spi_h),
            "walker did not reach SPI.h via fixture; reached={:?}",
            res.reached,
        );
    }

    #[test]
    fn resolver_round_trip() {
        let mut fx = MiniFramework::new();
        fx.add_library("SPI").cpp("// impl\n").done();
        fx.add_library("Wire").cpp("// wire impl\n").done();
        fx.sketch("#include <SPI.h>\nvoid setup() {}\nvoid loop() {}\n");

        let libs = discover_framework_libraries(&fx.libraries_dir());
        let sel = resolve(&fx.project_seeds(), &fx.project_search_paths(), &libs);

        assert_eq!(
            sel.required_libraries,
            vec!["SPI".to_string()],
            "only SPI should be selected; got {:?}",
            sel.required_libraries,
        );
        // Wire must NOT bleed in via mere existence under libraries/.
        assert!(!sel.required_libraries.iter().any(|n| n == "Wire"));
    }
}
