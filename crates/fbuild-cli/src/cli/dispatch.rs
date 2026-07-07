//! Top-level async dispatcher: parse argv, set up tracing + Ctrl+C, then
//! fan out to per-subcommand handlers in the topic modules.

use clap::Parser;

use crate::{daemon_client, lib_select, mcp, output, update_check};

use super::args::{resolve_project_dir, rewrite_args, BloatCmd, Cli, Commands};
use super::bloat_lookup::run_bloat_lookup;
use super::bringup::run_bringup;
use super::build::run_build;
use super::clang_tools::{run_clang_tool, run_iwyu};
use super::clangd_config::run_clangd_config;
use super::compile_many::{
    build_ci_pio_env, normalize_ci_sketches, run_compile_many, CompileManyArgs,
};
use super::daemon_cmd::run_daemon;
use super::deploy::{run_deploy, run_monitor, run_test_emu};
use super::device::run_device;
use super::graph_cmd::run_bloat_graph;
use super::lnk::run_lnk;
use super::monitor_parse::parse_monitor_flags;
use super::pio::{pio_build, pio_deploy, pio_monitor};
use super::port_scan::run_port;
use super::purge::{run_purge, run_purge_gc};
use super::reset::run_reset;
use super::serial_probe::run_serial;
use super::show::run_show;
use super::symbols_cmd::run_symbols;
use super::sync_cmd::run_sync_cmd;

