//! `fbuild daemon ...` subcommand handlers (status, restart, kill, locks,
//! cache-stats, gc, monitor-tail) plus process-management helpers shared
//! with the purge subcommand.

use crate::daemon_client::{self, DaemonClient};

use super::args::DaemonAction;
use super::purge::format_size;
use super::show::run_show;

pub async fn run_daemon(action: DaemonAction) -> fbuild_core::Result<()> {
    let client = DaemonClient::new();
    match action {
        DaemonAction::Stop => {
            if !client.health().await {
                println!("daemon is not running");
                return Ok(());
            }
            client.shutdown().await?;
            // Wait for it to actually stop
            for _ in 0..50 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if !client.health().await {
                    println!("daemon stopped");
                    return Ok(());
                }
            }
            println!("daemon stop requested (may still be shutting down)");
        }
        DaemonAction::Status => {
            if client.health().await {
                match client.daemon_info().await {
                    Ok(info) => {
                        let uptime = format_uptime(info.uptime_seconds);
                        println!("daemon is running at {}", fbuild_paths::get_daemon_url());
                        println!("  PID:     {}", info.pid);
                        println!("  Port:    {}", info.port);
                        println!("  Uptime:  {}", uptime);
                        println!("  Version: {}", info.version);
                        println!("  Mode:    {}", if info.dev_mode { "dev" } else { "prod" });
                        println!("  State:   {}", info.daemon_state);
                        if info.operation_in_progress {
                            if let Some(ref op) = info.current_operation {
                                println!("  Operation: {}", op);
                            } else {
                                println!("  Operation: (in progress)");
                            }
                        }
                        if let Some(ref install) = info.dependency_install {
                            println!(
                                "  Install: {} {} ({:?}, {:?})",
                                install.name,
                                install.version.as_deref().unwrap_or(""),
                                install.phase,
                                install.role
                            );
                            println!("           {}", install.message);
                        }
                        if info.client_count > 0 {
                            println!("  Clients: {}", info.client_count);
                        }
                        if let Some(ref cwd) = info.spawner_cwd {
                            println!("  Spawned from: {}", cwd);
                        }
                        if let Some(mtime) = info.source_mtime {
                            println!("  Binary mtime: {:.0}", mtime);
                        }
                    }
                    Err(_) => {
                        println!("daemon is running at {}", fbuild_paths::get_daemon_url());
                    }
                }
            } else {
                println!("daemon is not running");
            }
        }
        DaemonAction::Restart => {
            // Stop if running
            if client.health().await {
                client.shutdown().await?;
                for _ in 0..50 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    if !client.health().await {
                        break;
                    }
                }
            }
            // Start fresh
            daemon_client::ensure_daemon_running().await?;
            println!("daemon restarted");
        }
        DaemonAction::List => {
            run_daemon_list(&client).await?;
        }
        DaemonAction::Kill { pid, force } => {
            run_daemon_kill(&client, pid, force).await?;
        }
        DaemonAction::KillAll { force } => {
            run_daemon_kill_all(force)?;
        }
        DaemonAction::Locks => {
            run_daemon_locks(&client).await?;
        }
        DaemonAction::ClearLocks => {
            run_daemon_clear_locks(&client).await?;
        }
        DaemonAction::CacheStats => {
            run_daemon_cache_stats(&client).await?;
        }
        DaemonAction::Gc => {
            run_daemon_gc(&client).await?;
        }
        DaemonAction::Monitor { no_follow, lines } => {
            return run_show("daemon", !no_follow, lines);
        }
        DaemonAction::RunningProcess { json } => {
            run_daemon_running_process(json)?;
        }
    }
    Ok(())
}

