//! macOS runtime-library support behind [`crate::platform::runtime`].
//!
//! macOS resolves library dependencies through `LC_LOAD_DYLIB` plus
//! `@rpath`/`DYLD_*` variables, and the vendor binaries fbuild installs are
//! self-contained: nothing has to be staged beside them and there is no
//! search path to hand a child. Probing therefore has nothing to learn and
//! reports a started binary.

use std::io;
use std::path::Path;

use crate::path::NormalizedPath;
use crate::platform::runtime::BinaryProbe;

/// macOS exposes no variable fbuild composes for vendor binaries.
pub const LIBRARY_PATH_VAR: Option<&str> = None;

/// A self-contained binary is reported as started without being run.
pub fn probe_binary(_binary: &Path, _library_path: Option<&str>) -> BinaryProbe {
    BinaryProbe::Started
}

/// Nothing is staged beside a self-contained binary.
pub fn stage_runtime_libraries(_binary: &Path) -> io::Result<()> {
    Ok(())
}

/// No extra child directories are needed.
pub fn runtime_library_dirs(_binary: &Path) -> io::Result<Vec<NormalizedPath>> {
    Ok(Vec::new())
}
