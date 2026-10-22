use super::*;

/// Retry an exec that failed with ETXTBSY. A sibling test thread that
/// forks while a fake esptool is being written inherits the write fd until
/// its child execs, so even a staged-and-renamed script can be briefly busy.
macro_rules! busy_retry {
    ($call:expr) => {{
        let mut attempts = 0;
        loop {
            let result = $call.await;
            match &result {
                Err(error) if attempts < 50 && error.to_string().contains("Text file busy") => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                _ => break result,
            }
        }
    }};
}

#[test]
fn extract_version_from_pioarduino_metadata_url() {
    // The registry release tag (0.0.1) must NOT win over the real esptool
    // version embedded in the filename.
    assert_eq!(
        extract_esptool_version(
            "https://github.com/pioarduino/registry/releases/download/0.0.1/esptoolpy-v5.3.0.zip"
        ),
        "5.3.0"
    );
}

#[test]
fn extract_version_from_bare_filename() {
    assert_eq!(extract_esptool_version("esptoolpy-v4.8.1.zip"), "4.8.1");
}

#[test]
fn extract_version_from_release_tag_when_filename_is_generic() {
    // platform-espressif32 53.03.10 publishes the opposite shape: generic
    // filename, version in the release tag. Returning "unknown" here 404s
    // the download and degrades to a bare `esptool` PATH lookup, which
    // broke every ESP32 build on FastLED master (FastLED/fbuild#1217).
    assert_eq!(
        extract_esptool_version(
            "https://github.com/pioarduino/esptool/releases/download/v4.8.5/esptool.zip"
        ),
        "4.8.5"
    );
}

#[test]
fn filename_version_still_wins_over_release_tag() {
    // Guards the ordering: both components carry a dotted version here, and
    // the filename's must win or we regress the registry-tag case.
    assert_eq!(
        extract_esptool_version(
            "https://github.com/pioarduino/registry/releases/download/0.0.1/esptoolpy-v5.3.0.zip"
        ),
        "5.3.0"
    );
}

#[test]
fn extract_version_falls_back_to_unknown() {
    assert_eq!(
        extract_esptool_version("https://example.com/esptool.zip"),
        "unknown"
    );
}

#[test]
fn platform_tag_is_known_for_this_host_or_none() {
    // Just assert the mapping is total over the match arms without panic;
    // the value depends on the build host.
    let tag = tasmota_platform_tag();
    if let Some(t) = tag {
        assert!(t.starts_with("linux-") || t.starts_with("macos-") || t.starts_with("windows-"));
    }
}

#[test]
fn find_binary_at_root() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::write(root.join(esptool_bin_name()), b"bin").unwrap();
    let found = find_esptool_binary(root).unwrap();
    assert_eq!(found.as_path(), root.join(esptool_bin_name()));
}

#[test]
fn find_binary_nested_one_level() {
    let tmp = tempfile::TempDir::new().unwrap();
    let inner = tmp.path().join("esptool-linux-amd64");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join(esptool_bin_name()), b"bin").unwrap();
    let found = find_esptool_binary(tmp.path()).unwrap();
    assert_eq!(found.as_path(), inner.join(esptool_bin_name()));
}

#[test]
fn find_binary_accepts_a_nested_ape_com() {
    let tmp = tempfile::TempDir::new().unwrap();
    let inner = tmp.path().join("esptool-any");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("esptool.com"), b"MZqFpD='\n").unwrap();
    let found = find_esptool_binary(tmp.path()).unwrap();
    assert_eq!(found.as_path(), inner.join("esptool.com"));
}

#[test]
fn find_binary_missing_returns_none() {
    let tmp = tempfile::TempDir::new().unwrap();
    assert_eq!(find_esptool_binary(tmp.path()), None);
}

#[test]
fn validate_rejects_tree_without_binary() {
    let tmp = tempfile::TempDir::new().unwrap();
    assert!(validate_esptool(tmp.path()).is_err());
}

