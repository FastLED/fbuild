//! Espressif QEMU for Xtensa (`qemu-system-xtensa`) used for ESP32-S3 emulation.
//!
//! Resolution order:
//! 1. `FBUILD_QEMU_XTENSA_PATH`
//! 2. `qemu-system-xtensa` already on `PATH`
//! 3. Managed `fbuild` cache install
//! 4. Existing ESP-IDF tools install under `IDF_TOOLS_PATH` / default `.espressif`
//! 5. Managed download into the `fbuild` cache

use std::path::{Path, PathBuf};

use fbuild_core::{FbuildError, Result};

use crate::{CacheSubdir, Package, PackageBase, PackageInfo};

const QEMU_RELEASE_TAG: &str = "esp-develop-9.2.2-20250817";
const QEMU_ARCHIVE_STEM: &str = "qemu-xtensa-softmmu-esp_develop_9.2.2_20250817";

pub struct EspQemuXtensa {
    base: PackageBase,
}

impl EspQemuXtensa {
    pub fn new(project_dir: &Path) -> Result<Self> {
        let pkg = platform_package()?;
        let url = format!(
            "https://github.com/espressif/qemu/releases/download/{}/{}-{}.tar.xz",
            QEMU_RELEASE_TAG, QEMU_ARCHIVE_STEM, pkg.archive_suffix
        );
        Ok(Self {
            base: PackageBase::new(
                "esp-qemu-xtensa",
                QEMU_RELEASE_TAG,
                &url,
                "qemu-xtensa",
                Some(pkg.sha256),
                CacheSubdir::Toolchains,
                project_dir,
            ),
        })
    }

    pub fn resolve_executable(&self) -> Result<PathBuf> {
        if let Ok(raw) = std::env::var("FBUILD_QEMU_XTENSA_PATH") {
            let path = PathBuf::from(raw);
            let path = validate_qemu_path(path, "FBUILD_QEMU_XTENSA_PATH")?;
            hydrate_windows_runtime(&path)?;
            validate_windows_runtime(&path)?;
            return Ok(path);
        }

        if let Some(path) = find_on_path(binary_name()) {
            hydrate_windows_runtime(&path)?;
            validate_windows_runtime(&path)?;
            return Ok(path);
        }

        if self.is_installed() {
            let path = find_qemu_binary(&self.base.install_path())?;
            hydrate_windows_runtime(&path)?;
            validate_windows_runtime(&path)?;
            return Ok(path);
        }

        if let Some(path) = find_existing_idf_qemu() {
            hydrate_windows_runtime(&path)?;
            validate_windows_runtime(&path)?;
            return Ok(path);
        }

        let _ = self.ensure_installed()?;
        let path = find_qemu_binary(&self.base.install_path())?;
        hydrate_windows_runtime(&path)?;
        validate_windows_runtime(&path)?;
        Ok(path)
    }

    fn validate_install(install_dir: &Path) -> Result<()> {
        let _ = find_qemu_binary(install_dir)?;
        Ok(())
    }
}

