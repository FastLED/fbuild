//! P-01 (mini) warm-resolver benchmark.
//!
//! Phase 4 of <https://github.com/FastLED/fbuild/issues/205> shipped
//! [`fbuild_library_select::cache::resolve_cached`], a `KvStore`-backed memo
//! in front of [`fbuild_library_select::resolve`]. AC#5 / P-01 of #205 sets
//! a "warm library-selection ≤ current fbuild + 50 ms" goal; the real
//! per-board matrix lives under `bench/fastled-examples/` and waits on a
//! checked-out FastLED tree. This bench is the per-crate counterpart: it
//! measures the synthetic warm path against the same `MiniFramework`
//! fixture as `resolve_cold`, so we have a regression guard for the cache-
//! hit code path independent of the larger matrix.
//!
//! Structure mirrors `resolve_cold.rs`: build the ~30-library tree once,
//! open a `KvStore` once, prime the cache with one untimed `resolve_cached`
//! call, then time only the second invocation (the hit path).
//!
//! Run with:
//!
//! ```text
//! soldr cargo bench -p fbuild-library-select --bench resolve_warm
//! ```
//!
//! Follows up #215 (mini bench) and #205 Phase 7 (perf budgets).

use std::path::PathBuf;

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use fbuild_library_select::cache::{resolve_cached, CacheKeyInputs};
use fbuild_packages::library::framework_library::discover_framework_libraries;
use fbuild_packages::library::FrameworkLibrary;
use fbuild_test_support::MiniFramework;
use zccache_artifact::KvStore;

const LIB_COUNT: usize = 30;
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

fn bench_resolve_warm(c: &mut Criterion) {
    let (mf, seeds, search_paths, libs) = build_fixture();
    let kv_dir = tempfile::tempdir().expect("resolve_warm: failed to create kv tempdir");
    let kv = KvStore::open(kv_dir.path().join("kv")).expect("resolve_warm: KvStore::open failed");

    let framework_root = mf.framework_root().to_path_buf();
    let inputs = CacheKeyInputs {
        toolchain_triple: "avr-unknown-none",
        framework_install_path: &framework_root,
        framework_version: "1.59.0",
    };

    // Prime the cache so the timed loop measures the hit path only.
    let primed = resolve_cached(&seeds, &search_paths, &libs, &inputs, &kv)
        .expect("resolve_warm: prime resolve_cached failed");
    assert!(
        !primed.from_cache,
        "resolve_warm: priming call must miss (got hit)"
    );

    // WHY: confirm the very next call hits before entering the bench loop;
    // otherwise we'd silently measure cold work and report a misleading
    // baseline.
    let probe = resolve_cached(&seeds, &search_paths, &libs, &inputs, &kv)
        .expect("resolve_warm: probe resolve_cached failed");
    assert!(
        probe.from_cache,
        "resolve_warm: second call did not hit cache; bench would measure misses"
    );

    let mut group = c.benchmark_group("resolve");
    group.throughput(Throughput::Elements(libs.len() as u64));
    group.bench_function("warm_30_libs_chain_5", |b| {
        b.iter(|| {
            let res = resolve_cached(
                black_box(&seeds),
                black_box(&search_paths),
                black_box(&libs),
                black_box(&inputs),
                black_box(&kv),
            )
            .expect("resolve_warm: resolve_cached failed inside bench loop");
            if !res.from_cache {
                panic!("resolve_warm: bench iteration missed the cache");
            }
            black_box(res);
        });
    });
    group.finish();
    drop(mf);
    drop(kv_dir);
}

criterion_group!(benches, bench_resolve_warm);
criterion_main!(benches);
