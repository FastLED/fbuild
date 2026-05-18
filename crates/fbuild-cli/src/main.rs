mod cli;
mod daemon_client;
mod lib_select;
mod mcp;

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
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(cli::async_main());
}