impl Package for EspQemuXtensa {
    fn ensure_installed(&self) -> Result<PathBuf> {
        if self.is_installed() {
            return qemu_root(&self.base.install_path());
        }

        let install_path =
            crate::block_on_package_future(self.base.staged_install(Self::validate_install))?;

        qemu_root(&install_path)
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        find_qemu_binary(&self.base.install_path()).is_ok()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

struct PlatformPackage {
    archive_suffix: &'static str,
    sha256: &'static str,
}

fn platform_package() -> Result<PlatformPackage> {
    if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
        Ok(PlatformPackage {
            archive_suffix: "x86_64-w64-mingw32",
            sha256: "ef550b912726997f3c1ff4a4fb13c1569e2b692efdc5c9f9c3c926a8f7c540fa",
        })
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        Ok(PlatformPackage {
            archive_suffix: "x86_64-linux-gnu",
            sha256: "588bfaccd0f929650655d10a580f020c6ba9c131712d8fa519280081b8d126eb",
        })
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
        Ok(PlatformPackage {
            archive_suffix: "aarch64-linux-gnu",
            sha256: "317f6e0fd1dba0886d8110709823d909593ef29438822a14f81ebe19d72ce7cd",
        })
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        Ok(PlatformPackage {
            archive_suffix: "x86_64-apple-darwin",
            sha256: "00b9dbc2124cf7633cb86f264fbc524226ad4001bce68bbdba43c9bdc4eb026e",
        })
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        Ok(PlatformPackage {
            archive_suffix: "aarch64-apple-darwin",
            sha256: "aa92e337461d482f5d9f31cd8efc0bd67b3de8fcfcfb567289cb43a59c184651",
        })
    } else {
        Err(FbuildError::PackageError(format!(
            "native QEMU is not supported on {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )))
    }
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "qemu-system-xtensa.exe"
    } else {
        "qemu-system-xtensa"
    }
}

fn validate_qemu_path(path: PathBuf, source: &str) -> Result<PathBuf> {
    if path.is_file() {
        return Ok(path);
    }
    Err(FbuildError::PackageError(format!(
        "{} points to missing QEMU executable: {}",
        source,
        path.display()
    )))
}

fn find_qemu_binary(root: &Path) -> Result<PathBuf> {
    let file_name = binary_name();
    let direct = root.join(file_name);
    if direct.is_file() {
        return Ok(direct);
    }

    let in_bin = root.join("bin").join(file_name);
    if in_bin.is_file() {
        return Ok(in_bin);
    }

    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let nested_direct = path.join(file_name);
            if nested_direct.is_file() {
                return Ok(nested_direct);
            }

            let nested_bin = path.join("bin").join(file_name);
            if nested_bin.is_file() {
                return Ok(nested_bin);
            }
        }
    }

    Err(FbuildError::PackageError(format!(
        "qemu-system-xtensa not found in {}",
        root.display()
    )))
}

fn qemu_root(install_dir: &Path) -> Result<PathBuf> {
    let exe = find_qemu_binary(install_dir)?;
    let in_bin_dir = exe
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|name| name.to_str())
        == Some("bin");
    if in_bin_dir {
        Ok(exe
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or(install_dir)
            .to_path_buf())
    } else {
        Ok(exe.parent().unwrap_or(install_dir).to_path_buf())
    }
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn find_existing_idf_qemu() -> Option<PathBuf> {
    for root in candidate_idf_tools_roots() {
        let qemu_dir = root.join("tools").join("qemu-xtensa");
        if !qemu_dir.is_dir() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&qemu_dir) {
            let mut versions: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
            versions.sort();
            versions.reverse();
            for version_dir in versions {
                if let Ok(path) = find_qemu_binary(&version_dir) {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn candidate_idf_tools_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(path) = std::env::var_os("IDF_TOOLS_PATH") {
        roots.push(PathBuf::from(path));
    }
    if let Some(home) = user_home_dir() {
        roots.push(home.join(".espressif"));
    }
    roots.sort();
    roots.dedup();
    roots
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(windows)]
pub fn build_windows_qemu_path_env(qemu_path: &Path, current_path: &str) -> Result<String> {
    hydrate_windows_runtime(qemu_path)?;
    let mut dirs = windows_runtime_dirs(qemu_path)?;
    for dir in std::env::split_paths(std::ffi::OsStr::new(current_path)) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs)
        .map_err(|e| FbuildError::PackageError(format!("failed to build QEMU PATH: {}", e)))
        .map(|joined| joined.to_string_lossy().into_owned())
}

#[cfg(windows)]
fn validate_windows_runtime(qemu_path: &Path) -> Result<()> {
    let _ = windows_runtime_dirs(qemu_path)?;
    Ok(())
}

#[cfg(windows)]
fn hydrate_windows_runtime(qemu_path: &Path) -> Result<()> {
    let exe_dir = qemu_path.parent().ok_or_else(|| {
        FbuildError::PackageError(format!(
            "qemu executable has no parent directory: {}",
            qemu_path.display()
        ))
    })?;
    let target = exe_dir.join("libiconv-2.dll");
    if target.is_file() {
        return Ok(());
    }

    let Some(source) = find_windows_libiconv_path() else {
        return Ok(());
    };
    if source == target {
        return Ok(());
    }

    std::fs::copy(&source, &target).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to stage libiconv-2.dll next to {} from {}: {}",
            qemu_path.display(),
            source.display(),
            e
        ))
    })?;
    Ok(())
}

