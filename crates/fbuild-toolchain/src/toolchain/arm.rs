//! ARM GCC toolchain package.
//!
//! Downloads and manages the ARM GCC 15.2.Rel1 toolchain from developer.arm.com
//! (14.2.Rel1 on Intel macOS, ARM's last darwin-x86_64 release).
//! fbuild's bucket mirrors that host under `vendor/developer.arm.com/<path>` and
//! is tried first, so an ARM outage does not stop a build (FastLED/fbuild#2029);
//! the install still verifies the pinned SHA-256 of whichever copy it uses.
//! Provides paths to: arm-none-eabi-gcc, arm-none-eabi-g++, arm-none-eabi-ar,
//! arm-none-eabi-objcopy, arm-none-eabi-size.

use std::path::{Path, PathBuf};

use fbuild_core::platform::host::{HostArch, HostOs, HostPlatform};

use crate::{CacheSubdir, PackageBase, PackageInfo, Toolchain};

/// ARM GCC toolchain version.
const ARM_GCC_VERSION: &str = "15.2.Rel1";
const ARM_GCC_BASE_URL: &str = "https://developer.arm.com/-/media/Files/downloads/gnu";

/// ARM GCC toolchain manager.
pub struct ArmToolchain {
    base: PackageBase,
    /// Resolved install path (set after ensure_installed)
    install_dir: Option<PathBuf>,
    /// Set when ARM publishes no package for this host and no override pins one.
    unsupported_host: Option<HostPlatform>,
}

impl ArmToolchain {
    pub fn new(project_dir: &Path) -> Self {
        let (url, checksum, version, unsupported_host) = host_source();
        Self {
            base: PackageBase::new(
                "arm-gcc",
                version,
                &url,
                ARM_GCC_BASE_URL,
                checksum,
                CacheSubdir::Toolchains,
                project_dir,
            ),
            install_dir: None,
            unsupported_host,
        }
    }

    /// Use the host-specific compiler declared by a pinned PlatformIO
    /// platform, retaining the ordinary ARM binary layout validation.
    pub fn with_override(project_dir: &Path, ovr: fbuild_config::PackageOverride) -> Self {
        let mut toolchain = Self::new(project_dir);
        toolchain.base = toolchain.base.with_override(ovr);
        toolchain.unsupported_host = None;
        toolchain
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        let (url, checksum, version, unsupported_host) = host_source();
        Self {
            base: PackageBase::with_cache_root(
                "arm-gcc",
                version,
                &url,
                ARM_GCC_BASE_URL,
                checksum,
                CacheSubdir::Toolchains,
                project_dir,
                cache_root,
            ),
            install_dir: None,
            unsupported_host,
        }
    }

    /// Get the resolved install directory, or compute it.
    fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_bin_root(&self.base.install_path()))
    }

    /// Validate that the toolchain installation has all required files.
    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let root = find_bin_root(install_dir);
        let bin_dir = root.join("bin");

        if !bin_dir.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "arm-gcc bin directory not found at {}",
                bin_dir.display()
            )));
        }

        let required_tools = [
            "arm-none-eabi-gcc",
            "arm-none-eabi-g++",
            "arm-none-eabi-ar",
            "arm-none-eabi-objcopy",
            "arm-none-eabi-size",
        ];
        for tool in &required_tools {
            let tool_path = tool_binary(&bin_dir, tool);
            if !tool_path.exists() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "required tool {} not found at {}",
                    tool,
                    tool_path.display()
                )));
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::Package for ArmToolchain {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }
        if let Some(host) = self.unsupported_host {
            return Err(unsupported_host(host));
        }

        let install_path = self.base.staged_install(Self::validate).await?;
        Ok(find_bin_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_bin_root(&self.base.install_path());
        tool_binary(&root.join("bin"), "arm-none-eabi-gcc").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Toolchain for ArmToolchain {
    fn get_gcc_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-gcc")
    }

    fn get_gxx_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-g++")
    }

    fn get_ar_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-ar")
    }

    fn get_objcopy_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-objcopy")
    }

    fn get_size_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-size")
    }

    fn get_bin_dir(&self) -> PathBuf {
        self.resolved_dir().join("bin")
    }
}