#[test]
fn invalid_cached_install_is_removed_for_reprovisioning() {
    let tmp = tempfile::TempDir::new().unwrap();
    let install = tmp.path().join("cached-esptool");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::write(install.join("stale-marker"), b"incomplete").unwrap();

    remove_invalid_cached_install(&install).unwrap();

    assert!(
        !install.exists(),
        "an invalid cache hit must be removed before staged_install runs"
    );
}

#[test]
fn valid_cached_install_is_preserved() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join(esptool_bin_name()), b"bin").unwrap();

    remove_invalid_cached_install(tmp.path()).unwrap();

    assert!(tmp.path().join(esptool_bin_name()).exists());
}

#[tokio::test]
async fn verify_uses_supported_version_subcommand() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let bin = tmp.path().join(esptool_bin_name());
    write_script(
        &bin,
        "#!/bin/sh\n[ \"$#\" -eq 1 ] && [ \"$1\" = \"version\" ]\n",
    );

    busy_retry!(verify_esptool_binary(&bin)).unwrap();
}

/// A fake esptool that prints `esptool.py v4.5.1` and appends a line to
/// `calls` on every run, so tests can count spawns.
/// Install an executable script without ever exec'ing a file that is
/// open for writing: a sibling test thread forking mid-write inherits
/// the write fd, and exec then fails with ETXTBSY.
fn write_script(path: &Path, body: &str) {
    let staging = path.with_extension("staging");
    std::fs::write(&staging, body).unwrap();
    fbuild_core::platform::fs::set_executable(&staging).unwrap();
    std::fs::rename(&staging, path).unwrap();
}

fn counting_esptool(dir: &Path) -> (NormalizedPath, NormalizedPath) {
    let bin = NormalizedPath::new(dir.join(esptool_bin_name()));
    let calls = NormalizedPath::new(dir.join("calls"));
    write_script(
        &bin,
        &format!(
            "#!/bin/sh\necho x >> '{}'\necho 'esptool.py v4.5.1'\n",
            calls.display()
        ),
    );
    (bin, calls)
}

fn spawn_count(calls: &Path) -> usize {
    std::fs::read_to_string(calls)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[tokio::test]
async fn verify_once_spawns_esptool_only_for_the_first_build() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let (bin, calls) = counting_esptool(tmp.path());

    busy_retry!(verify_esptool_once(&bin, tmp.path(), Some("4.5.1"))).unwrap();
    busy_retry!(verify_esptool_once(&bin, tmp.path(), Some("4.5.1"))).unwrap();
    busy_retry!(verify_esptool_once(&bin, tmp.path(), Some("4.5.1"))).unwrap();

    assert_eq!(spawn_count(&calls), 1);
}

#[tokio::test]
async fn verify_once_reverifies_a_replaced_binary() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let (bin, calls) = counting_esptool(tmp.path());
    busy_retry!(verify_esptool_once(&bin, tmp.path(), None)).unwrap();

    let mut script = std::fs::read_to_string(&bin).unwrap();
    script.push_str("# replaced\n");
    write_script(&bin, &script);
    busy_retry!(verify_esptool_once(&bin, tmp.path(), None)).unwrap();

    assert_eq!(spawn_count(&calls), 2);
}

#[tokio::test]
async fn verify_once_rejects_the_wrong_version_every_time() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let (bin, calls) = counting_esptool(tmp.path());

    assert!(busy_retry!(verify_esptool_once(&bin, tmp.path(), Some("9.9.9"))).is_err());
    assert!(busy_retry!(verify_esptool_once(&bin, tmp.path(), Some("9.9.9"))).is_err());
    assert_eq!(spawn_count(&calls), 2, "a failed check must not be stamped");

    busy_retry!(verify_esptool_once(&bin, tmp.path(), Some("4.5.1"))).unwrap();
    assert_eq!(spawn_count(&calls), 3, "a new expected version re-verifies");
}