fn run_daemon_running_process(json: bool) -> fbuild_core::Result<()> {
    use fbuild_paths::running_process as rp;

    let mode = rp::running_process_daemon_mode();
    let service_definition_path = rp::running_process_service_definition_path();
    let daemon_candidate = daemon_executable_candidate();
    let daemon_candidate_exists = daemon_candidate.exists();

    // v1 broker adoption lives in `fbuild-daemon`'s `broker` module
    // (zackees/running-process#437 / FastLED/fbuild#510 / #560). The
    // dependency-free facts this diagnostic prints (cache roots + display
    // constants) come from `fbuild-paths::running_process` so the CLI need not
    // depend on the daemon. The diagnostics command surfaces the live adoption
    // facts (registered payload protocol, isolation modes, encoding lane, cache
    // roots) instead of the prior stubbed preview.
    let runtime_dir = daemon_candidate
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let cache_roots = rp::CacheRoots::discover(runtime_dir);
    let selected_path = if rp::running_process_disabled() {
        "direct"
    } else {
        "broker"
    };
    let last_acquisition = daemon_client::last_daemon_acquisition();
    let live_mode = last_acquisition
        .as_ref()
        .map(|a| a.mode())
        .unwrap_or_else(|| mode.as_str());
    let negotiated_endpoint = last_acquisition.as_ref().and_then(|a| a.endpoint());
    let negotiated_daemon_version = last_acquisition.as_ref().and_then(|a| a.daemon_version());
    let fallback_reason = last_acquisition.as_ref().and_then(|a| a.reason());

    if json {
        let payload = serde_json::json!({
            "service_name": rp::SERVICE_NAME,
            "broker_isolation": rp::BROKER_ISOLATION,
            "ci_isolation": "EXPLICIT_INSTANCE",
            "ci_instance": rp::CI_TRUSTED_INSTANCE,
            "min_version": rp::MIN_VERSION,
            "payload_protocol": format!("{:#06X}", rp::FBUILD_PAYLOAD_PROTOCOL),
            "fbuild_protocol_version": rp::FBUILD_PROTOCOL_VERSION,
            "encoding_lane": "json-direct + prost-broker (parity-tested)",
            "service_definition": {
                "file_name": rp::SERVICE_DEFINITION_FILE_NAME,
                "template": rp::SERVICE_DEFINITION_TEMPLATE,
                "path": service_definition_path.display().to_string(),
            },
            "cache_roots": {
                "artifact": cache_roots.artifact.display().to_string(),
                "index": cache_roots.index.display().to_string(),
                "temp": cache_roots.temp.display().to_string(),
                "log": cache_roots.log.display().to_string(),
                "lock": cache_roots.lock.display().to_string(),
                "runtime": cache_roots.runtime.display().to_string(),
                "config": cache_roots.config.display().to_string(),
            },
            "daemon": {
                "binary_name": rp::DAEMON_BINARY_NAME,
                "candidate_path": daemon_candidate.display().to_string(),
                "candidate_exists": daemon_candidate_exists,
                "endpoint": fbuild_paths::get_daemon_url(),
            },
            "mode": {
                "current": live_mode,
                "selected_path": selected_path,
                "uses_direct_fallback": live_mode != "broker-negotiated",
                "running_process_disabled": rp::running_process_disabled(),
                "broker_requested": rp::running_process_broker_requested(),
                "summary": rp::running_process_adoption_summary(),
                "negotiated_endpoint": negotiated_endpoint,
                "negotiated_daemon_version": negotiated_daemon_version,
                "fallback_reason": fallback_reason,
            },
        });
        let output = serde_json::to_string_pretty(&payload)
            .map_err(|e| fbuild_core::FbuildError::Other(format!("json serialize: {e}")))?;
        println!("{output}");
        return Ok(());
    }

    println!("running-process broker adoption");
    println!("  Service:              {}", rp::SERVICE_NAME);
    println!("  Isolation (local):    {}", rp::BROKER_ISOLATION);
    println!(
        "  Isolation (CI):       EXPLICIT_INSTANCE \"{}\"",
        rp::CI_TRUSTED_INSTANCE
    );
    println!("  Min version:          {}", rp::MIN_VERSION);
    println!(
        "  Payload protocol:     {:#06X}",
        rp::FBUILD_PAYLOAD_PROTOCOL
    );
    println!("  fbuild proto version: {}", rp::FBUILD_PROTOCOL_VERSION);
    println!("  Encoding lane:        json-direct + prost-broker (parity-tested)");
    println!(
        "  Service definition:   {}",
        service_definition_path.display()
    );
    println!("  Selected path:        {selected_path}");
    println!("  Mode:                 {live_mode}");
    if let Some(endpoint) = negotiated_endpoint {
        println!("  Negotiated endpoint:  {endpoint}");
    }
    if let Some(version) = negotiated_daemon_version {
        println!("  Negotiated version:   {version}");
    }
    if let Some(reason) = fallback_reason {
        println!("  Fallback reason:      {reason}");
    }
    println!("  Daemon endpoint:      {}", fbuild_paths::get_daemon_url());
    println!("  Daemon binary:        {}", daemon_candidate.display());
    println!(
        "  Daemon binary exists: {}",
        if daemon_candidate_exists { "yes" } else { "no" }
    );
    println!("  Cache roots:");
    println!("    - artifact: {}", cache_roots.artifact.display());
    println!("    - index:    {}", cache_roots.index.display());
    println!("    - temp:     {}", cache_roots.temp.display());
    println!("    - log:      {}", cache_roots.log.display());
    println!("    - lock:     {}", cache_roots.lock.display());
    println!("    - runtime:  {}", cache_roots.runtime.display());
    println!("    - config:   {}", cache_roots.config.display());
    Ok(())
}

