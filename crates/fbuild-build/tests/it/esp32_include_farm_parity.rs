//! Real-SDK parity for the ESP32 header farm (FastLED/fbuild#1588).
//!
//! The farm planner's equivalence check reads `#include` lines as text, so it
//! cannot see macro includes (`#include FOO_H`) or a farm that compiles but
//! picks a *different* header. This test asks GCC instead: it builds a real
//! ESP32 fixture with the farm and with plain `-I` (`FBUILD_INCLUDE_FARM=0`)
//! and asserts every translation unit's `-MMD` depfile names the same real
//! header files.
//!
//! Run one variant with:
//! `soldr cargo test -p fbuild-build --test it -- --ignored --exact
//!  esp32_include_farm_parity::farm_parity_esp32dev --nocapture --test-threads=1`

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use fbuild_build::{BuildOrchestrator, BuildParams, compile_backend};
use fbuild_core::BuildProfile;
use fbuild_core::path::NormalizedPath;

const REAL_BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

/// `FBUILD_INCLUDE_FARM` is process-wide; serialize the builds that flip it.
static FARM_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn install_test_compile_backend() {
    static INSTALL: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    INSTALL
        .get_or_init(|| async {
            let backend = compile_backend::CompileBackend::start()
                .await
                .expect("compile backend starts for ESP32 farm parity test");
            compile_backend::install_global(backend);
        })
        .await;
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == fbuild_paths::FBUILD_DIR_NAME || name == "compile_commands.json" {
            continue;
        }
        let dest = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

async fn build(project_dir: &Path, env_name: &str, farm: bool) -> NormalizedPath {
    let build_dir = project_dir.join(format!(
        "{}/{}/{}",
        fbuild_paths::FBUILD_DIR_NAME,
        fbuild_paths::BUILD_DIR_NAME,
        if farm { "farm" } else { "plain" }
    ));
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: env_name.to_string(),
        clean_all: false,
        clean_only: false,
        clean: true,
        profile: BuildProfile::Release,
        build_dir: build_dir.clone(),
        verbose: false,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: false,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
        caller_path: None,
    };
    // Process-wide; FARM_ENV keeps the other parity tests out meanwhile.
    std::env::set_var("FBUILD_INCLUDE_FARM", if farm { "1" } else { "0" });
    let result = tokio::time::timeout(
        REAL_BUILD_TIMEOUT,
        fbuild_build::esp32::orchestrator::Esp32Orchestrator.build(&params),
    )
    .await
    .unwrap_or_else(|_| panic!("{env_name} build (farm={farm}) exceeded its time budget"));
    std::env::remove_var("FBUILD_INCLUDE_FARM");
    let result = result.unwrap_or_else(|e| panic!("{env_name} build (farm={farm}) failed: {e}"));
    assert!(result.success, "{env_name} build (farm={farm}) failed");
    NormalizedPath::from(build_dir)
}

/// Headers listed by one depfile, resolved to real paths. The first entry
/// (the object target) is dropped; relative paths are relative to the project.
/// Files generated inside the build dir (the sketch's `.ino.cpp`) are keyed
/// relative to it, since the two builds use different build dirs.
fn depfile_headers(
    depfile: &Path,
    build_dir: &Path,
    project_dir: &Path,
) -> BTreeSet<NormalizedPath> {
    let build_dir = fs::canonicalize(build_dir).unwrap();
    let text = fs::read_to_string(depfile).unwrap().replace("\\\n", " ");
    let Some((_, deps)) = text.split_once(": ") else {
        return BTreeSet::new();
    };
    deps.split_whitespace()
        .filter(|p| !p.ends_with(':'))
        .map(|p| {
            let path = Path::new(p);
            let path = if path.is_relative() {
                project_dir.join(path)
            } else {
                path.to_path_buf()
            };
            let path = fs::canonicalize(&path).unwrap_or(path);
            NormalizedPath::from(match path.strip_prefix(&build_dir) {
                Ok(rel) => Path::new("<build>").join(rel),
                Err(_) => path,
            })
        })
        .collect()
}

/// `core/Esp_da4f.cpp.d` -> `core/Esp.cpp.d`: the suffix hashes the source
/// path, which for generated sketch sources includes the build dir.
fn unit_key(rel: &Path) -> NormalizedPath {
    let name = rel.file_name().unwrap().to_string_lossy().into_owned();
    // The hash sits right before the source extension: `<stem>_<4 hex>.<ext>.d`.
    let hashed = name.char_indices().find(|&(i, c)| {
        c == '_'
            && name
                .get(i + 1..i + 5)
                .is_some_and(|h| h.chars().all(|c| c.is_ascii_hexdigit()))
            && name.get(i + 5..i + 6) == Some(".")
    });
    match hashed {
        Some((i, _)) => {
            NormalizedPath::from(rel.with_file_name(format!("{}{}", &name[..i], &name[i + 5..])))
        }
        None => NormalizedPath::from(rel),
    }
}