/// `FBUILD_ESPTOOL_PATH` is process-global; serialize the tests that touch
/// it so a parallel run can't observe another test's value.
static ESPTOOL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `f` with `FBUILD_ESPTOOL_PATH` set to `value` (or unset for `None`),
/// restoring whatever the environment had before.
fn with_env_override<T>(value: Option<&Path>, f: impl FnOnce() -> T) -> T {
    let _guard = ESPTOOL_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let saved = std::env::var_os(ESPTOOL_PATH_ENV_VAR);
    match value {
        Some(v) => std::env::set_var(ESPTOOL_PATH_ENV_VAR, v),
        None => std::env::remove_var(ESPTOOL_PATH_ENV_VAR),
    }
    let out = f();
    match saved {
        Some(v) => std::env::set_var(ESPTOOL_PATH_ENV_VAR, v),
        None => std::env::remove_var(ESPTOOL_PATH_ENV_VAR),
    }
    out
}

#[test]
fn override_unset_provisions_normally() {
    assert!(
        with_env_override(None, esptool_path_override)
            .unwrap()
            .is_none()
    );
}

#[test]
fn override_pointing_at_a_file_wins() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = tmp.path().join(esptool_bin_name());
    std::fs::write(&bin, b"stub").unwrap();

    let got = with_env_override(Some(&bin), esptool_path_override).unwrap();

    assert_eq!(got.map(|p| p.as_path().to_path_buf()), Some(bin));
}

/// The whole point of the escape hatch is that it's used when provisioning
/// is broken; silently falling back to provisioning would hide the typo.
#[test]
fn override_pointing_nowhere_is_an_error_not_a_fallback() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ghost = tmp.path().join("does-not-exist");

    let err = with_env_override(Some(&ghost), esptool_path_override).unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains(ESPTOOL_PATH_ENV_VAR), "{msg}");
    assert!(msg.contains("does-not-exist"), "{msg}");
}

/// A directory is not an executable — treat it like any other non-file.
#[test]
fn override_pointing_at_a_directory_is_an_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    assert!(with_env_override(Some(tmp.path()), esptool_path_override).is_err());
}

#[test]
fn empty_override_is_treated_as_unset() {
    assert!(
        with_env_override(Some(Path::new("")), esptool_path_override)
            .unwrap()
            .is_none()
    );
}

/// The #1217 fault: an unparseable metadata URL yields version `unknown`,
/// which builds a `vunknown` download URL that 404s. Reporting that URL is
/// what makes the failure diagnosable at the point it happens.
/// A value that is present but blank is a shell artifact
/// (`FBUILD_ESPTOOL_PATH="$SOMETHING_UNSET"`), not a deliberate override,
/// so it must read as unset rather than as a path that doesn't exist.
#[test]
fn whitespace_only_override_is_treated_as_unset() {
    assert!(
        with_env_override(Some(Path::new(" \t")), esptool_path_override)
            .unwrap()
            .is_none()
    );
}

#[test]
fn download_url_reports_the_parsed_version() {
    let tmp = tempfile::TempDir::new().unwrap();
    let esptool = Esptool::from_metadata_url(tmp.path(), "https://example.com/esptool.zip");

    assert_eq!(esptool.version(), "unknown");
    if let Ok(url) = esptool.download_url() {
        assert!(url.contains("/download/vunknown/"), "{url}");
    }
}

#[test]
fn download_url_matches_the_url_ensure_installed_uses() {
    let tmp = tempfile::TempDir::new().unwrap();
    let esptool = Esptool::from_metadata_url(
        tmp.path(),
        "https://github.com/pioarduino/registry/releases/download/0.0.1/esptoolpy-v5.3.0.zip",
    );

    assert_eq!(esptool.version(), "5.3.0");
    if fbuild_core::platform::host::is_windows() {
        assert_eq!(
            esptool.download_url().unwrap(),
            "https://github.com/pioarduino/esptool/releases/download/v5.3.0/esptool.zip"
        );
    } else if let Some(tag) = tasmota_platform_tag() {
        assert_eq!(
            esptool.download_url().unwrap(),
            Esptool::release_url("5.3.0", tag)
        );
    }
}