fn daemon_executable_candidate() -> std::path::PathBuf {
    let Some(parent) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    else {
        return std::path::PathBuf::from(fbuild_paths::running_process::DAEMON_BINARY_NAME);
    };
    parent.join(fbuild_paths::running_process::DAEMON_BINARY_NAME)
}

pub async fn run_daemon_list(client: &DaemonClient) -> fbuild_core::Result<()> {
    if client.health().await {
        match client.daemon_info().await {
            Ok(info) => {
                let uptime = format_uptime(info.uptime_seconds);
                println!("fbuild daemon (running)");
                println!("  PID:     {}", info.pid);
                println!("  Port:    {}", info.port);
                println!("  Uptime:  {}", uptime);
                println!("  Version: {}", info.version);
                println!("  Mode:    {}", if info.dev_mode { "dev" } else { "prod" });
            }
            Err(e) => {
                println!("daemon is running but info unavailable: {}", e);
            }
        }
    } else {
        println!("no daemon is running");
        // Check for stale PID file
        let pid_file = fbuild_paths::get_daemon_pid_file();
        if pid_file.exists() {
            if let Ok(contents) = std::fs::read_to_string(&pid_file) {
                println!(
                    "  (stale PID file: {} — PID {})",
                    pid_file.display(),
                    contents.trim()
                );
            }
        }
    }

    // Also scan for orphan processes
    let pids = find_daemon_pids()?;
    if pids.len() > 1 {
        println!("\nwarning: multiple fbuild-daemon processes detected:");
        for pid in &pids {
            println!("  PID {}", pid);
        }
        println!("use 'fbuild daemon kill-all' to clean up");
    }
    Ok(())
}

pub async fn run_daemon_locks(client: &DaemonClient) -> fbuild_core::Result<()> {
    if !client.health().await {
        println!("daemon is not running");
        return Ok(());
    }

    let status = client.lock_status().await?;

    // Display port locks
    if status.port_locks.is_empty() {
        println!("Port Locks: (none)");
    } else {
        println!("Port Locks:");
        for lock in &status.port_locks {
            let state = if lock.is_held { "HELD" } else { "FREE" };
            let writer = lock.writer_client_id.as_deref().unwrap_or("none");
            println!(
                "  {} [{}] open={} writer={} readers={}",
                lock.port, state, lock.is_open, writer, lock.reader_count
            );
        }
    }

    // Display project locks
    if status.project_locks.is_empty() {
        println!("Project Locks: (none)");
    } else {
        println!("Project Locks:");
        for lock in &status.project_locks {
            let state = if lock.is_held { "HELD" } else { "FREE" };
            println!("  {} [{}]", lock.project_dir, state);
        }
    }

    if !status.stale_locks.is_empty() {
        println!(
            "\nWarning: {} stale lock(s) detected. Use 'fbuild daemon clear-locks' to clear.",
            status.stale_locks.len()
        );
    }

    Ok(())
}