/// A platform-specific ARM toolchain package entry.
struct ArmPlatformPackage {
    /// ARM release, spelled like [`ARM_GCC_VERSION`]; lowercased it is the
    /// download directory and the filename infix.
    version: &'static str,
    filename: &'static str,
    /// SHA-256 checksum, as ARM publishes it in `<filename>.sha256asc`.
    checksum: &'static str,
}

/// ARM's last darwin-x86_64 release: 15.2.rel1 ships no Intel macOS build
/// (FastLED/fbuild#2029).
const ARM_GCC_DARWIN_X86_64_VERSION: &str = "14.2.Rel1";

/// All platform variants for the ARM toolchain — no `cfg!` so tests can
/// validate every entry regardless of which platform runs the test.
fn all_platform_packages() -> [(&'static str, ArmPlatformPackage); 5] {
    [
        (
            "windows",
            ArmPlatformPackage {
                version: ARM_GCC_VERSION,
                filename: "arm-gnu-toolchain-15.2.rel1-mingw-w64-x86_64-arm-none-eabi.zip",
                checksum: "7936cac895611023ffb22a64b8e426098c7104cb689778c1894572ca840b9ece",
            },
        ),
        (
            "macos-arm64",
            ArmPlatformPackage {
                version: ARM_GCC_VERSION,
                filename: "arm-gnu-toolchain-15.2.rel1-darwin-arm64-arm-none-eabi.tar.xz",
                checksum: "1938a84b7105c192e3fb4fa5e893ba25f425f7ddab40515ae608cd40f68669a8",
            },
        ),
        (
            "macos-x86_64",
            ArmPlatformPackage {
                version: ARM_GCC_DARWIN_X86_64_VERSION,
                filename: "arm-gnu-toolchain-14.2.rel1-darwin-x86_64-arm-none-eabi.tar.xz",
                checksum: "2d9e717dd4f7751d18936ae1365d25916534105ebcb7583039eff1092b824505",
            },
        ),
        (
            "linux-aarch64",
            ArmPlatformPackage {
                version: ARM_GCC_VERSION,
                filename: "arm-gnu-toolchain-15.2.rel1-aarch64-arm-none-eabi.tar.xz",
                checksum: "d061559d814b205ed30c5b7c577c03317ec447ca51cd5a159d26b12a5bbeb20c",
            },
        ),
        (
            "linux-x86_64",
            ArmPlatformPackage {
                version: ARM_GCC_VERSION,
                filename: "arm-gnu-toolchain-15.2.rel1-x86_64-arm-none-eabi.tar.xz",
                checksum: "597893282ac8c6ab1a4073977f2362990184599643b4c5ee34870a8215783a16",
            },
        ),
    ]
}

/// Key into [`all_platform_packages`] for `host`, or `None` where ARM
/// publishes no build (a macOS arch other than arm64 and x86_64).
fn platform_key(host: HostPlatform) -> Option<&'static str> {
    match (host.os(), host.arch()) {
        (HostOs::Windows, _) => Some("windows"),
        (HostOs::Macos, HostArch::Aarch64) => Some("macos-arm64"),
        (HostOs::Macos, HostArch::X86_64) => Some("macos-x86_64"),
        (HostOs::Macos, _) => None,
        (HostOs::Linux, HostArch::Aarch64) => Some("linux-aarch64"),
        (HostOs::Linux, _) => Some("linux-x86_64"),
    }
}

/// Where the toolchain for one host comes from.
#[derive(Debug)]
struct ArmSource {
    url: String,
    checksum: &'static str,
    /// Package version, which also keys the cache directory.
    version: &'static str,
}

/// Download URL, pinned SHA-256 and version for `host`, or `None` (see
/// [`platform_key`]).
fn platform_package(host: HostPlatform) -> Option<ArmSource> {
    let key = platform_key(host)?;
    all_platform_packages()
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, pkg)| ArmSource {
            url: format!(
                "{}/{}/binrel/{}",
                ARM_GCC_BASE_URL,
                pkg.version.to_lowercase(),
                pkg.filename
            ),
            checksum: pkg.checksum,
            version: pkg.version,
        })
}

