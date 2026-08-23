//! Library Manager panel (FastLED/fbuild#1076 Phase 2): a daemon-served,
//! self-contained, **read-only** HTML page plus its data endpoint for
//! browsing a project's declared `lib_deps`.
//!
//! - `GET /api/ide/libraries?project=<dir>&env=<name>` — parses
//!   `<project>/platformio.ini`'s `lib_deps` for the given (or default)
//!   environment, classifies each entry with
//!   [`fbuild_config::classify_lib_dep`] (the same classifier `fbuild sync`
//!   uses — moved to `fbuild-config` so both crates can share it without
//!   `fbuild-daemon` depending on `fbuild-cli`), and reports a best-effort
//!   installed/not-installed flag per entry.
//! - `GET /libraries` — the page itself; reads `?project=&env=` from its
//!   own URL, fetches the endpoint above, and renders a table. No
//!   mutation, no network fetches, no external assets.
//!
//! ## Install-state detection reality
//!
//! There is no single fixed "installed libraries" directory. Per-project,
//! per-environment library output lands under
//! `fbuild_paths::BuildLayout::resolve()` (`<project>/.fbuild/build/<env>/
//! <profile>/libs/`), and it's only populated once *after* a build that
//! needed dependencies has actually run — `fbuild-build`'s ESP32
//! orchestrator, for example, only creates it when `lib_deps` is non-empty
//! (`crates/fbuild-build-esp/src/esp32/orchestrator/build.rs`). This
//! handler checks the **release** profile's `libs/` directory for a
//! same-named (case-insensitive) subdirectory as a best-effort signal —
//! not a guarantee. If that directory doesn't exist yet (no build has run)
//! every entry reports `installed: false`, which the response's
//! `install_state_note` field spells out so a client doesn't misread that
//! as "this library isn't available anywhere."

use std::path::Path;

use fbuild_core::path::NormalizedPath;

use axum::Json;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};

use fbuild_config::PlatformIOConfig;

use crate::models::{IdeLibrariesQuery, IdeLibrariesResponse, IdeLibraryEntry};

const LIBRARIES_PAGE_HTML: &str = include_str!("../../web/libraries/index.html");

/// Explains what `installed` means on the libraries page.
///
/// Built from the canonical path segments rather than spelled by hand: the
/// note names a real directory layout, and a note that disagrees with the
/// layout is worse than no note (FastLED/fbuild#1349).
fn install_state_note() -> String {
    format!(
        "Installed state is best-effort: it checks the release build profile's          <project>/{}/{}/<env>/release/libs/ directory for a same-named subdirectory.          That directory is only populated after a build that needed dependencies has run —          if no build has run yet, every entry reports installed: false even though the          source may be perfectly resolvable.",
        fbuild_paths::FBUILD_DIR_NAME,
        fbuild_paths::BUILD_DIR_NAME
    )
}

/// GET /libraries — serve the self-contained Library Manager page.
pub async fn libraries_page() -> impl IntoResponse {
    Html(LIBRARIES_PAGE_HTML)
}

/// Validate that `project_dir` is usable: it must exist and contain a
/// `platformio.ini`. Pure filesystem check, no parsing.
fn validate_project_dir(project_dir: &Path) -> Result<(), String> {
    if !project_dir.exists() {
        return Err(format!(
            "project directory does not exist: {}",
            project_dir.display()
        ));
    }
    if !project_dir.join("platformio.ini").is_file() {
        return Err(format!(
            "no platformio.ini found in {}",
            project_dir.display()
        ));
    }
    Ok(())
}

/// Resolve the effective environment name: explicit `env`, else the
/// config's default environment, else an error (unlike `install-deps`,
/// this is a read-only browsing endpoint, so silently guessing "default"
/// would be more confusing than telling the caller to pick one).
fn resolve_env_name(config: &PlatformIOConfig, env: Option<&str>) -> Result<String, String> {
    if let Some(e) = env {
        return Ok(e.to_string());
    }
    config
        .get_default_environment()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            "no environment specified and platformio.ini has no default_envs; pass ?env=<name>"
                .to_string()
        })
}

/// Best-effort: names of subdirectories directly under `libs_dir`, lowercased
/// for case-insensitive matching. Returns an empty set (not an error) when
/// the directory doesn't exist — that's the normal "no build has run yet"
/// state, not a failure.
async fn installed_dir_names(libs_dir: &Path) -> Vec<(String, NormalizedPath)> {
    let mut out = Vec::new();
    let Ok(mut entries) = fbuild_core::fs::read_dir(libs_dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        // `file_type()` rather than `Path::is_dir()`: the latter issues a
        // blocking `stat` on the async worker, which is the whole reason
        // this function moved off `std::fs` (FastLED/fbuild#844).
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            out.push((name.to_ascii_lowercase(), path.into()));
        }
    }
    out
}