pub async fn run_daemon_clear_locks(client: &DaemonClient) -> fbuild_core::Result<()> {
    if !client.health().await {
        println!("daemon is not running");
        return Ok(());
    }

    let result = client.clear_locks().await?;
    println!("{}", result.message);
    if result.cleared_count > 0 {
        println!("Cleared {} lock(s)", result.cleared_count);
    }
    Ok(())
}

pub async fn run_daemon_cache_stats(client: &DaemonClient) -> fbuild_core::Result<()> {
    if !client.health().await {
        // Fall back to local cache stats if daemon isn't running
        match fbuild_packages::DiskCache::open() {
            Ok(dc) => {
                let stats = dc.stats().map_err(|e| {
                    fbuild_core::FbuildError::Other(format!("failed to read cache stats: {}", e))
                })?;
                println!("{}", stats);
            }
            Err(e) => {
                return Err(fbuild_core::FbuildError::Other(format!(
                    "failed to open disk cache: {}",
                    e
                )));
            }
        }
        return Ok(());
    }

    let stats = client.cache_stats().await?;
    if !stats.success {
        return Err(fbuild_core::FbuildError::Other(format!(
            "failed to get cache stats: {}",
            stats.message.as_deref().unwrap_or("unknown error")
        )));
    }
    println!("Disk Cache Statistics:");
    println!("  Entries:    {}", stats.entry_count);
    println!("  Installed:  {}", format_size(stats.installed_bytes));
    println!("  Archives:   {}", format_size(stats.archive_bytes));
    println!("  Total:      {}", format_size(stats.total_bytes));
    println!(
        "  Watermarks: {} high / {} low",
        format_size(stats.high_watermark),
        format_size(stats.low_watermark)
    );
    println!("  Archive budget: {}", format_size(stats.archive_budget));
    Ok(())
}

pub async fn run_daemon_gc(client: &DaemonClient) -> fbuild_core::Result<()> {
    if !client.health().await {
        // Fall back to local GC if daemon isn't running
        let dc = fbuild_packages::DiskCache::open().map_err(|e| {
            fbuild_core::FbuildError::Other(format!("failed to open disk cache: {}", e))
        })?;
        let report = dc
            .run_gc()
            .map_err(|e| fbuild_core::FbuildError::Other(format!("GC failed: {}", e)))?;
        print_gc_report(&report);
        return Ok(());
    }

    let result = client.run_gc().await?;
    if !result.success {
        return Err(fbuild_core::FbuildError::Other(format!(
            "GC failed: {}",
            result.message.as_deref().unwrap_or("unknown error")
        )));
    }
    println!("GC complete:");
    println!(
        "  Installed evicted: {} ({})",
        result.installed_evicted,
        format_size(result.installed_bytes_freed)
    );
    println!(
        "  Archives evicted:  {} ({})",
        result.archives_evicted,
        format_size(result.archive_bytes_freed)
    );
    println!(
        "  Total freed:       {}",
        format_size(result.total_bytes_freed)
    );
    if result.orphan_files_removed > 0 {
        println!("  Orphan files removed: {}", result.orphan_files_removed);
    }
    if result.orphan_rows_cleaned > 0 {
        println!("  Orphan rows cleaned:  {}", result.orphan_rows_cleaned);
    }
    Ok(())
}

pub fn print_gc_report(report: &fbuild_packages::disk_cache::GcReport) {
    if report.total_bytes_freed() == 0
        && report.orphan_files_removed == 0
        && report.orphan_rows_cleaned == 0
    {
        println!("GC: nothing to clean up");
        return;
    }
    println!("GC complete:");
    println!(
        "  Installed evicted: {} ({})",
        report.installed_evicted,
        format_size(report.installed_bytes_freed)
    );
    println!(
        "  Archives evicted:  {} ({})",
        report.archives_evicted,
        format_size(report.archive_bytes_freed)
    );
    println!(
        "  Total freed:       {}",
        format_size(report.total_bytes_freed())
    );
    if report.orphan_files_removed > 0 {
        println!("  Orphan files removed: {}", report.orphan_files_removed);
    }
    if report.orphan_rows_cleaned > 0 {
        println!("  Orphan rows cleaned:  {}", report.orphan_rows_cleaned);
    }
}