#[test]
fn windows_registry_esptool_uses_published_source_archives() {
    let tmp = tempfile::TempDir::new().unwrap();
    // The real 5.1.0 registry tools.json publishes this any-host source URL;
    // its upstream release has no esptool-windows-amd64.zip asset.
    for version in ["5.1.0", "5.3.0"] {
        let metadata = format!(
            "https://github.com/pioarduino/registry/releases/download/0.0.1/esptoolpy-v{version}.zip"
        );
        let source = format!(
            "https://github.com/pioarduino/esptool/releases/download/v{version}/esptool.zip"
        );
        let windows = Esptool::from_metadata_url_for_host(tmp.path(), &metadata, true);
        assert_eq!(windows.version(), version);
        assert_eq!(windows.download_url().unwrap(), source);
        assert_eq!(windows.source_url.as_deref(), Some(source.as_str()));
        assert_eq!(windows.expected_runtime_version.as_deref(), Some(version));

        let unix = Esptool::from_metadata_url_for_host(tmp.path(), &metadata, false);
        assert!(unix.source_url.is_none());
        assert_eq!(unix.version(), version);
    }
}

#[test]
fn windows_source_selection_preserves_direct_and_platformio_archives() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = "https://github.com/pioarduino/esptool/releases/download/v4.7.5/esptool.zip";
    for windows in [false, true] {
        let esptool = Esptool::from_metadata_url_for_host(tmp.path(), source, windows);
        assert_eq!(esptool.download_url().unwrap(), source);
        assert_eq!(esptool.expected_runtime_version.as_deref(), Some("4.7.5"));
    }
    let payload = fbuild_core::platformio_package::ResolvedPayload {
        owner: "platformio".into(),
        kind: fbuild_core::platformio_package::PackageKind::Tool,
        name: "tool-esptoolpy".into(),
        version: "1.30300.0".into(),
        system: "all".into(),
        url: "https://dl.registry.platformio.org/download/platformio/tool/tool-esptoolpy/1.30300.0/tool-esptoolpy-1.30300.0.tar.gz".into(),
        sha256: "published-registry-digest".into(),
    };
    let legacy = Esptool::from_registry_payload(tmp.path(), &payload);
    assert_eq!(legacy.download_url().unwrap(), payload.url);
    assert_eq!(legacy.version(), "1.30300.0");
    assert_eq!(
        legacy.source_checksum.as_deref(),
        Some(payload.sha256.as_str())
    );
    assert!(legacy.expected_runtime_version.is_none());
}

#[test]
fn pinned_pioarduino_source_archive_is_not_replaced_by_tasmota() {
    let tmp = tempfile::TempDir::new().unwrap();
    let url = "https://github.com/pioarduino/esptool/releases/download/v4.7.5/esptool.zip";
    let esptool = Esptool::from_metadata_url(tmp.path(), url);
    assert_eq!(esptool.version(), "4.7.5");
    assert_eq!(esptool.download_url().unwrap(), url);
    assert_eq!(
        source_esptool_path_for_host(tmp.path(), true),
        source_tools_dir(tmp.path()).join("esptool/Scripts/esptool.exe")
    );
    assert_eq!(
        source_esptool_path_for_host(tmp.path(), false),
        source_tools_dir(tmp.path()).join("esptool/bin/esptool.py")
    );
    assert!(validate_source_esptool(tmp.path()).is_err());
}

#[test]
fn windows_source_esptool_resolves_uv_console_launcher() {
    let tmp = tempfile::TempDir::new().unwrap();
    let scripts = source_tools_dir(tmp.path()).join("esptool/Scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    // uv strips the .py suffix from the console-script name before
    // appending .exe on Windows. Model the installed filesystem layout.
    let launcher = scripts.join("esptool.exe");
    std::fs::write(&launcher, b"installed console launcher").unwrap();

    let resolved = source_esptool_path_for_host(tmp.path(), true);
    assert!(resolved.is_file(), "missing managed launcher: {resolved}");
    assert_eq!(resolved, launcher);
}

#[test]
fn legacy_platformio_esptool_setup_py_archive_is_valid() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("setup.py"),
        "from setuptools import setup\nsetup()\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("esptool.py"), "print('esptool')\n").unwrap();

    validate_source_esptool(tmp.path()).unwrap();
}
