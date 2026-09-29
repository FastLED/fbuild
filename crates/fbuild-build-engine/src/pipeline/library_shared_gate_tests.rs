//! Proves `LibDeps::compile` and `compile_sources_parallel_shared` share ONE
//! job budget (FastLED/fbuild#1559): a fake in-process core compile and a
//! resolved `LibDeps` (two symlinked local libraries, compiled via real
//! fake-shell-script gcc/g++/ar) run in `tokio::join!` against the same
//! `JobGate`. Overlap between the two phases (never possible before
//! FastLED/fbuild#1468/#1559 unified the gate) plus a total in-flight count
//! that never exceeds the gate size proves the sharing.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fbuild_core::Result as FbuildResult;
use fbuild_core::path::NormalizedPath;

use super::resolve_lib_deps;
use crate::compiler::{CompileResult, Compiler};
use crate::flag_overlay::LanguageExtraFlags;
use crate::parallel::compile_sources_parallel_shared;
use std::path::Path;

/// Fake in-process "core" compiler: records a (label, phase, ns) event to
/// the shared log for every compile, sleeping long enough that at least
/// one lib_deps archive compile is guaranteed to overlap it.
struct FakeCoreCompiler {
    gcc: NormalizedPath,
    log: Arc<std::sync::Mutex<Vec<(String, String, u128)>>>,
    sleep_ms: u64,
}

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

#[async_trait::async_trait]
impl Compiler for FakeCoreCompiler {
    async fn compile_one(
        &self,
        _compiler_path: &Path,
        _source: &Path,
        output: &Path,
        _flags: &[String],
        _extra_flags: &[String],
    ) -> FbuildResult<CompileResult> {
        self.log
            .lock()
            .unwrap()
            .push(("core".to_string(), "start".to_string(), now_ns()));
        tokio::time::sleep(std::time::Duration::from_millis(self.sleep_ms)).await;
        self.log
            .lock()
            .unwrap()
            .push(("core".to_string(), "end".to_string(), now_ns()));
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(output, "").unwrap();
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

/// Installs a fake gcc/g++ that logs "<basename> start <ns>" /
/// "<basename> end <ns>" to `log` and sleeps ~0.1s before touching `-o`.
/// Staged via `.staging` + `set_executable` + `rename` to avoid ETXTBSY
/// (FastLED's `symlink://` local libraries recompile on every resolve).
fn install_timed_fake_compiler(path: &Path, log: &Path) {
    let script = format!(
        "#!/bin/sh\n\
         obj=\"\"\n\
         src=\"\"\n\
         prev=\"\"\n\
         for arg in \"$@\"; do\n\
         \x20 if [ \"$prev\" = \"-o\" ]; then obj=\"$arg\"; fi\n\
         \x20 if [ \"$prev\" = \"-c\" ]; then src=\"$arg\"; fi\n\
         \x20 prev=\"$arg\"\n\
         done\n\
         name=$(basename \"$src\")\n\
         echo \"$name start $(date +%s%N)\" >> \"{log}\"\n\
         sleep 0.1\n\
         echo \"$name end $(date +%s%N)\" >> \"{log}\"\n\
         mkdir -p \"$(dirname \"$obj\")\"\n\
         touch \"$obj\"\n",
        log = log.display()
    );
    let staging = path.with_extension("staging");
    std::fs::write(&staging, script).unwrap();
    fbuild_core::platform::fs::set_executable(&staging).unwrap();
    std::fs::rename(&staging, path).unwrap();
}

fn install_fake_ar(path: &Path) {
    let script = "#!/bin/sh\ntouch \"$2\"\n";
    let staging = path.with_extension("staging");
    std::fs::write(&staging, script).unwrap();
    fbuild_core::platform::fs::set_executable(&staging).unwrap();
    std::fs::rename(&staging, path).unwrap();
}

fn make_local_library(project: &Path, rel: &str, name: &str) {
    let src = project.join(rel).join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join(format!("{name}.cpp")), "int f() { return 1; }\n").unwrap();
    std::fs::write(src.join(format!("{name}.h")), "int f();\n").unwrap();
}

/// Parses "<name> start|end <ns>" lines from a fake-compiler log into
/// `(name, start_ns, end_ns)` intervals.
fn parse_intervals(log_text: &str) -> Vec<(String, u128, u128)> {
    let mut starts: std::collections::HashMap<String, u128> = std::collections::HashMap::new();
    let mut out = Vec::new();
    for line in log_text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            continue;
        }
        let (name, phase, ts) = (parts[0], parts[1], parts[2]);
        let ts: u128 = ts.parse().unwrap();
        match phase {
            "start" => {
                starts.insert(name.to_string(), ts);
            }
            "end" => {
                if let Some(start) = starts.remove(name) {
                    out.push((name.to_string(), start, ts));
                }
            }
            _ => {}
        }
    }
    out
}

