//! P-02 cold-resolver benchmark.
//!
//! Phase 7 of <https://github.com/FastLED/fbuild/issues/205> sets a ≤ 200 ms
//! cold budget for `resolve()` on a typical teensy41-class project. This
//! benchmark builds a synthetic ~30-library framework tree (matches
//! Teensyduino's library count) on a `MiniFramework` tempdir and walks it
//! end-to-end on every iteration. No cache layer sits in front of `resolve()`
//! today (Phase 4 cache memoization waits on zccache#130), so each iteration
//! is genuinely cold from the resolver's point of view — the OS page cache
//! warms up after the first run, but the resolver still re-canonicalizes
//! every include dir and re-walks every seed.
//!
//! Run with:
//!
//! ```text
//! soldr cargo bench -p fbuild-library-select --bench resolve_cold
//! ```
//!
//! Future PRs gate against the baseline number this captures.

use std::path::PathBuf;

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use fbuild_library_select::resolve;
use fbuild_packages::library::framework_library::discover_framework_libraries;
use fbuild_packages::library::FrameworkLibrary;
use fbuild_test_support::MiniFramework;

/// Approximate Teensyduino library count. Exact value isn't load-bearing — we
/// just want the fixture to be in the right order of magnitude so the bench
/// surfaces the same allocation/IO patterns a real teensy41 project hits.
const LIB_COUNT: usize = 30;

/// Length of the transitive include chain rooted at `Lib00`. The other
/// `LIB_COUNT - CHAIN_LEN` libraries are unreferenced and must NOT be selected
/// — that's the #204 regression case the resolver is built around. Keeping
/// some unselected libs in the fixture means the bench measures the cost of
/// rejecting them too, not just the cost of the selected closure.
const CHAIN_LEN: usize = 5;

fn build_fixture() -> (
    MiniFramework,
    Vec<PathBuf>,
    Vec<PathBuf>,
    Vec<FrameworkLibrary>,
) {
    let mut mf = MiniFramework::new();
    for i in 0..LIB_COUNT {
        let name = format!("Lib{i:02}");
        let next = if i + 1 < CHAIN_LEN {
            Some(format!("Lib{:02}", i + 1))
        } else {
            None
        };
        let header = if let Some(n) = &next {
            format!("#pragma once\n#include <{n}.h>\n")
        } else {
            "#pragma once\n".to_string()
        };
        let cpp = format!("#include <{name}.h>\nvoid {name}_func() {{}}\n");
        mf.add_library(&name).header(&header).cpp(&cpp).done();
    }
    mf.sketch("#include <Lib00.h>\nvoid setup() {}\nvoid loop() {}\n");

    let libs = discover_framework_libraries(&mf.libraries_dir());
    let seeds = mf.project_seeds();
    let search_paths = mf.project_search_paths();
    (mf, seeds, search_paths, libs)
}

fn bench_resolve(c: &mut Criterion) {
    let (mf, seeds, search_paths, libs) = build_fixture();
    let mut group = c.benchmark_group("resolve");
    group.throughput(Throughput::Elements(libs.len() as u64));
    group.bench_function("cold_30_libs_chain_5", |b| {
        b.iter(|| {
            let sel = resolve(
                black_box(&seeds),
                black_box(&search_paths),
                black_box(&libs),
            );
            black_box(sel);
        });
    });
    group.finish();
    // Keep `mf` alive until after the bench so the temp dir doesn't get
    // cleaned out from under `resolve()` mid-run.
    drop(mf);
}

criterion_group!(benches, bench_resolve);
criterion_main!(benches);
