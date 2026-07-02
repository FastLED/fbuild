#!/usr/bin/env bash
# In-container entry point for the fbuild profiling harness
# (FastLED/fbuild#942). Runs inside the fbuild-profile-linux image with
# the fbuild source bind-mounted at /work and an artifact dir at /out.
#
# Scenarios (each iterated FBUILD_PROFILE_ITERS times, default 3):
#   cold  — ~/.fbuild + ~/.platformio wiped, daemon killed, fresh
#           project copy. Measures downloads + installs + full compile.
#   warm  — global cache intact, daemon running, but a FRESH project
#           copy. Measures how much of a first build the global cache
#           actually saves (uncached work shows up here).
#   hot   — immediate no-change rebuild of the same project copy.
#           Measures fingerprint/fast-path overhead.
#
# Per scenario-iteration this captures:
#   * wall clock + /usr/bin/time -v resource stats
#   * full CLI stdout/stderr
#   * ~/.fbuild/prod/daemon/daemon.log (FBUILD_PERF_LOG phase summaries
#     are emitted by the daemon, not the CLI)
#   * on-CPU:  perf record -F 99 cpu-clock, system-wide, → flamegraph
#   * off-CPU: bpftrace sched_switch wait-time sums (best effort on
#     WSL2 kernels) + raw `perf -e sched:sched_switch` fallback data
set -uo pipefail

ITERS="${FBUILD_PROFILE_ITERS:-3}"
SCENARIOS="${FBUILD_PROFILE_SCENARIOS:-cold,warm,hot}"
ENV_NAME="${FBUILD_PROFILE_ENV:-demo}"
BUILD_TIMEOUT="${FBUILD_PROFILE_BUILD_TIMEOUT:-5400}"
OUT=/out
TEMPLATE=/opt/nightdriver
PROJ=/tmp/nightdriver
FG=/opt/FlameGraph

# Ubuntu's /usr/bin/perf wrapper refuses to run under a WSL2 kernel
# string; call the real binary directly.
PERF="$(ls -d /usr/lib/linux-tools-*/perf 2>/dev/null | head -1)"
if [[ -z "$PERF" ]]; then
    echo "FATAL: no perf binary found under /usr/lib/linux-tools-*" >&2
    exit 1
fi

log() { echo "[profile $(date -u +%H:%M:%S)] $*"; }

# ---------------------------------------------------------------- setup
log "relaxing perf/bpf restrictions (best effort)"
sysctl -qw kernel.perf_event_paranoid=-1 2>/dev/null || log "WARN: could not set perf_event_paranoid"
sysctl -qw kernel.kptr_restrict=0 2>/dev/null || true
mount -t debugfs debugfs /sys/kernel/debug 2>/dev/null || true

log "building fbuild + fbuild-daemon from /work (soldr cargo, frame pointers on)"
cd /work
# Frame pointers + debuginfo make perf's fp unwinder produce clean
# stacks on WSL2 (dwarf unwinding there is unreliable). This changes
# fbuild's OWN build only — firmware compile flags are untouched.
export RUSTFLAGS="-C force-frame-pointers=yes -C debuginfo=2"
soldr cargo build --release -p fbuild-cli -p fbuild-daemon 2>&1 | tail -20
build_rc=${PIPESTATUS[0]}
if [[ $build_rc -ne 0 ]]; then
    echo "FATAL: fbuild build failed (rc=$build_rc)" >&2
    exit "$build_rc"
fi
unset RUSTFLAGS

export PATH="/work/target/release:$PATH"
command -v fbuild >/dev/null || { echo "FATAL: fbuild not on PATH" >&2; exit 1; }
command -v fbuild-daemon >/dev/null || { echo "FATAL: fbuild-daemon not on PATH" >&2; exit 1; }
log "fbuild: $(fbuild --version 2>&1 | head -1)"

# Everything the system-under-test caches must die with this container.
export FBUILD_PERF_LOG=1
export RUST_LOG="${RUST_LOG:-info}"

DAEMON_LOG="$HOME/.fbuild/prod/daemon/daemon.log"

kill_daemon() {
    pkill -f fbuild-daemon 2>/dev/null && sleep 1 || true
}

wipe_fbuild_state() {
    kill_daemon
    rm -rf "$HOME/.fbuild" "$HOME/.platformio"
}

fresh_project() {
    rm -rf "$PROJ"
    cp -r "$TEMPLATE" "$PROJ"
}

git_sha() { git -C /work rev-parse --short HEAD 2>/dev/null || echo unknown; }

mkdir -p "$OUT"
cat > "$OUT/meta.json" <<EOF
{"fbuild_sha": "$(git_sha)", "env": "$ENV_NAME", "iters": $ITERS,
 "scenarios": "$SCENARIOS", "ncpu": $(nproc),
 "kernel": "$(uname -r)", "date_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"}
