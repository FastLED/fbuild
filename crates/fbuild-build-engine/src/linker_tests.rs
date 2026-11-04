//! Sub-phase attribution for `Linker::link_all` (FastLED/fbuild#1894).
//!
//! A fresh-project ESP32 build spends ~80% of its wall time in one opaque
//! `link-convert-size` phase. These tests pin the split the engine records
//! inside it — `link-elf`, `link-convert`, `link-size`, `link-symbols` — so a
//! later refactor cannot silently collapse the attribution back to one number.
//!
//! The fixture implements `Linker`, whose methods are typed `&[PathBuf]`, so
//! this file needs `std::path::PathBuf` (see the `ban_std_pathbuf` allowlist
//! entry that names it).

use std::path::{Path, PathBuf};

use fbuild_core::{Result, SizeInfo};

use crate::linker::{LinkExtraArgs, Linker};
use crate::perf_log::PerfTimer;

/// A linker that fabricates artifacts instead of running a toolchain, so the
/// tests measure the accounting and nothing else.
struct FakeLinker {
    size_tool: PathBuf,
}

#[async_trait::async_trait]
impl Linker for FakeLinker {
    async fn archive(&self, _objects: &[PathBuf], _output: &Path) -> Result<()> {
        Ok(())
    }

    async fn link(
        &self,
        _objects: &[PathBuf],
        _archives: &[PathBuf],
        output: &Path,
        _extra: &LinkExtraArgs,
    ) -> Result<PathBuf> {
        let elf = output.join("firmware.elf");
        std::fs::write(&elf, b"\x7fELF fake")?;
        Ok(elf)
    }

    async fn convert_firmware(&self, _elf: &Path, output_dir: &Path) -> Result<PathBuf> {
        let bin = output_dir.join("firmware.bin");
        std::fs::write(&bin, b"fake image")?;
        Ok(bin)
    }

    async fn report_size(&self, _elf: &Path) -> Result<SizeInfo> {
        Ok(SizeInfo {
            text: 1024,
            data: 0,
            bss: 0,
            total_flash: 1024,
            total_ram: 0,
            max_flash: None,
            max_ram: None,
        })
    }

    fn size_tool_path(&self) -> &Path {
        self.size_tool.as_path()
    }
}

/// A directory holding one sketch object older than the link that consumes it.
fn project() -> (tempfile::TempDir, FakeLinker, Vec<PathBuf>) {
    let tmp = tempfile::TempDir::new().unwrap();
    let object = tmp.path().join("sketch.o");
    std::fs::write(&object, b"obj").unwrap();
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&object)
        .unwrap()
        .set_modified(past)
        .unwrap();
    let linker = FakeLinker {
        size_tool: tmp.path().join("size"),
    };
    (tmp, linker, vec![object])
}

#[tokio::test]
async fn a_relink_records_the_link_convert_and_size_split() {
    let (tmp, linker, objects) = project();
    let mut perf = PerfTimer::new("link-subphases");
    perf.activate();

    linker
        .link_all(
            &objects,
            &[],
            tmp.path(),
            &LinkExtraArgs::default(),
            false,
            &mut perf,
        )
        .await
        .unwrap();

    for phase in ["link-elf", "link-convert", "link-size"] {
        assert!(
            perf.recorded(phase).is_some(),
            "`{phase}` must be attributed inside link_all, got nothing"
        );
    }
    assert!(
        perf.recorded("link-symbols").is_none(),
        "symbol analysis was not requested, so it must not be reported"
    );
    assert!(
        tmp.path().join("firmware.bin").exists(),
        "the conversion still has to run"
    );
}

#[tokio::test]
async fn a_reused_elf_records_the_conversion_but_not_a_link() {
    let (tmp, linker, objects) = project();
    let mut first = PerfTimer::new("link-first");
    first.activate();
    linker
        .link_all(
            &objects,
            &[],
            tmp.path(),
            &LinkExtraArgs::default(),
            false,
            &mut first,
        )
        .await
        .unwrap();

    // The ELF and its digest sidecar are current, so the second call is the
    // no-op path: it must not claim a link it never ran.
    let mut second = PerfTimer::new("link-second");
    second.activate();
    linker
        .link_all(
            &objects,
            &[],
            tmp.path(),
            &LinkExtraArgs::default(),
            false,
            &mut second,
        )
        .await
        .unwrap();

    assert!(
        second.recorded("link-elf").is_none(),
        "the ELF was reused; reporting `link-elf` would invent a relink"
    );
    assert!(second.recorded("link-convert").is_some());
    assert!(second.recorded("link-size").is_some());
}

