mod cli;
mod daemon_client;
mod lib_select;
mod mcp;
mod output;
mod sync;
mod update_check;

fn main() {
    // Trampoline through a larger-stack thread: Windows' default 1 MB main-thread
    // stack is not enough for clap's `--help` formatting across fbuild's full
    // subcommand tree on debug builds (would crash with STATUS_STACK_OVERFLOW
    // before parse_from even returns).
    let handle = std::thread::Builder::new()
        .name("fbuild-main".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(async_main_entry)
        .expect("failed to spawn main thread");
    if handle.join().is_err() {
        std::process::exit(1);
    }
}

fn async_main_entry() {
    // APE (cosmocc) host tools: keep extracted loaders under fbuild's own cache.
    fbuild_core::platform::ape::set_default_cache_root(fbuild_paths::get_cache_root().join("ape"));
    // FastLED/fbuild#1285: derive the dev daemon-identity stamp once, at the
    // top level, and export the value so every child — including the spawned
    // daemon — inherits it instead of re-hashing per invocation. Official
    // (non-dev) invocations export nothing. A hash failure is reported and
    // otherwise ignored: dev builds must keep working.
    match fbuild_paths::dev_daemon_namespace::namespace_to_export() {
        Ok(Some(namespace)) => unsafe {
            // SAFETY: single-threaded startup — before the tokio runtime or
            // any environment reader exists.
            std::env::set_var(
                fbuild_paths::dev_daemon_namespace::ZCCACHE_DAEMON_NAMESPACE_ENV,
                namespace,
            )
        },
        Ok(None) => {}
        Err(error) => {
            eprintln!("warning: failed to derive dev daemon-identity namespace: {error}")
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(cli::async_main());
}
