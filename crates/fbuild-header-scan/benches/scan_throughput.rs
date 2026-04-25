//! Criterion benchmark for `fbuild_header_scan::scan` throughput.
//!
//! P-03 of FastLED/fbuild#205: capture single-thread MB/s on three input
//! sizes (tiny / medium / large) so future PRs can regress against a
//! recorded baseline. The aspirational threshold is ≥ 50 MB/s
//! single-thread; this harness records the number but does not gate CI.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use fbuild_header_scan::scan;

/// Generate a synthetic C++ source string at least `target_bytes` long.
///
/// The template intentionally exercises the scanner's adversary paths:
/// angled + quoted `#include`, line and multi-line block comments
/// containing fake `#include`s, string and raw-string literals with
/// embedded `#include` payloads, identifiers ending in `R` / `L`
/// (which must NOT be treated as raw-string prefixes), and a char
/// literal containing `#`. Repeated until we hit the byte budget.
fn fixture(target_bytes: usize) -> String {
    let template = "\
        #include <a.h>\n\
        // comment with #include <not_real.h>\n\
        const char* s = \"#include <also_not_real.h>\";\n\
        const char* r = R\"(#include <not_real_either.h>)\";\n\
        auto FooR = 0; // identifier ending in R, NOT a raw string\n\
        auto FooL = 1; // identifier ending in L, NOT a wide-string prefix\n\
        /* block\n   #include <inside_block.h>\n*/\n\
        char c = '#';\n\
        #include \"b.h\"\n\
    ";
    let mut s = String::with_capacity(target_bytes + template.len());
    while s.len() < target_bytes {
        s.push_str(template);
    }
    s
}

fn bench_scanner(c: &mut Criterion) {
    let mut group = c.benchmark_group("scan");
    for (name, size) in [
        ("tiny", 64usize),
        ("medium", 100 * 1024),
        ("large", 2 * 1024 * 1024),
    ] {
        let src = fixture(size);
        let actual_len = src.len();
        group.throughput(Throughput::Bytes(actual_len as u64));
        group.bench_function(name, |b| {
            b.iter(|| {
                let refs = scan(black_box(&src));
                black_box(refs);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_scanner);
criterion_main!(benches);
