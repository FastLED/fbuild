//! fbuild policy for the canonical running-process APE loader.
//!
//! Extraction, validation, installation and nested launches belong to
//! running-process. This adapter preserves fbuild's environment overrides.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::OnceLock;

use crate::path::NormalizedPath;

/// fbuild override for the shared APE loader.
pub const LOADER_ENV: &str = "FBUILD_APE_LOADER";
/// fbuild override for extracted loader storage.
pub const CACHE_DIR_ENV: &str = "FBUILD_APE_CACHE_DIR";

static DEFAULT_CACHE_ROOT: OnceLock<NormalizedPath> = OnceLock::new();

/// Register the fbuild cache selected by the application at startup.
/// The first registration wins, as with the original fbuild loader.
pub fn set_default_cache_root(dir: impl AsRef<Path>) {
    let _ = DEFAULT_CACHE_ROOT.set(NormalizedPath::new(dir));
}

/// Whether a file is an APE image.
pub fn is_ape_file(path: &Path) -> bool {
    running_process::ape::is_ape_file(path)
}

/// Whether a header starts with an APE magic.
pub fn is_ape_header(header: &[u8]) -> bool {
    running_process::ape::is_ape_header(header)
}

/// Prepared loader and image for an external tool.
pub struct ApeLaunch {
    pub loader: NormalizedPath,
    pub image: NormalizedPath,
    inner: running_process::ape::ApeLaunch,
}

impl ApeLaunch {
    /// Make the loader available to tools launched by the child.
    pub fn child_path(&self, inherited: Option<&OsStr>) -> Option<OsString> {
        self.inner.child_path(inherited)
    }
}

fn options(overlay: Option<&[(&str, &str)]>) -> running_process::ape::ApeOptions {
    options_with_cache(overlay, DEFAULT_CACHE_ROOT.get().map(AsRef::as_ref))
}

fn options_with_cache(
    overlay: Option<&[(&str, &str)]>,
    default_cache: Option<&Path>,
) -> running_process::ape::ApeOptions {
    let vars = overlay.unwrap_or_default();
    let value = |key: &str| {
        vars.iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| OsString::from(v))
            .or_else(|| std::env::var_os(key))
    };
    let mut options = running_process::ape::ApeOptions::with_overrides(
        false,
        vars.iter()
            .map(|(k, v)| (OsStr::new(k), Some(OsStr::new(v)))),
    );
    if let Some(loader) = value(LOADER_ENV).filter(|v| !v.is_empty()) {
        options.loader = Some(loader);
    }
    let cache = value(CACHE_DIR_ENV)
        .filter(|v| !v.is_empty())
        .map(NormalizedPath::new)
        .or_else(|| {
            value("FBCACHE_DIR")
                .filter(|v| !v.is_empty())
                .map(|root| NormalizedPath::new(root).join("ape"))
        })
        .or_else(|| {
            value("FBUILD_CACHE_DIR")
                .filter(|v| !v.is_empty())
                .map(|root| NormalizedPath::new(root).join("ape"))
        });
    // Keep the registered fbuild root as a fallback even when the caller
    // supplies an override that the shared loader cannot execute from.
    if let Some(default_cache) = default_cache {
        options
            .cache_dirs
            .insert(0, NormalizedPath::new(default_cache).into_path_buf());
    }
    if let Some(cache) = cache {
        options.cache_dirs.insert(0, cache.into_path_buf());
    }
    options
}

/// Plan a tool launch with the child's cwd and environment overlay.
pub fn plan_launch(
    program: &OsStr,
    cwd: Option<&Path>,
    overlay: Option<&[(&str, &str)]>,
) -> Option<ApeLaunch> {
    let inner = running_process::ape::plan_launch(program, cwd, &options(overlay))?;
    Some(ApeLaunch {
        loader: inner.loader.clone().into(),
        image: inner.image.clone().into(),
        inner,
    })
}

