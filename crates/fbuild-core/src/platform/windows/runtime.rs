//! Windows runtime-library support behind [`crate::platform::runtime`].
//!
//! A Windows PE binary resolves its DLLs from its own directory first, then
//! from `PATH`. Vendor binaries built with MinGW link `libiconv-2.dll`, which
//! the archive does not ship, so fbuild stages a copy next to the executable
//! and reports the directories a child needs on `PATH`.

use std::io;
use std::path::Path;

use crate::path::NormalizedPath;
use crate::platform::runtime::BinaryProbe;

/// The MinGW runtime library the installed vendor binaries link.
const ICONV_DLL: &str = "libiconv-2.dll";

/// Windows resolves DLLs through the child's `PATH`, not a loader variable
/// fbuild composes; `probe_binary` therefore reports a started binary and the
/// caller hydrates `PATH` instead.
pub const LIBRARY_PATH_VAR: Option<&str> = None;

/// A binary whose DLLs the caller hydrates onto `PATH` is reported as started
/// without being run.
pub fn probe_binary(_binary: &Path, _library_path: Option<&str>) -> BinaryProbe {
    BinaryProbe::Started
}

/// Make sure `binary`'s runtime DLL sits beside it.
///
/// A binary that already has a sibling copy is left alone; one that does not
/// gets a copy of the first `libiconv-2.dll` found, so the loader can satisfy
/// it from the executable's own directory. [`io::ErrorKind::NotFound`] means
/// no copy exists anywhere fbuild looks.
pub fn stage_runtime_libraries(binary: &Path) -> io::Result<()> {
    let exe_dir = parent_dir(binary)?;
    let target = exe_dir.join(ICONV_DLL);
    if target.is_file() {
        return Ok(());
    }

    let source = find_libiconv_path().ok_or_else(no_libiconv)?;
    if source.as_path() == target.as_path() {
        return Ok(());
    }

    std::fs::copy(&source, &target).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "failed to stage {ICONV_DLL} next to {} from {}: {error}",
                binary.display(),
                source.display()
            ),
        )
    })?;
    Ok(())
}

/// Directories a child needs on `PATH` to load `binary`'s runtime DLLs.
///
/// The executable's own directory comes first because the loader searches it
/// before `PATH`; when the DLL is not there yet, the directory it came from is
/// appended so a caller can still resolve it.
pub fn runtime_library_dirs(binary: &Path) -> io::Result<Vec<NormalizedPath>> {
    let exe_dir = parent_dir(binary)?;
    let mut dirs = vec![exe_dir];
    if dirs[0].join(ICONV_DLL).is_file() {
        return Ok(dirs);
    }

    let iconv_dir = find_libiconv_path()
        .and_then(|path| path.as_path().parent().map(NormalizedPath::from))
        .ok_or_else(no_libiconv)?;
    dirs.push(iconv_dir);
    Ok(dirs)
}

fn parent_dir(binary: &Path) -> io::Result<NormalizedPath> {
    binary.parent().map(NormalizedPath::from).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("executable has no parent directory: {}", binary.display()),
        )
    })
}

fn no_libiconv() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("no {ICONV_DLL} was found on PATH or in the known install locations"),
    )
}

/// First `libiconv-2.dll` on `PATH` or in the places Windows tools ship one.
fn find_libiconv_path() -> Option<NormalizedPath> {
    if let Some(path) = find_on_path(ICONV_DLL) {
        return Some(path);
    }

    let mut candidates: Vec<NormalizedPath> = Vec::new();
    for var in ["ProgramW6432", "ProgramFiles"] {
        if let Some(root) = std::env::var_os(var) {
            let root = NormalizedPath::from(std::path::Path::new(&root));
            candidates.push(
                root.join("Git")
                    .join("mingw64")
                    .join("bin")
                    .join(ICONV_DLL),
            );
            candidates.push(
                root.join("Git")
                    .join("mingw64")
                    .join("libexec")
                    .join("git-core")
                    .join(ICONV_DLL),
            );
        }
    }
    candidates.push(NormalizedPath::from(
        r"C:\Program Files\Git\mingw64\bin\libiconv-2.dll",
    ));
    candidates.push(NormalizedPath::from(
        r"C:\Program Files\Git\mingw64\libexec\git-core\libiconv-2.dll",
    ));
    candidates.push(NormalizedPath::from(r"C:\msys64\ucrt64\bin\libiconv-2.dll"));

    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let avr_root = NormalizedPath::from(std::path::Path::new(&local))
            .join("Arduino15")
            .join("packages")
            .join("arduino")
            .join("tools")
            .join("avr-gcc");
        if let Ok(versions) = std::fs::read_dir(avr_root.as_path()) {
            for version in versions.flatten() {
                let root = NormalizedPath::from(version.path());
                candidates.push(root.join("bin").join(ICONV_DLL));
                candidates.push(root.join("avr").join("bin").join(ICONV_DLL));
            }
        }
    }

    candidates.into_iter().find(|path| path.is_file())
}

/// An exact file name on `PATH` (never a tool-name expansion).
fn find_on_path(file_name: &str) -> Option<NormalizedPath> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(file_name))
        .find(|candidate| candidate.is_file())
        .map(NormalizedPath::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Staging must place the DLL beside the binary, and the directory list
    /// must lead with the executable's own directory.
    #[test]
    fn staging_copies_a_path_dll_next_to_the_binary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe_dir = tmp.path().join("qemu").join("bin");
        let runtime_dir = tmp.path().join("runtime");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let exe = exe_dir.join("qemu-system-xtensa.exe");
        std::fs::write(&exe, b"").unwrap();
        std::fs::write(runtime_dir.join(ICONV_DLL), b"iconv").unwrap();

        let old_path = std::env::var_os("PATH");
        std::env::set_var("PATH", &runtime_dir);

        stage_runtime_libraries(&exe).unwrap();
        assert_eq!(std::fs::read(exe_dir.join(ICONV_DLL)).unwrap(), b"iconv");

        let dirs = runtime_library_dirs(&exe).unwrap();
        assert_eq!(dirs[0].as_path(), exe_dir.as_path());

        if let Some(path) = old_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
    }
}
