//! Linux runtime-library support behind [`crate::platform::runtime`].
//!
//! Linux ELF binaries carry their library dependencies in `DT_NEEDED` and are
//! resolved by the dynamic loader from `LD_LIBRARY_PATH`, the `RPATH`/
//! `RUNPATH` entries, and the loader cache. A vendor binary shipped without an
//! `RPATH` therefore has to be probed on the host that will run it — which is
//! exactly what `probe_binary` does.

use std::path::Path;
use std::time::Duration;

use crate::path::NormalizedPath;
use crate::platform::runtime::BinaryProbe;

/// The variable glibc's loader reads for extra library directories.
pub const LIBRARY_PATH_VAR: Option<&str> = Some("LD_LIBRARY_PATH");

/// Run `binary --version` and classify what the loader said.
pub fn probe_binary(binary: &Path, library_path: Option<&str>) -> BinaryProbe {
    // Short synchronous probe: verify the binary can start before we hand it
    // to a caller that will spawn it for real. Uses
    // run_command_blocking_retrying_exec_busy, which routes through
    // containment (containment group on all platforms) and is ~100 ms.
    let env: Option<Vec<(&str, &str)>> = match (LIBRARY_PATH_VAR, library_path) {
        (Some(var), Some(value)) => Some(vec![(var, value)]),
        _ => None,
    };

    // Retrying variant: fbuild probes binaries it has just downloaded and
    // extracted, and `exec` on a file any process still holds open for
    // writing fails with `ETXTBSY` — including a `fork`ed child of this
    // process that has not reached its own `exec` yet. See
    // FastLED/fbuild#1366.
    let probe_result = crate::subprocess::run_command_blocking_retrying_exec_busy(
        &[&binary.to_string_lossy(), "--version"],
        None, // cwd
        env.as_deref(),
        Some(Duration::from_secs(5)),
    );

    match probe_result {
        Ok(out) if out.success() => BinaryProbe::Started,
        Ok(out) if out.exit_code == 127 => {
            let detail = out
                .stderr
                .lines()
                .find(|l| l.contains("error while loading shared libraries"))
                .map(|l| l.trim().to_string())
                .unwrap_or_else(|| out.stderr.trim().to_string());
            BinaryProbe::MissingSharedLibrary(detail)
        }
        Err(error) => BinaryProbe::ProbeFailed(error.to_string()),
        Ok(_) => BinaryProbe::Inconclusive,
    }
}

/// Linux installs nothing beside a vendor binary: the loader finds its
/// libraries through the search path the caller supplies.
pub fn stage_runtime_libraries(_binary: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Linux needs no extra child directories of its own; a caller's search path
/// is composed from its own bundle.
pub fn runtime_library_dirs(_binary: &Path) -> std::io::Result<Vec<NormalizedPath>> {
    Ok(Vec::new())
}