EOF
: > "$OUT/timings.jsonl"

# --------------------------------------------------------- one profiled run
run_one() {
    local scenario="$1" iter="$2"
    local dir="$OUT/${scenario}-${iter}"
    mkdir -p "$dir"
    log "=== $scenario iter $iter ==="

    # On-CPU sampler: system-wide so daemon + compiler subprocesses are
    # all visible. cpu-clock because WSL2 exposes no hardware PMU.
    "$PERF" record -F 99 -e cpu-clock -g --call-graph fp -a \
        -o "$dir/oncpu.data" -- sleep "$BUILD_TIMEOUT" &>/dev/null &
    local oncpu_pid=$!

    # Off-CPU sampler #1: bpftrace sched_switch wait-time sums. Known
    # to fail on some WSL2 kernels — best effort, fallback below.
    bpftrace ci/docker-profile/offcpu.bt > "$dir/offcpu-bpftrace.txt" 2> "$dir/offcpu-bpftrace.err" &
    local bpf_pid=$!

    # Off-CPU sampler #2 (fallback): raw sched_switch events via perf.
    "$PERF" record -e sched:sched_switch -g --call-graph fp -a \
        -o "$dir/offcpu-sched.data" -- sleep "$BUILD_TIMEOUT" &>/dev/null &
    local sched_pid=$!

    sleep 1  # let samplers attach

    local t0 t1 rc
    t0=$(date +%s.%N)
    timeout "$BUILD_TIMEOUT" /usr/bin/time -v -o "$dir/time-v.txt" \
        fbuild build "$PROJ" -e "$ENV_NAME" \
        > "$dir/cli-stdout.log" 2> "$dir/cli-stderr.log"
    rc=$?
    t1=$(date +%s.%N)

    # Stop samplers (perf flushes its file on SIGINT/SIGTERM).
    kill -INT "$oncpu_pid" "$sched_pid" 2>/dev/null
    kill -INT "$bpf_pid" 2>/dev/null
    wait "$oncpu_pid" "$sched_pid" "$bpf_pid" 2>/dev/null

    local wall
    wall=$(echo "$t1 $t0" | awk '{printf "%.2f", $1 - $2}')
    echo "{\"scenario\": \"$scenario\", \"iter\": $iter, \"wall_s\": $wall, \"exit\": $rc}" >> "$OUT/timings.jsonl"
    log "$scenario iter $iter: ${wall}s (exit $rc)"

    # The FBUILD_PERF_LOG phase summary is emitted by whichever process
    # owns the timer — usually the daemon → daemon.log.
    [[ -f "$DAEMON_LOG" ]] && cp "$DAEMON_LOG" "$dir/daemon.log"
    grep -h "perf-log" "$dir/cli-stderr.log" "$dir/daemon.log" 2>/dev/null > "$dir/perf-log-lines.txt" || true

    # Flamegraphs (best effort — never fail the run over rendering).
    (
        cd "$dir"
        "$PERF" script -i oncpu.data 2>/dev/null \
            | "$FG/stackcollapse-perf.pl" > oncpu.folded 2>/dev/null
        [[ -s oncpu.folded ]] && "$FG/flamegraph.pl" --title "$scenario-$iter on-CPU" \
            oncpu.folded > oncpu.svg 2>/dev/null
        if [[ -s offcpu-bpftrace.txt ]]; then
            "$FG/stackcollapse-bpftrace.pl" offcpu-bpftrace.txt > offcpu.folded 2>/dev/null
            [[ -s offcpu.folded ]] && "$FG/flamegraph.pl" --color=io --countname=us \
                --title "$scenario-$iter off-CPU" offcpu.folded > offcpu.svg 2>/dev/null
        fi
        # Raw sched data is kept for offline analysis; a rendered
        # flamegraph from switch counts alone would be misleading.
        rm -f oncpu.data.old
    ) || true

    return "$rc"
}

# ------------------------------------------------------------- main loop
overall_rc=0
IFS=',' read -ra WANT <<< "$SCENARIOS"
for iter in $(seq 1 "$ITERS"); do
    for scenario in "${WANT[@]}"; do
        case "$scenario" in
            cold)
                wipe_fbuild_state
                fresh_project
                ;;
            warm)
                fresh_project
                ;;
            hot)
                # no-op rebuild: same project, same cache, daemon up.
                # Guard for a hot-only invocation with no prior copy.
                [[ -d "$PROJ" ]] || fresh_project
                ;;
            *)
                log "unknown scenario '$scenario', skipping"
                continue
                ;;
        esac
        run_one "$scenario" "$iter" || overall_rc=1
    done
done

kill_daemon
log "done. artifacts in $OUT (timings.jsonl, per-run dirs)"
cat "$OUT/timings.jsonl"
exit "$overall_rc"