/// Translate fbuild's APE settings for a compiler spawned by embedded zccache.
/// The compiler path and argv remain unchanged; kernal-api plans the launch.
pub fn add_environment_overrides(env: &mut Vec<(String, String)>) {
    let overlay: Vec<_> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let options = options(Some(&overlay));
    let settings = [
        (running_process::ape::LOADER_ENV, options.loader),
        (
            running_process::ape::CACHE_DIR_ENV,
            options
                .cache_dirs
                .first()
                .map(|path| path.as_os_str().to_owned()),
        ),
    ];
    for (key, value) in settings {
        if let Some(value) = value.and_then(|value| value.into_string().ok()) {
            env.retain(|(k, _)| k != key);
            env.push((key.to_string(), value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_overrides_select_loader_and_cache() {
        let opts = options(Some(&[
            (LOADER_ENV, "first"),
            (LOADER_ENV, "chosen-loader"),
            (CACHE_DIR_ENV, "chosen-cache"),
        ]));
        assert_eq!(opts.loader, Some(OsString::from("chosen-loader")));
        assert_eq!(
            opts.cache_dirs[0],
            NormalizedPath::new("chosen-cache").into_path_buf()
        );
    }

    #[test]
    fn fbcache_root_is_preserved() {
        let opts = options(Some(&[
            ("FBCACHE_DIR", "session-cache"),
            (CACHE_DIR_ENV, ""),
        ]));
        assert_eq!(
            opts.cache_dirs[0],
            NormalizedPath::new("session-cache")
                .join("ape")
                .into_path_buf()
        );
    }

    #[test]
    fn registered_cache_root_is_preserved() {
        let opts = options_with_cache(
            Some(&[
                (CACHE_DIR_ENV, ""),
                ("FBCACHE_DIR", ""),
                ("FBUILD_CACHE_DIR", ""),
            ]),
            Some(Path::new("registered-cache/ape")),
        );
        assert_eq!(
            opts.cache_dirs[0],
            NormalizedPath::new("registered-cache/ape").into_path_buf()
        );
    }

    #[test]
    fn explicit_cache_keeps_registered_root_as_a_fallback() {
        let opts = options_with_cache(
            Some(&[(CACHE_DIR_ENV, "preferred-cache")]),
            Some(Path::new("registered-cache/ape")),
        );
        assert_eq!(
            opts.cache_dirs[0],
            NormalizedPath::new("preferred-cache").into_path_buf()
        );
        assert_eq!(
            opts.cache_dirs[1],
            NormalizedPath::new("registered-cache/ape").into_path_buf()
        );
    }

    #[test]
    fn embedded_compiler_receives_fbuild_overrides() {
        let mut env = vec![
            (LOADER_ENV.to_string(), "chosen-loader".to_string()),
            (CACHE_DIR_ENV.to_string(), "chosen-cache".to_string()),
        ];
        add_environment_overrides(&mut env);
        assert!(env.contains(&(
            running_process::ape::LOADER_ENV.to_string(),
            "chosen-loader".to_string()
        )));
        assert!(env.contains(&(
            running_process::ape::CACHE_DIR_ENV.to_string(),
            "chosen-cache".to_string()
        )));
    }

    #[test]
    fn missing_program_has_no_launch_plan() {
        assert!(plan_launch(OsStr::new("fbuild-missing-ape-fixture"), None, None).is_none());
    }

    fn fixture() -> NormalizedPath {
        NormalizedPath::new(env!("CARGO_MANIFEST_DIR")).join("data/ape-hello/hello.com")
    }

    #[test]
    fn real_ape_runs_through_the_contained_std_path() {
        use crate::platform::process::{self, ContainedStdio, StdioSource};
        use std::io::Read;
        let mut command = process::command(fixture());
        command.arg("std path");
        let mut child = process::spawn_contained(
            &mut command,
            ContainedStdio {
                stdout: StdioSource::Pipe,
                stderr: StdioSource::Pipe,
                ..ContainedStdio::default()
            },
        )
        .expect("real APE spawns");
        let mut stdout = String::new();
        child
            .take_stdout()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        assert_eq!(child.wait().unwrap(), 0);
        assert_eq!(stdout, "hello world std path\n");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_ape_runs_with_child_cwd_and_hostile_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let image = NormalizedPath::new(tmp.path()).join("tool.com");
        std::fs::copy(fixture(), &image).unwrap();
        crate::platform::fs::set_executable(&image).unwrap();
        let cache = NormalizedPath::new(tmp.path()).join("cache");
        let cache_str = cache.to_str().unwrap();
        let out = crate::subprocess::run_command(
            &["./tool.com", "argument with spaces"],
            Some(tmp.path()),
            Some(&[
                ("PATH", "/nonexistent"),
                ("TMPDIR", "/nonexistent"),
                ("HOME", "/nonexistent"),
                (CACHE_DIR_ENV, cache_str),
            ]),
            Some(std::time::Duration::from_secs(60)),
        )
        .await
        .expect("real APE subprocess spawns");
        assert!(out.success(), "{}", out.stderr);
        assert_eq!(out.stdout, "hello world argument with spaces\n");
    }

    #[test]
    fn concurrent_first_launches_use_the_shared_loader() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = NormalizedPath::new(tmp.path()).join("cache");
        let image = fixture();
        let barrier = std::sync::Barrier::new(48);
        std::thread::scope(|scope| {
            for _ in 0..48 {
                let barrier = &barrier;
                let image = &image;
                let cache = &cache;
                scope.spawn(move || {
                    barrier.wait();
                    let launch = plan_launch(
                        image.as_os_str(),
                        None,
                        Some(&[(CACHE_DIR_ENV, cache.to_str().unwrap())]),
                    );
                    // Windows runs the PE image natively.
                    let mut command = match launch {
                        Some(launch) => {
                            // allow-direct-spawn: construction only, spawned through the contained facade below.
                            let mut command = std::process::Command::new(&launch.loader);
                            command.arg(&launch.image);
                            command
                        }
                        None => crate::platform::process::command(image),
                    };
                    let mut child = crate::platform::process::spawn_contained(
                        &mut command,
                        crate::platform::process::ContainedStdio {
                            stdout: crate::platform::process::StdioSource::Null,
                            ..crate::platform::process::ContainedStdio::default()
                        },
                    )
                    .expect("concurrent APE launch");
                    assert_eq!(child.wait().unwrap(), 0);
                });
            }
        });
    }
}