fn intervals_overlap(a: (u128, u128), b: (u128, u128)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Sweep line over every interval's start/end events; returns the peak
/// number of simultaneously in-flight compiles.
fn max_concurrency(intervals: &[(u128, u128)]) -> usize {
    let mut events: Vec<(u128, i32)> = Vec::new();
    for &(start, end) in intervals {
        events.push((start, 1));
        events.push((end, -1));
    }
    events.sort();
    let (mut cur, mut max) = (0i32, 0i32);
    for (_, delta) in events {
        cur += delta;
        max = max.max(cur);
    }
    max.max(0) as usize
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lib_deps_and_core_share_one_job_gate() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    for _ in 0..5 {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        make_local_library(&project, "liba", "liba");
        make_local_library(&project, "libb", "libb");
        let build_dir = tmp.path().join("build");
        std::fs::create_dir_all(&build_dir).unwrap();

        let tools_dir = tmp.path().join("tools");
        std::fs::create_dir_all(&tools_dir).unwrap();
        let lib_log = tmp.path().join("lib_compile.log");
        let gcc = tools_dir.join("gcc");
        let gxx = tools_dir.join("g++");
        let ar = tools_dir.join("ar");
        install_timed_fake_compiler(&gcc, &lib_log);
        install_timed_fake_compiler(&gxx, &lib_log);
        install_fake_ar(&ar);

        let mut include_dirs = Vec::new();
        let lib_deps = resolve_lib_deps(
            &[
                "LibA=symlink://liba".to_string(),
                "LibB=symlink://libb".to_string(),
            ],
            &[],
            &project,
            &build_dir,
            &gcc,
            &gxx,
            &ar,
            &ar,
            &[],
            &[],
            &mut include_dirs,
            false,
            None,
        )
        .await
        .unwrap();

        let core_log: Arc<std::sync::Mutex<Vec<(String, String, u128)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let core_compiler = FakeCoreCompiler {
            gcc: NormalizedPath::new(&gcc),
            log: core_log.clone(),
            // Long enough that the two ~0.1s library compiles (which
            // must serialize behind each other once the gate's second
            // slot is claimed by this core compile) are guaranteed to
            // land inside this compile's [start, end) window.
            sleep_ms: 300,
        };
        let core_source = project.join("core.cpp");
        std::fs::write(&core_source, "int g();\n").unwrap();
        let core_build_dir = build_dir.join("core");

        let gate = fbuild_packages::library::library_compiler::job_gate(2);
        let core_sources = [core_source.clone()];
        let core_flags = LanguageExtraFlags::default();
        let (core_result, lib_archives) = tokio::join!(
            compile_sources_parallel_shared(
                &core_compiler,
                &core_sources,
                &core_build_dir,
                &core_flags,
                &gate,
                None,
                None,
            ),
            lib_deps.compile(&gate),
        );

        let core_result = core_result.unwrap();
        assert_eq!(core_result.objects.len(), 1);
        let lib_archives = lib_archives.unwrap();
        assert_eq!(
            lib_archives.len(),
            2,
            "both lib_deps archives must be produced"
        );
        for archive in &lib_archives {
            assert!(archive.exists(), "{} must exist", archive.display());
        }

        // Merge the in-process core events with the subprocess lib
        // events recorded by the fake gcc/g++ scripts.
        let core_events = core_log.lock().unwrap().clone();
        let core_intervals: Vec<(u128, u128)> = {
            let text = core_events
                .iter()
                .map(|(label, phase, ts)| format!("{label} {phase} {ts}"))
                .collect::<Vec<_>>()
                .join("\n");
            parse_intervals(&text)
                .into_iter()
                .map(|(_, s, e)| (s, e))
                .collect()
        };
        assert_eq!(core_intervals.len(), 1, "exactly one core compile ran");
        let core_interval = core_intervals[0];

        let lib_log_text = std::fs::read_to_string(&lib_log).unwrap();
        let lib_intervals: Vec<(u128, u128)> = parse_intervals(&lib_log_text)
            .into_iter()
            .map(|(_, s, e)| (s, e))
            .collect();
        assert_eq!(
            lib_intervals.len(),
            2,
            "both library TU compiles must be logged"
        );

        // (b) the library compile overlaps the core compile — not
        // strictly sequential.
        assert!(
            lib_intervals
                .iter()
                .any(|&lib_interval| intervals_overlap(core_interval, lib_interval)),
            "expected at least one lib_deps compile to overlap the core compile: \
             core={core_interval:?} libs={lib_intervals:?}"
        );

        // (a) total in-flight across both never exceeds the gate size.
        let mut all_intervals = lib_intervals.clone();
        all_intervals.push(core_interval);
        let peak = max_concurrency(&all_intervals);
        assert!(
            peak <= 2,
            "gate of size 2 must never be exceeded, saw peak={peak}"
        );

        // The two lib compiles must not run concurrently with each
        // other AND the core compile at once (only one gate, size 2:
        // core takes one slot for its whole 300ms, so the two lib
        // compiles must themselves serialize behind the gate's other
        // slot).
        assert!(
            !intervals_overlap(lib_intervals[0], lib_intervals[1]),
            "with only one non-core slot, the two lib_deps TUs must not overlap \
             each other: {:?} vs {:?}",
            lib_intervals[0],
            lib_intervals[1]
        );
    }
}