fn depfiles(
    build_dir: &Path,
    project_dir: &Path,
) -> BTreeMap<NormalizedPath, BTreeSet<NormalizedPath>> {
    fn walk(dir: &Path, out: &mut Vec<NormalizedPath>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "d") {
                out.push(NormalizedPath::from(path));
            }
        }
    }
    let mut files = Vec::new();
    walk(build_dir, &mut files);
    files
        .into_iter()
        .map(|d| {
            let rel = unit_key(d.strip_prefix(build_dir).unwrap());
            (rel, depfile_headers(&d, build_dir, project_dir))
        })
        .collect()
}

/// Whether any raw depfile under `build_dir` names a header via the farm.
fn mentions_farm(build_dir: &Path) -> bool {
    fn walk(dir: &Path) -> bool {
        fs::read_dir(dir).unwrap().flatten().any(|entry| {
            let path = entry.path();
            if path.is_dir() {
                walk(&path)
            } else {
                path.extension().is_some_and(|e| e == "d")
                    && fs::read_to_string(&path).is_ok_and(|t| t.contains("include-farms"))
            }
        })
    }
    walk(build_dir)
}

async fn assert_farm_parity(fixture: &str, env_name: &str) {
    install_test_compile_backend().await;
    let _guard = FARM_ENV.lock().await;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/platform");
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path().join(fixture);
    copy_tree(&fixtures.join(fixture), &project_dir);

    let plain_dir = build(&project_dir, env_name, false).await;
    let farm_dir = build(&project_dir, env_name, true).await;
    // Guard against a vacuous pass: the farm build must really go through the
    // farm (it falls back to plain -I on any error), and the plain one must not.
    assert!(
        mentions_farm(&farm_dir),
        "{env_name}: farm build never used the include farm"
    );
    assert!(
        !mentions_farm(&plain_dir),
        "{env_name}: FBUILD_INCLUDE_FARM=0 still used the farm"
    );
    let plain = depfiles(&plain_dir, &project_dir);
    let farm = depfiles(&farm_dir, &project_dir);

    assert!(
        !plain.is_empty(),
        "{env_name}: no depfiles in {}",
        plain_dir.display()
    );
    let plain_keys: BTreeSet<_> = plain.keys().collect();
    let farm_keys: BTreeSet<_> = farm.keys().collect();
    assert!(
        plain_keys == farm_keys,
        "{env_name}: farm and plain builds compiled different units\n  only without farm: {:?}\n  only with farm:    {:?}",
        plain_keys.difference(&farm_keys).collect::<Vec<_>>(),
        farm_keys.difference(&plain_keys).collect::<Vec<_>>()
    );

    let mut diverged = Vec::new();
    for (unit, headers) in &plain {
        let farm_headers = &farm[unit];
        if headers != farm_headers {
            let only_plain: Vec<_> = headers.difference(farm_headers).take(5).collect();
            let only_farm: Vec<_> = farm_headers.difference(headers).take(5).collect();
            diverged.push(format!(
                "{}\n  only without farm: {only_plain:?}\n  only with farm:    {only_farm:?}",
                unit.display()
            ));
        }
    }
    assert!(
        diverged.is_empty(),
        "{env_name}: the include farm resolved headers differently in {} of {} units:\n{}",
        diverged.len(),
        plain.len(),
        diverged.join("\n")
    );
    eprintln!(
        "{env_name}: {} units resolve identical headers with and without the farm",
        plain.len()
    );
}

macro_rules! farm_parity {
    ($($name:ident => ($fixture:literal, $env:literal)),* $(,)?) => {$(
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        #[ignore = "downloads ESP32 toolchain (~hundreds of MB)"]
        async fn $name() {
            assert_farm_parity($fixture, $env).await;
        }
    )*};
}

farm_parity! {
    farm_parity_esp32dev => ("esp32dev", "esp32dev"),
    farm_parity_esp32s2 => ("esp32s2", "esp32s2"),
    farm_parity_esp32s3 => ("esp32s3", "esp32s3"),
    farm_parity_esp32c2 => ("esp32c2", "esp32c2"),
    farm_parity_esp32c3 => ("esp32c3", "esp32c3"),
    farm_parity_esp32c5 => ("esp32c5", "esp32c5"),
    farm_parity_esp32c6 => ("esp32c6", "esp32c6"),
    farm_parity_esp32h2 => ("esp32h2", "esp32h2"),
    farm_parity_esp32p4 => ("esp32p4", "esp32p4"),
}