pub async fn async_main() {
    let cli = Cli::parse_from(rewrite_args());

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Scan caller's environment for `PLATFORMIO_*` vars and warn about any
    // that fbuild does not act on, so users aren't bitten by silent
    // mis-builds. The captured map is forwarded to the daemon per request.
    let pio_env = daemon_client::capture_pio_env();
    for var in fbuild_config::scan_unsupported(&pio_env) {
        tracing::warn!("{} is set but not supported by fbuild (ignored)", var);
    }
    for var in fbuild_config::scan_warn_only(&pio_env) {
        tracing::warn!("{} is set but fbuild does not act on it", var);
    }

    // Handle Ctrl+C with exit code 130 (standard POSIX SIGINT behavior, matches Python)
    ctrlc::set_handler(move || {
        output::warn("Interrupted");
        std::process::exit(130);
    })
    .ok();

    // Notify when running in dev mode (matches Python behavior)
    if std::env::var("FBUILD_DEV_MODE").is_ok_and(|v| v == "1") {
        output::progress("FBUILD_DEV_MODE=1 (dev mode: port 8865, ~/.fbuild/dev/)");
    }

    // FastLED/fbuild#626 Phase 1: passive update check. Kick it off in
    // the background so the network round-trip (~200 ms hot cache, up to
    // 3 s cold) doesn't gate the command that follows. The task detaches;
    // when it completes it prints a single stderr line if a newer stable
    // release is available. Network failures are swallowed. Fully
    // suppressed by `--no-update-check`, `FBUILD_NO_UPDATE_CHECK=1`, or
    // any CI marker (`CI=true` etc.).
    let update_check_opts = update_check::CheckOptions {
        no_update_check: cli.no_update_check,
    };
    tokio::spawn(async move {
        update_check::run_passive_check(env!("CARGO_PKG_VERSION"), &update_check_opts).await;
    });

    // Extract top-level project_dir before matching (since match partially moves cli)
    let top_level_project_dir = cli.project_dir.clone();

    let result = match cli.command {
        Some(Commands::Symbols {
            input,
            map,
            nm,
            cppfilt,
            build_info,
            json,
            output_dir,
            top,
            no_graph,
            graph_top,
            graph_min_bytes,
            graph_depth,
            graph_fan_out,
            graph_collapse_archive,
            graph_exclude_archive,
        }) => {
            run_symbols(
                input,
                map,
                nm,
                cppfilt,
                build_info,
                json,
                output_dir,
                top,
                no_graph,
                graph_top,
                graph_min_bytes,
                graph_depth,
                graph_fan_out,
                graph_collapse_archive,
                graph_exclude_archive,
            )
            .await
        }
        Some(Commands::Bloat { cmd }) => match cmd {
            BloatCmd::Graph {
                input,
                symbol,
                map,
                nm,
                cppfilt,
                build_info,
                output,
                depth,
                fan_out,
                max_depth,
                collapse_archive,
                exclude_archive,
            } => {
                run_bloat_graph(
                    input,
                    symbol,
                    map,
                    nm,
                    cppfilt,
                    build_info,
                    output,
                    depth,
                    fan_out,
                    max_depth,
                    collapse_archive,
                    exclude_archive,
                )
                .await
            }
            BloatCmd::Lookup {
                input,
                symbol,
                symbol_mangled,
                json,
                map,
                nm,
                cppfilt,
                build_info,
            } => {
                run_bloat_lookup(
                    input,
                    symbol,
                    symbol_mangled,
                    json,
                    map,
                    nm,
                    cppfilt,
                    build_info,
                )
                .await
            }
        },
        Some(Commands::Build {
            project_dir,
            environment,
            clean,
            verbose,
            jobs,
            quick,
            release,
            platformio,
            dry_run,
            target,
            symbol_analysis,
            no_timestamp,
            output_dir,
            shrink: _,
            no_shrink: _,
            bloat_analysis,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            if platformio {
                pio_build(&project_dir, environment.as_deref(), clean, verbose).await
            } else {
                run_build(
                    project_dir,
                    environment,
                    clean,
                    verbose,
                    jobs,
                    quick,
                    release,
                    dry_run,
                    target,
                    symbol_analysis,
                    no_timestamp,
                    output_dir,
                    bloat_analysis,
                )
                .await
            }
        }
        Some(Commands::Deploy {
            project_dir,
            environment,
            port,
            clean,
            monitor,
            verbose,
            platformio,
            timeout,
            halt_on_error,
            halt_on_success,
            expect,
            no_timestamp,
            skip_build,
            qemu,
            qemu_timeout,
            baud_rate,
            no_probe_rs,
            to,
            emulator,
            target,
            output_dir,
            shrink: _,
            no_shrink: _,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            if platformio {
                pio_deploy(
                    &project_dir,
                    environment.as_deref(),
                    port.as_deref(),
                    clean,
                    verbose,
                )
                .await
            } else {
                let monitor_after = monitor.is_some();
                let parsed = monitor
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(parse_monitor_flags)
                    .unwrap_or_default();
                run_deploy(
                    project_dir,
                    environment,
                    port,
                    clean,
                    monitor_after,
                    verbose,
                    timeout.or(parsed.timeout),
                    halt_on_error.or(parsed.halt_on_error),
                    halt_on_success.or(parsed.halt_on_success),
                    expect.or(parsed.expect),
                    no_timestamp,
                    skip_build,
                    qemu,
                    qemu_timeout,
                    baud_rate,
                    no_probe_rs,
                    to,
                    emulator,
                    target,
                    output_dir,
                )
                .await
            }
        }
        Some(Commands::Monitor {
            project_dir,
            environment,
            port,
            baud_rate,
            verbose: _,
            platformio,
            timeout,
            halt_on_error,
            halt_on_success,
            expect,
            no_timestamp,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            if platformio {
                pio_monitor(
                    &project_dir,
                    environment.as_deref(),
                    port.as_deref(),
                    baud_rate,
                )
                .await
            } else {
                run_monitor(
                    project_dir,
                    environment,
                    port,
                    baud_rate,
                    timeout,
                    halt_on_error,
                    halt_on_success,
                    expect,
                    no_timestamp,
                )
                .await
            }
        }
        Some(Commands::Reset {
            project_dir,
            environment,
            port,
            verbose,
        }) => run_reset(project_dir, environment, port, verbose),
        Some(Commands::Purge {
            target,
            dry_run,
            project_dir,
            gc,
        }) => {
            if gc {
                run_purge_gc().await
            } else {
                run_purge(target, dry_run, project_dir)
            }
        }
        Some(Commands::Sync {
            project_dir,
            environment,
            yes,
            locked,
            check,
            dry_run,
            upgrade,
            upgrade_package,
        }) => {
            let code = run_sync_cmd(
                Some(fbuild_core::path::NormalizedPath::new(project_dir)),
                environment,
                yes,
                locked,
                check,
                dry_run,
                upgrade,
                upgrade_package,
            )
            .await;
            if code == 0 {
                Ok(())
            } else {
                std::process::exit(code);
            }
        }
        Some(Commands::Daemon { action }) => run_daemon(action).await,
        Some(Commands::Show {
            target,
            no_follow,
            lines,
        }) => run_show(&target, !no_follow, lines),
        Some(Commands::Device { action }) => run_device(action).await,
        Some(Commands::Mcp) => {
            let code = mcp::run_mcp_server().await;
            if code == 0 {
                Ok(())
            } else {
                Err(fbuild_core::FbuildError::BuildFailed(
                    "MCP server exited with error".to_string(),
                ))
            }
        }
        Some(Commands::ClangTidy {
            project_dir,
            environment,
            verbose,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            run_clang_tool(
                fbuild_packages::toolchain::ClangComponentKind::ClangExtra,
                "clang-tidy",
                project_dir,
                environment,
                verbose,
                &[],
            )
            .await
        }
        Some(Commands::Iwyu {
            project_dir,
            environment,
            verbose,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            run_iwyu(project_dir, environment, verbose).await
        }
        Some(Commands::ClangdConfig {
            project_dir,
            environment,
            verbose,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            run_clangd_config(project_dir, environment, verbose).await
        }
        Some(Commands::TestEmu {
            project_dir,
            environment,
            verbose,
            shrink: _,
            no_shrink: _,
            timeout,
            halt_on_error,
            halt_on_success,
            expect,
            no_timestamp,
            emulator,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            run_test_emu(
                project_dir,
                environment,
                verbose,
                timeout,
                halt_on_error,
                halt_on_success,
                expect,
                no_timestamp,
                emulator,
            )
            .await
        }
        Some(Commands::ClangQuery {
            project_dir,
            environment,
            verbose,
            matcher,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            let extra: Vec<String> = matcher
                .map(|m| vec!["-c".to_string(), m])
                .unwrap_or_default();
            let extra_refs: Vec<&str> = extra.iter().map(|s| s.as_str()).collect();
            run_clang_tool(
                fbuild_packages::toolchain::ClangComponentKind::ClangExtra,
                "clang-query",
                project_dir,
                environment,
                verbose,
                &extra_refs,
            )
            .await
        }
        Some(Commands::Lnk { action }) => run_lnk(action, &top_level_project_dir).await,
        Some(Commands::LibSelect {
            project_dir,
            environment,
            explain,
            json,
        }) => {
            let project_dir = resolve_project_dir(project_dir, &top_level_project_dir);
            let exit = lib_select::run(
                std::path::Path::new(&project_dir),
                environment.as_deref(),
                explain,
                json,
            );
            std::process::exit(exit);
        }
        Some(Commands::CompileMany {
            board,
            framework_jobs,
            sketch_jobs,
            quick,
            release,
            verbose,
            diag_stage2,
            sketches,
        }) => {
            run_compile_many(CompileManyArgs {
                board,
                framework_jobs,
                sketch_jobs,
                quick,
                release,
                verbose,
                diag_stage2,
                sketches,
                pio_env: std::collections::HashMap::new(),
            })
            .await
        }
        Some(Commands::Ci {
            board,
            libs,
            project_conf,
            shrink: _,
            no_shrink: _,
            keep_build_dir: _keep_build_dir,
            build_dir,
            framework_jobs,
            sketch_jobs,
            quick,
            release,
            verbose,
            diag_stage2,
            sketches,
        }) => {
            if let Some(bd) = &build_dir {
                output::warn(format!(
                    "--build-dir {} is accepted for pio ci compatibility but not yet honored; outputs go to .fbuild/build/...",
                    bd
                ));
            }
            let normalized = normalize_ci_sketches(&sketches);
            let pio_env = build_ci_pio_env(&libs, project_conf.as_deref()).await;
            run_compile_many(CompileManyArgs {
                board,
                framework_jobs,
                sketch_jobs,
                quick,
                release,
                verbose,
                diag_stage2,
                sketches: normalized,
                pio_env,
            })
            .await
        }
        None => {
            // Default action: deploy with monitor (like Python fbuild)
            let project_dir = cli.project_dir.unwrap_or_else(|| ".".to_string());
            if cli.platformio {
                pio_deploy(
                    &project_dir,
                    cli.environment.as_deref(),
                    cli.port.as_deref(),
                    cli.clean,
                    cli.verbose,
                )
                .await
            } else {
                let monitor_after = true;
                let parsed = cli
                    .monitor
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(parse_monitor_flags)
                    .unwrap_or_default();
                run_deploy(
                    project_dir,
                    cli.environment,
                    cli.port,
                    cli.clean,
                    monitor_after,
                    cli.verbose,
                    cli.timeout.or(parsed.timeout),
                    cli.halt_on_error.or(parsed.halt_on_error),
                    cli.halt_on_success.or(parsed.halt_on_success),
                    cli.expect.or(parsed.expect),
                    false,
                    false,
                    false,
                    30,
                    None,
                    false,
                    None,
                    None,
                    None,
                    None,
                )
                .await
            }
        }
        Some(Commands::Serial { action }) => run_serial(action),
        Some(Commands::Bringup(args)) => run_bringup(args),
        Some(Commands::Port { action }) => run_port(action),
    };

    if let Err(e) = result {
        output::error(format!("{}", e));
        std::process::exit(1);
    }
}
