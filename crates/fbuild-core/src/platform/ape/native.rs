//! Host-native stand-ins for APE images, for spawners fbuild does not control.
//!
//! zccache runs the compiler itself (through `kernal-api`), so it can't be
//! routed through `<loader> <image>`. For those callers fbuild hands over a
//! `#!/bin/sh` shim the kernel executes directly. It keeps the image's file
//! name (zccache detects the compiler family from it) and `exec`s the loader
//! on the *original* image path, so tools that locate their own subprograms
//! relative to themselves (gcc → `../libexec/.../cc1`) still find them. The
//! shim also prepends fbuild's `ape` directory to `PATH`, so APE programs the
//! tool spawns in turn (gcc → `cc1`, `as`) resolve a loader too.
//!
//! Relocating an assimilated (native-header) copy instead was tried and
//! rejected: gcc then fails with "cannot execute 'cc1'".
//!
//! Shims live in content-addressed subdirectories of the APE cache
//! (`shim-<hash>/<name>`).

use std::path::Path;

use crate::path::NormalizedPath;

use super::super::host::{self, HostPlatform};
use super::{
    CACHE_DIR_ENV, LOADER_ENV, Memo, image_key, install, is_ape_file, plan_launch_for, short_hash,
};

static NATIVE_MEMO: Memo = Memo::new();

/// A path the kernel can execute directly that behaves like `program`, when
/// `program` is an APE image on a host that can't exec it natively. `None`
/// for non-APE programs, on Windows (APE runs natively), or when nothing
/// could be materialized — callers then use `program` unchanged.
pub fn native_executable(program: &Path) -> Option<NormalizedPath> {
    let host = host::current();
    if host.is_windows() || !is_ape_file(program) {
        return None;
    }
    let env = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty());
    let dirs = super::cache_dirs(env(CACHE_DIR_ENV));
    native_executable_for(
        host,
        program,
        &dirs,
        env(LOADER_ENV).as_deref(),
        env("PATH").as_deref(),
    )
}