pub async fn run_daemon_kill(
    client: &DaemonClient,
    pid: Option<u32>,
    force: bool,
) -> fbuild_core::Result<()> {
    let target_pid = if let Some(p) = pid {
        p
    } else if client.health().await {
        match client.daemon_info().await {
            Ok(info) => info.pid,
            Err(_) => read_pid_from_file()?,
        }
    } else {
        read_pid_from_file()?
    };

    kill_process(target_pid, force)?;
    println!("killed daemon (PID {})", target_pid);
    let _ = std::fs::remove_file(fbuild_paths::get_daemon_pid_file());
    Ok(())
}

pub fn read_pid_from_file() -> fbuild_core::Result<u32> {
    let pid_file = fbuild_paths::get_daemon_pid_file();
    if pid_file.exists() {
        std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| {
                fbuild_core::FbuildError::DaemonError(
                    "could not parse PID from PID file".to_string(),
                )
            })
    } else {
        Err(fbuild_core::FbuildError::DaemonError(
            "no daemon running and no PID file found".to_string(),
        ))
    }
}

pub fn run_daemon_kill_all(force: bool) -> fbuild_core::Result<()> {
    let pids = find_daemon_pids()?;
    if pids.is_empty() {
        println!("no fbuild-daemon processes found");
        return Ok(());
    }

    let mut killed = 0;
    for pid in &pids {
        match kill_process(*pid, force) {
            Ok(()) => {
                println!("killed daemon (PID {})", pid);
                killed += 1;
            }
            Err(e) => {
                eprintln!("failed to kill PID {}: {}", pid, e);
            }
        }
    }

    let _ = std::fs::remove_file(fbuild_paths::get_daemon_pid_file());
    println!("killed {} daemon(s)", killed);
    Ok(())
}

pub fn kill_process(pid: u32, force: bool) -> fbuild_core::Result<()> {
    let pid_str = pid.to_string();
    let argv: Vec<&str> = if cfg!(windows) {
        if force {
            vec!["taskkill", "/F", "/PID", &pid_str]
        } else {
            vec!["taskkill", "/PID", &pid_str]
        }
    } else {
        let signal = if force { "-9" } else { "-TERM" };
        vec!["kill", signal, &pid_str]
    };

    let output = fbuild_core::subprocess::run_command(&argv, None, None, None).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to execute kill command: {}", e))
    })?;

    if !output.success() {
        return Err(fbuild_core::FbuildError::Other(format!(
            "kill failed: {}",
            output.stderr.trim()
        )));
    }
    Ok(())
}

pub fn find_daemon_pids() -> fbuild_core::Result<Vec<u32>> {
    if cfg!(windows) {
        let output = fbuild_core::subprocess::run_command(
            &[
                "tasklist",
                "/FI",
                "IMAGENAME eq fbuild-daemon.exe",
                "/FO",
                "CSV",
                "/NH",
            ],
            None,
            None,
            None,
        )
        .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to run tasklist: {}", e)))?;
        let mut pids = Vec::new();
        for line in output.stdout.lines() {
            // CSV format: "image name","PID","session name","session#","mem usage"
            if line.contains("fbuild-daemon") {
                let fields: Vec<&str> = line.split(',').collect();
                if fields.len() >= 2 {
                    let pid_str = fields[1].trim_matches('"').trim();
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        pids.push(pid);
                    }
                }
            }
        }
        Ok(pids)
    } else {
        let output = fbuild_core::subprocess::run_command(
            &["pgrep", "-f", "fbuild-daemon"],
            None,
            None,
            None,
        )
        .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to run pgrep: {}", e)))?;
        let pids: Vec<u32> = output
            .stdout
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect();
        Ok(pids)
    }
}

pub fn format_uptime(seconds: f64) -> String {
    let secs = seconds as u64;
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}