#[cfg(not(windows))]
fn validate_windows_runtime(_qemu_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn hydrate_windows_runtime(_qemu_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn windows_runtime_dirs(qemu_path: &Path) -> Result<Vec<PathBuf>> {
    let exe_dir = qemu_path.parent().ok_or_else(|| {
        FbuildError::PackageError(format!(
            "qemu executable has no parent directory: {}",
            qemu_path.display()
        ))
    })?;

    let mut dirs = vec![exe_dir.to_path_buf()];
    if exe_dir.join("libiconv-2.dll").is_file() {
        return Ok(dirs);
    }

    if let Some(iconv) =
        find_windows_libiconv_path().and_then(|path| path.parent().map(Path::to_path_buf))
    {
        dirs.push(iconv);
        return Ok(dirs);
    }

    Err(FbuildError::PackageError(format!(
        "QEMU on Windows requires libiconv-2.dll. Install Git for Windows or add libiconv-2.dll to PATH before running {}",
        qemu_path.display()
    )))
}

#[cfg(windows)]
fn find_windows_libiconv_path() -> Option<PathBuf> {
    if let Some(path) = find_on_path("libiconv-2.dll") {
        return Some(path);
    }

    let mut candidates = Vec::new();
    for var in ["ProgramW6432", "ProgramFiles"] {
        if let Some(root) = std::env::var_os(var) {
            let root = PathBuf::from(root);
            candidates.push(
                root.join("Git")
                    .join("mingw64")
                    .join("bin")
                    .join("libiconv-2.dll"),
            );
            candidates.push(
                root.join("Git")
                    .join("mingw64")
                    .join("libexec")
                    .join("git-core")
                    .join("libiconv-2.dll"),
            );
        }
    }
    candidates.push(PathBuf::from(
        r"C:\Program Files\Git\mingw64\bin\libiconv-2.dll",
    ));
    candidates.push(PathBuf::from(
        r"C:\Program Files\Git\mingw64\libexec\git-core\libiconv-2.dll",
    ));
    candidates.push(PathBuf::from(r"C:\msys64\ucrt64\bin\libiconv-2.dll"));

    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let avr_root = PathBuf::from(local)
            .join("Arduino15")
            .join("packages")
            .join("arduino")
            .join("tools")
            .join("avr-gcc");
        if let Ok(versions) = std::fs::read_dir(avr_root) {
            for version in versions.flatten() {
                let root = version.path();
                candidates.push(root.join("bin").join("libiconv-2.dll"));
                candidates.push(root.join("avr").join("bin").join("libiconv-2.dll"));
            }
        }
    }

    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_qemu_binary_direct_bin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(binary_name());
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(find_qemu_binary(tmp.path()).unwrap(), exe);
    }

    #[test]
    fn find_qemu_binary_nested_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("qemu");
        let bin = nested.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(binary_name());
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(find_qemu_binary(tmp.path()).unwrap(), exe);
        assert_eq!(qemu_root(tmp.path()).unwrap(), nested);
    }

    #[test]
    fn candidate_idf_tools_roots_has_default_home_shape() {
        let roots = candidate_idf_tools_roots();
        if let Some(home) = user_home_dir() {
            assert!(roots.contains(&home.join(".espressif")));
        }
    }

    #[cfg(windows)]
    #[test]
    fn build_windows_qemu_path_env_keeps_exe_dir_first() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe_dir = tmp.path().join("qemu").join("bin");
        let runtime_dir = tmp.path().join("runtime");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let exe = exe_dir.join(binary_name());
        std::fs::write(&exe, b"").unwrap();
        std::fs::write(runtime_dir.join("libiconv-2.dll"), b"").unwrap();
        let old_path = std::env::var_os("PATH");
        std::env::set_var("PATH", &runtime_dir);

        let combined = build_windows_qemu_path_env(&exe, r"C:\Windows\System32").unwrap();
        let parts: Vec<_> = std::env::split_paths(std::ffi::OsStr::new(&combined)).collect();
        assert_eq!(parts[0], exe_dir);
        assert!(exe_dir.join("libiconv-2.dll").is_file());
        assert!(!parts.contains(&runtime_dir));

        if let Some(path) = old_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
    }

    #[cfg(windows)]
    #[test]
    fn hydrate_windows_runtime_copies_iconv_next_to_exe() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe_dir = tmp.path().join("qemu").join("bin");
        let runtime_dir = tmp.path().join("runtime");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let exe = exe_dir.join(binary_name());
        std::fs::write(&exe, b"").unwrap();
        let iconv = runtime_dir.join("libiconv-2.dll");
        std::fs::write(&iconv, b"iconv").unwrap();

        let old_path = std::env::var_os("PATH");
        std::env::set_var("PATH", &runtime_dir);

        hydrate_windows_runtime(&exe).unwrap();
        assert_eq!(
            std::fs::read(exe_dir.join("libiconv-2.dll")).unwrap(),
            b"iconv"
        );

        if let Some(path) = old_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
    }
}