/// URL, SHA-256, version and unsupported host for the machine running
/// fbuild. A host without a package gets the vendor directory as a URL that
/// is never fetched: `ensure_installed` rejects it first unless an override
/// pins a compiler.
fn host_source() -> (String, Option<&'static str>, &'static str, Option<HostPlatform>) {
    let host = fbuild_core::platform::host::current();
    match platform_package(host) {
        Some(src) => (src.url, Some(src.checksum), src.version, None),
        None => (
            ARM_GCC_BASE_URL.to_string(),
            None,
            ARM_GCC_VERSION,
            Some(host),
        ),
    }
}

fn unsupported_host(host: HostPlatform) -> fbuild_core::FbuildError {
    let pinned: Vec<&str> = all_platform_packages().iter().map(|(k, _)| *k).collect();
    fbuild_core::FbuildError::PackageError(format!(
        "ARM publishes no GCC toolchain for {} {} (pinned: {}); pin a host compiler through the PlatformIO platform's packages instead",
        host.os_name(),
        host.arch_name(),
        pinned.join(", "),
    ))
}

/// Find the actual root directory containing bin/ inside an extracted archive.
///
/// Archives often have a single top-level directory (e.g. `arm-gnu-toolchain-15.2.rel1-.../`).
fn find_bin_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("bin").exists() {
        return install_dir.to_path_buf();
    }

    // Check one level deep for a nested directory with bin/
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("bin").exists() {
                return path;
            }
        }
    }

    install_dir.to_path_buf()
}

