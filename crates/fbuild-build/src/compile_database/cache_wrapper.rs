//! Strip compiler-cache wrappers (sccache/zccache/ccache) from argument lists.

/// Strip cache wrapper (sccache/zccache/ccache) from compiler arguments.
///
/// If the first element of `args` is a known cache wrapper, returns args
/// without it (the real compiler is the second element). Otherwise returns
/// args unchanged.
pub fn strip_cache_wrapper(args: &[String]) -> Vec<String> {
    if args.len() < 2 {
        return args.to_vec();
    }

    // Extract the file stem manually so Windows paths (with `\`) work on Unix.
    // `Path::file_stem` only splits on the platform's native separator, so
    // `C:\...\sccache.exe` is treated as one component on Linux/macOS.
    let filename = args[0].rsplit(['/', '\\']).next().unwrap_or(&args[0]);
    let stem = filename
        .strip_suffix(".exe")
        .or_else(|| filename.strip_suffix(".EXE"))
        .unwrap_or(filename)
        .to_lowercase();

    if stem == "sccache" || stem == "ccache" || stem == "zccache" {
        if stem == "zccache" && args.get(1).is_some_and(|arg| arg == "wrap") {
            if args.len() < 3 {
                return args.to_vec();
            }
            return args[2..].to_vec();
        }
        args[1..].to_vec()
    } else {
        args.to_vec()
    }
}