/// Build the classified + install-state-annotated entry list for one
/// environment's `lib_deps`. Pure given its inputs — no I/O beyond the one
/// `libs_dir` read baked into `installed`, which callers already resolved.
async fn build_library_entries(
    config: &PlatformIOConfig,
    env_name: &str,
    libs_dir: &Path,
) -> Result<Vec<IdeLibraryEntry>, String> {
    let raw_deps = config
        .get_lib_deps(env_name)
        .map_err(|e| format!("invalid environment '{}': {}", env_name, e))?;

    let installed = installed_dir_names(libs_dir).await;

    Ok(raw_deps
        .iter()
        .map(|raw| {
            let classified = fbuild_config::classify_lib_dep(raw);
            let needle = classified.name.to_ascii_lowercase();
            let matched = installed.iter().find(|(name, _)| *name == needle);

            IdeLibraryEntry {
                raw: classified.raw,
                name: classified.name,
                source_type: classified.source_type,
                version_spec: classified.version_spec,
                owner: classified.owner,
                url: classified.url,
                local_path: classified.local_path,
                installed: matched.is_some(),
                installed_path: matched.map(|(_, path)| path.display().to_string()),
            }
        })
        .collect())
}

/// `GET /api/ide/libraries?project=<dir>&env=<name>`
pub async fn list_libraries(
    Query(params): Query<IdeLibrariesQuery>,
) -> (StatusCode, Json<IdeLibrariesResponse>) {
    let Some(project) = params.project.filter(|p| !p.trim().is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(IdeLibrariesResponse {
                success: false,
                project: None,
                environment: None,
                libraries: Vec::new(),
                install_state_note: install_state_note(),
                error: Some(
                    "missing required ?project=<absolute project dir> query param; \
                     run `fbuild libraries` from a project directory to open this page \
                     pre-filled"
                        .to_string(),
                ),
            }),
        );
    };

    let project_dir = NormalizedPath::from(&project);
    if let Err(e) = validate_project_dir(&project_dir) {
        return (
            StatusCode::BAD_REQUEST,
            Json(IdeLibrariesResponse {
                success: false,
                project: Some(project),
                environment: params.env.clone(),
                libraries: Vec::new(),
                install_state_note: install_state_note(),
                error: Some(e),
            }),
        );
    }

    let config = match PlatformIOConfig::from_path(&project_dir.join("platformio.ini")) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(IdeLibrariesResponse {
                    success: false,
                    project: Some(project),
                    environment: params.env.clone(),
                    libraries: Vec::new(),
                    install_state_note: install_state_note(),
                    error: Some(format!("failed to parse platformio.ini: {}", e)),
                }),
            );
        }
    };

    let env_name = match resolve_env_name(&config, params.env.as_deref()) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(IdeLibrariesResponse {
                    success: false,
                    project: Some(project),
                    environment: params.env.clone(),
                    libraries: Vec::new(),
                    install_state_note: install_state_note(),
                    error: Some(e),
                }),
            );
        }
    };

    let libs_dir = fbuild_paths::BuildLayout::new(
        project_dir.to_path_buf(),
        env_name.clone(),
        fbuild_core::BuildProfile::Release,
    )
    .resolve()
    .join("libs");

    match build_library_entries(&config, &env_name, &libs_dir).await {
        Ok(libraries) => (
            StatusCode::OK,
            Json(IdeLibrariesResponse {
                success: true,
                project: Some(project),
                environment: Some(env_name),
                libraries,
                install_state_note: install_state_note(),
                error: None,
            }),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(IdeLibrariesResponse {
                success: false,
                project: Some(project),
                environment: Some(env_name),
                libraries: Vec::new(),
                install_state_note: install_state_note(),
                error: Some(e),
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn write_project(dir: &Path, ini: &str) {
        fbuild_core::fs::write(dir.join("platformio.ini"), ini)
            .await
            .unwrap();
    }

    const SAMPLE_INI: &str = "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
lib_deps =
    FastLED@^3.6.0
    https://github.com/adafruit/Adafruit_NeoPixel.git
    ./local/mylib
";

    #[tokio::test]
    async fn libraries_page_serves_html_with_no_external_deps() {
        let response = libraries_page().await.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("/api/ide/libraries"));
        assert!(
            !html.contains("cdn.")
                && !html.contains("unpkg.com")
                && !html.contains("jsdelivr.net")
                && !html.contains("googleapis.com")
        );
    }

    #[test]
    fn validate_project_dir_rejects_missing_dir() {
        let err = validate_project_dir(Path::new("/definitely/not/a/real/path")).unwrap_err();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn validate_project_dir_rejects_dir_without_platformio_ini() {
        let tmp = tempfile::tempdir().unwrap();
        let err = validate_project_dir(tmp.path()).unwrap_err();
        assert!(err.contains("platformio.ini"));
    }

    #[tokio::test]
    async fn validate_project_dir_accepts_dir_with_platformio_ini() {
        let tmp = tempfile::tempdir().unwrap();
        write_project(tmp.path(), SAMPLE_INI).await;
        assert!(validate_project_dir(tmp.path()).is_ok());
    }

    #[tokio::test]
    async fn resolve_env_name_prefers_explicit_env() {
        let tmp = tempfile::tempdir().unwrap();
        write_project(tmp.path(), SAMPLE_INI).await;
        let config = PlatformIOConfig::from_path(&tmp.path().join("platformio.ini")).unwrap();
        assert_eq!(resolve_env_name(&config, Some("uno")).unwrap(), "uno");
    }

    #[tokio::test]
    async fn resolve_env_name_falls_back_to_default() {
        let tmp = tempfile::tempdir().unwrap();
        write_project(tmp.path(), SAMPLE_INI).await;
        let config = PlatformIOConfig::from_path(&tmp.path().join("platformio.ini")).unwrap();
        assert_eq!(resolve_env_name(&config, None).unwrap(), "uno");
    }

    #[tokio::test]
    async fn build_library_entries_classifies_and_marks_installed() {
        let tmp = tempfile::tempdir().unwrap();
        write_project(tmp.path(), SAMPLE_INI).await;
        let config = PlatformIOConfig::from_path(&tmp.path().join("platformio.ini")).unwrap();

        let libs_dir = tmp.path().join("libs");
        fbuild_core::fs::create_dir_all(libs_dir.join("FastLED"))
            .await
            .unwrap();

        let entries = build_library_entries(&config, "uno", &libs_dir)
            .await
            .unwrap();
        assert_eq!(entries.len(), 3);

        let fastled = entries.iter().find(|e| e.name == "FastLED").unwrap();
        assert!(fastled.installed);
        assert!(fastled.installed_path.is_some());
        assert_eq!(fastled.source_type, fbuild_config::SourceType::Registry);

        let neopixel = entries
            .iter()
            .find(|e| e.name == "Adafruit_NeoPixel")
            .unwrap();
        assert!(!neopixel.installed);
        assert_eq!(neopixel.source_type, fbuild_config::SourceType::Github);

        let local = entries.iter().find(|e| e.name == "mylib").unwrap();
        assert!(!local.installed);
        assert_eq!(local.source_type, fbuild_config::SourceType::LocalPath);
    }

    #[tokio::test]
    async fn build_library_entries_no_libs_dir_marks_all_uninstalled() {
        let tmp = tempfile::tempdir().unwrap();
        write_project(tmp.path(), SAMPLE_INI).await;
        let config = PlatformIOConfig::from_path(&tmp.path().join("platformio.ini")).unwrap();

        let libs_dir = tmp.path().join("does-not-exist").join("libs");
        let entries = build_library_entries(&config, "uno", &libs_dir)
            .await
            .unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().all(|e| !e.installed));
    }

    #[tokio::test]
    async fn build_library_entries_rejects_unknown_environment() {
        let tmp = tempfile::tempdir().unwrap();
        write_project(tmp.path(), SAMPLE_INI).await;
        let config = PlatformIOConfig::from_path(&tmp.path().join("platformio.ini")).unwrap();
        let err = build_library_entries(&config, "not-an-env", Path::new("/nonexistent"))
            .await
            .unwrap_err();
        assert!(err.contains("not-an-env"));
    }

    #[tokio::test]
    async fn list_libraries_missing_project_param_returns_400_with_howto() {
        let (status, response) = list_libraries(Query(IdeLibrariesQuery {
            project: None,
            env: None,
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!response.success);
        let err = response.error.as_deref().unwrap_or_default();
        assert!(err.contains("fbuild libraries"));
    }

    #[tokio::test]
    async fn list_libraries_nonexistent_project_returns_400() {
        let (status, response) = list_libraries(Query(IdeLibrariesQuery {
            project: Some("/definitely/not/a/real/path".to_string()),
            env: None,
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!response.success);
        assert!(response.0.error.unwrap().contains("does not exist"));
    }

    #[tokio::test]
    async fn list_libraries_happy_path_returns_classified_deps() {
        let tmp = tempfile::tempdir().unwrap();
        write_project(tmp.path(), SAMPLE_INI).await;

        let (status, response) = list_libraries(Query(IdeLibrariesQuery {
            project: Some(tmp.path().display().to_string()),
            env: Some("uno".to_string()),
        }))
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(response.success);
        assert_eq!(response.environment.as_deref(), Some("uno"));
        assert_eq!(response.libraries.len(), 3);
        assert!(!response.install_state_note.is_empty());
    }
}