/// Get the full path to a tool binary (native spelling, or a `.com`/`.exe` APE).
fn tool_binary(bin_dir: &Path, name: &str) -> PathBuf {
    fbuild_core::platform::executable::resolve_tool_in(bin_dir, name).into_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;
    use std::collections::HashMap;

    fn host(os: HostOs, arch: HostArch) -> HostPlatform {
        HostPlatform::new(os, arch)
    }

    #[test]
    fn test_platform_package_returns_url() {
        let src = platform_package(host(HostOs::Linux, HostArch::X86_64)).unwrap();
        assert_eq!(
            src.url,
            "https://developer.arm.com/-/media/Files/downloads/gnu/15.2.rel1/binrel/arm-gnu-toolchain-15.2.rel1-x86_64-arm-none-eabi.tar.xz"
        );
        assert_eq!(
            src.checksum,
            "597893282ac8c6ab1a4073977f2362990184599643b4c5ee34870a8215783a16"
        );
        assert_eq!(src.version, "15.2.Rel1");
    }

    /// Intel macOS pins ARM's last darwin-x86_64 release, 14.2.rel1, by the
    /// SHA-256 ARM publishes in its `.sha256asc` (FastLED/fbuild#2029). It is
    /// cached under its own version, so it never shares a directory with 15.2.
    #[test]
    fn test_intel_macos_pins_14_2_rel1() {
        let src = platform_package(host(HostOs::Macos, HostArch::X86_64)).unwrap();
        assert_eq!(
            src.url,
            "https://developer.arm.com/-/media/Files/downloads/gnu/14.2.rel1/binrel/arm-gnu-toolchain-14.2.rel1-darwin-x86_64-arm-none-eabi.tar.xz"
        );
        assert_eq!(
            src.checksum,
            "2d9e717dd4f7751d18936ae1365d25916534105ebcb7583039eff1092b824505"
        );
        assert_eq!(src.version, "14.2.Rel1");
    }

    /// Apple Silicon gets ARM's native darwin-arm64 build; Intel macOS gets
    /// 14.2.rel1, ARM's last darwin-x86_64 release (FastLED/fbuild#2029).
    #[test]
    fn test_platform_package_selects_host_build() {
        let cases = [
            (HostOs::Windows, HostArch::X86_64, Some("mingw-w64-x86_64")),
            (HostOs::Windows, HostArch::Aarch64, Some("mingw-w64-x86_64")),
            (HostOs::Macos, HostArch::Aarch64, Some("darwin-arm64")),
            (HostOs::Macos, HostArch::X86_64, Some("darwin-x86_64")),
            (HostOs::Macos, HostArch::Other, None),
            (HostOs::Linux, HostArch::Aarch64, Some("15.2.rel1-aarch64")),
            (HostOs::Linux, HostArch::X86_64, Some("15.2.rel1-x86_64")),
        ];
        for (os, arch, want) in cases {
            let got = platform_package(host(os, arch)).map(|src| src.url);
            match want {
                Some(part) => assert!(
                    got.as_deref().is_some_and(|url| url.contains(part)),
                    "{os:?} {arch:?}: {got:?} lacks {part}"
                ),
                None => assert_eq!(got, None, "{os:?} {arch:?}"),
            }
        }
    }

    /// Every key `platform_key` can return has a package entry.
    #[test]
    fn test_every_platform_key_has_a_package() {
        let keys: Vec<_> = all_platform_packages().iter().map(|(k, _)| *k).collect();
        for os in [HostOs::Windows, HostOs::Macos, HostOs::Linux] {
            for arch in [
                HostArch::X86,
                HostArch::X86_64,
                HostArch::Arm,
                HostArch::Aarch64,
                HostArch::Other,
            ] {
                if let Some(key) = platform_key(host(os, arch)) {
                    assert!(keys.contains(&key), "{os:?} {arch:?} -> {key} has no entry");
                }
            }
        }
    }

    /// A host without a package fails with a typed error naming the host
    /// instead of fetching a URL that does not exist.
    #[tokio::test]
    async fn test_unsupported_host_fails_fast() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut tc = ArmToolchain::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        let other = host(HostOs::Macos, HostArch::Other);
        tc.unsupported_host = Some(other);
        let err = tc.ensure_installed().await.unwrap_err().to_string();
        assert!(err.contains(&format!("macos {}", other.arch_name())), "{err}");
        assert!(err.contains("macos-x86_64"), "{err}");
    }

    #[test]
    fn test_find_bin_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_bin_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("arm-gnu-toolchain-15.2.rel1");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), nested);
    }

    #[test]
    fn test_arm_toolchain_get_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = ArmToolchain::new(tmp.path());
        let tools: HashMap<String, PathBuf> = tc.get_all_tools();
        assert!(tools.contains_key("gcc"));
        assert!(tools.contains_key("g++"));
        assert!(tools.contains_key("ar"));
        assert!(tools.contains_key("objcopy"));
        assert!(tools.contains_key("size"));
    }

    #[test]
    fn test_arm_toolchain_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = ArmToolchain::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!tc.is_installed());
    }

    /// A package shipping each host tool as one APE `bin/<tool>.com` validates,
    /// reports installed, and resolves every tool to the `.com` file.
    #[test]
    fn test_arm_toolchain_finds_ape_com_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = ArmToolchain::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        let install = tc.base.install_path();
        let bin = install.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tools = ["gcc", "g++", "ar", "objcopy", "size"];
        assert!(ArmToolchain::validate(&install).is_err());
        for tool in tools {
            std::fs::write(bin.join(format!("arm-none-eabi-{tool}.com")), b"MZqFpD='\n").unwrap();
        }
        ArmToolchain::validate(&install).expect("APE .com tools satisfy validation");
        std::fs::write(
            crate::disk_cache::paths::install_complete_sentinel(&install),
            "",
        )
        .unwrap();
        assert!(tc.is_installed());
        assert_eq!(tc.get_gcc_path(), bin.join("arm-none-eabi-gcc.com"));
        assert_eq!(tc.get_size_path(), bin.join("arm-none-eabi-size.com"));
    }

    /// Every host entry pins a valid SHA-256, so neither the bucket copy nor
    /// the vendor copy is ever installed unverified (FastLED/fbuild#2029).
    /// Catches placeholder hashes like `a000...0002` on all platforms.
    #[test]
    fn test_every_platform_entry_has_a_valid_checksum() {
        for (platform, pkg) in &all_platform_packages() {
            let hash = pkg.checksum;
            assert_eq!(
                hash.len(),
                64,
                "checksum for {platform} has wrong length ({}): {hash}",
                hash.len(),
            );
            assert!(
                hash.chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "checksum for {platform} is not lowercase hex: {hash}",
            );
            // Reject obviously-fake placeholder patterns
            assert!(
                !hash.starts_with("a000000"),
                "checksum for {platform} looks like a placeholder: {hash}",
            );
        }
    }

    /// Every platform entry must have a valid URL.
    #[test]
    fn test_all_platform_urls_are_valid() {
        for (platform, pkg) in &all_platform_packages() {
            assert!(
                pkg.filename
                    .contains(&format!("-{}-", pkg.version.to_lowercase())),
                "filename for {platform} doesn't contain its version {}: {}",
                pkg.version,
                pkg.filename,
            );
        }
    }
}