#[tokio::test]
async fn requested_symbol_analysis_is_attributed() {
    let (tmp, linker, objects) = project();
    let mut perf = PerfTimer::new("link-symbols");
    perf.activate();

    linker
        .link_all(
            &objects,
            &[],
            tmp.path(),
            &LinkExtraArgs::default(),
            true,
            &mut perf,
        )
        .await
        .unwrap();

    assert!(
        perf.recorded("link-symbols").is_some(),
        "a requested symbol pass must be timed even when the tool is missing"
    );
}

/// A linker with a cross-project cache whose link calls are counted, so a
/// cache hit is proven by the absence of a second link rather than by timing.
struct CachedLinker {
    size_tool: PathBuf,
    cache: PathBuf,
    links: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl Linker for CachedLinker {
    async fn archive(&self, _objects: &[PathBuf], _output: &Path) -> Result<()> {
        Ok(())
    }

    async fn link(
        &self,
        _objects: &[PathBuf],
        _archives: &[PathBuf],
        output: &Path,
        _extra: &LinkExtraArgs,
    ) -> Result<PathBuf> {
        self.links.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let elf = output.join("firmware.elf");
        std::fs::write(&elf, b"\x7fELF cached")?;
        Ok(elf)
    }

    async fn convert_firmware(&self, _elf: &Path, output_dir: &Path) -> Result<PathBuf> {
        let bin = output_dir.join("firmware.bin");
        std::fs::write(&bin, b"cached image")?;
        Ok(bin)
    }

    async fn report_size(&self, _elf: &Path) -> Result<SizeInfo> {
        Ok(SizeInfo {
            text: 1024,
            data: 0,
            bss: 0,
            total_flash: 1024,
            total_ram: 0,
            max_flash: None,
            max_ram: None,
        })
    }

    fn size_tool_path(&self) -> &Path {
        self.size_tool.as_path()
    }

    fn link_output_cache_dir(&self) -> Option<&Path> {
        Some(self.cache.as_path())
    }
}

fn object_in(dir: &Path, bytes: &[u8]) -> PathBuf {
    let object = dir.join("sketch.o");
    std::fs::write(&object, bytes).unwrap();
    object
}

#[tokio::test]
async fn an_identical_link_in_a_fresh_directory_reuses_the_cached_elf() {
    let cache = tempfile::TempDir::new().unwrap();
    let first = tempfile::TempDir::new().unwrap();
    let linker = CachedLinker {
        size_tool: first.path().join("size"),
        cache: cache.path().to_path_buf(),
        links: std::sync::atomic::AtomicUsize::new(0),
    };
    let extra = LinkExtraArgs::default();
    let mut perf = PerfTimer::new("cache-first");
    perf.activate();
    linker
        .link_all(
            &[object_in(first.path(), b"same object")],
            &[],
            first.path(),
            &extra,
            false,
            &mut perf,
        )
        .await
        .unwrap();

    let second = tempfile::TempDir::new().unwrap();
    let mut perf2 = PerfTimer::new("cache-second");
    perf2.activate();
    let result = linker
        .link_all(
            &[object_in(second.path(), b"same object")],
            &[],
            second.path(),
            &extra,
            false,
            &mut perf2,
        )
        .await
        .unwrap();

    assert_eq!(
        linker.links.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "identical inputs in a fresh directory must not relink"
    );
    assert!(
        perf2.recorded("link-restore").is_some(),
        "the restore must be attributed"
    );
    assert_eq!(
        std::fs::read(second.path().join("firmware.elf")).unwrap(),
        b"\x7fELF cached"
    );
    assert!(
        result.bin_path.is_some(),
        "the convert leg still has to run"
    );

    // Different object bytes are a different link and must not hit.
    let third = tempfile::TempDir::new().unwrap();
    let mut perf3 = PerfTimer::new("cache-third");
    perf3.activate();
    linker
        .link_all(
            &[object_in(third.path(), b"different object")],
            &[],
            third.path(),
            &extra,
            false,
            &mut perf3,
        )
        .await
        .unwrap();
    assert_eq!(
        linker.links.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "changed inputs must relink"
    );
}