/// [`native_executable`] with every host input explicit, for tests.
pub fn native_executable_for(
    host: HostPlatform,
    program: &Path,
    dirs: &[NormalizedPath],
    loader_override: Option<&std::ffi::OsStr>,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<NormalizedPath> {
    if host.is_windows() || !is_ape_file(program) {
        return None;
    }
    let image = std::path::absolute(program).ok()?;
    let key = image_key(&image, host, dirs)?;
    if let Some(hit) = NATIVE_MEMO.get(&key) {
        return Some(hit);
    }
    let name = image.file_name()?.to_str()?.to_owned();
    let launch = plan_launch_for(
        host,
        image.as_os_str(),
        None,
        path_var,
        loader_override,
        dirs,
    )?;
    // A memfd path is only valid in this process's direct children; the
    // shim's exec runs one level further down.
    if super::super::selected::ape::is_anonymous(&launch.loader) {
        return None;
    }
    let mut shim = String::from("#!/bin/sh\n");
    // The image's identity goes into the shim bytes (and so its cache path):
    // zccache fingerprints a compiler by its path, mtime and size, so a shim
    // that stayed byte-identical across an in-place compiler upgrade would let
    // zccache reuse the old compiler's cache entries.
    let mtime = key
        .2
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    shim.push_str(&format!(
        "# fbuild-ape-image: len={} mtime={mtime}\n",
        key.1
    ));
    if let Some(dir) = &launch.ape_path_dir {
        shim.push_str(&format!(
            "PATH={}\"${{PATH:+:$PATH}}\"; export PATH\n",
            sh_quote(dir)?
        ));
    }
    shim.push_str(&format!(
        "exec {} {} \"$@\"\n",
        sh_quote(&launch.loader)?,
        sh_quote(&launch.image)?
    ));
    let sub = format!("shim-{}", short_hash(shim.as_bytes()));
    let native = install::install(dirs, Some(&sub), &name, shim.as_bytes())?;
    NATIVE_MEMO.put(key, native.clone());
    Some(native)
}

/// Single-quote `path` for `/bin/sh`.
fn sh_quote(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    Some(format!("'{}'", s.replace('\'', r"'\''")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::host::{HostArch, HostOs};

    const LINUX_X64: HostPlatform = HostPlatform::new(HostOs::Linux, HostArch::X86_64);
    const WINDOWS: HostPlatform = HostPlatform::new(HostOs::Windows, HostArch::X86_64);

    fn hello() -> NormalizedPath {
        NormalizedPath::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("data/ape-hello/hello.com"))
    }

    #[test]
    fn shim_keeps_name_runs_original_image_and_exposes_ape_on_path() {
        if host::current().is_windows() {
            return;
        }
        let cache = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("it's x86_64-linux-cosmo-gcc");
        std::fs::copy(hello(), &image).unwrap();
        let shim = native_executable_for(
            host::current(),
            &image,
            &[NormalizedPath::new(cache.path())],
            None,
            None,
        )
        .expect("shim");
        assert_eq!(shim.file_name(), image.file_name());
        assert!(
            shim.relative_to(&NormalizedPath::from(cache.path()))
                .is_some()
        );
        assert!(!is_ape_file(&shim), "the shim itself is a plain script");
        let text = std::fs::read_to_string(&shim).unwrap();
        assert!(
            text.starts_with("#!/bin/sh\n# fbuild-ape-image: len="),
            "{text}"
        );
        assert!(text.contains("\nPATH="), "{text}");
        // Spawned directly — no fbuild loader routing involved.
        let out = crate::subprocess::run_command_blocking_retrying_exec_busy(
            &[shim.to_str().unwrap(), "via", "shim"],
            None,
            Some(&[("PATH", "/nonexistent")]),
            None,
        )
        .unwrap();
        assert_eq!(
            out.stdout, "hello world via shim\n",
            "stderr: {}",
            out.stderr
        );
        // Memoized and stable.
        assert_eq!(
            native_executable_for(
                host::current(),
                &image,
                &[NormalizedPath::new(cache.path())],
                None,
                None
            ),
            Some(shim)
        );
    }

    /// An in-place compiler upgrade must change the shim zccache sees, or
    /// zccache's path/mtime/size fingerprint would reuse the old compiler.
    #[test]
    fn upgrading_the_image_in_place_changes_the_shim() {
        if host::current().is_windows() {
            return;
        }
        let cache = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("x86_64-linux-cosmo-gcc");
        std::fs::copy(hello(), &image).unwrap();
        let dirs = [NormalizedPath::new(cache.path())];
        let before = native_executable_for(host::current(), &image, &dirs, None, None).unwrap();
        let mut grown = std::fs::read(hello()).unwrap();
        grown.extend_from_slice(b"\0upgrade");
        std::fs::write(&image, grown).unwrap();
        let after = native_executable_for(host::current(), &image, &dirs, None, None).unwrap();
        assert_ne!(before, after);
        assert_ne!(
            std::fs::read(&before).unwrap(),
            std::fs::read(&after).unwrap()
        );
        assert_eq!(before.file_name(), after.file_name());
    }

    #[test]
    fn windows_non_ape_and_unusable_cache_yield_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            native_executable_for(
                WINDOWS,
                &hello(),
                &[NormalizedPath::new(dir.path())],
                None,
                None
            ),
            None
        );
        let script = dir.path().join("tool");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        assert_eq!(
            native_executable_for(
                LINUX_X64,
                &script,
                &[NormalizedPath::new(dir.path())],
                None,
                None
            ),
            None
        );
        let file_parent = dir.path().join("f");
        std::fs::write(&file_parent, "").unwrap();
        assert_eq!(
            native_executable_for(
                LINUX_X64,
                &hello(),
                &[NormalizedPath::new(file_parent.join("cache"))],
                None,
                None
            ),
            None
        );
    }
}
