//! Logic for downloading and installing the ESP-IDF SDK libs and MCU skeleton libs.

use std::path::{Path, PathBuf};

use super::fs_utils::copy_dir_recursive;
use super::Esp32Framework;

const NEW_SDK_LAYOUT: &str = "esp32-arduino-libs";
const OLD_SDK_LAYOUT: &str = "sdk";

fn looks_like_mcu_sdk_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    path.is_dir() && (name == "hosted" || name.starts_with("esp32"))
}

fn merge_sdk_archive_entries(temp_dir: &Path, tools_dir: &Path) -> fbuild_core::Result<()> {
    let new_layout_dir = tools_dir.join(NEW_SDK_LAYOUT);

    if let Ok(entries) = std::fs::read_dir(temp_dir) {
        for entry in entries.flatten() {
            let src = entry.path();
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();

            if src.is_dir() && (name == NEW_SDK_LAYOUT || name == OLD_SDK_LAYOUT) {
                copy_dir_recursive(&src, &tools_dir.join(&file_name))?;
            } else if looks_like_mcu_sdk_dir(&src) {
                copy_dir_recursive(&src, &new_layout_dir.join(&file_name))?;
            } else if src.is_file() {
                // Package metadata is useful for diagnostics but not part of the
                // SDK include search. Keep it next to the merged SDK layout.
                std::fs::create_dir_all(&new_layout_dir)?;
                std::fs::copy(&src, new_layout_dir.join(&file_name))?;
            }
        }
    }

    Ok(())
}

fn mcu_sdk_dir_candidates(tools_dir: &Path, mcu: &str) -> [PathBuf; 2] {
    [
        tools_dir.join(NEW_SDK_LAYOUT).join(mcu),
        tools_dir.join(OLD_SDK_LAYOUT).join(mcu),
    ]
}

fn mcu_sdk_complete(mcu_dir: &Path) -> bool {
    mcu_dir
        .join("include")
        .join("freertos")
        .join("FreeRTOS-Kernel")
        .join("include")
        .join("freertos")
        .join("FreeRTOS.h")
        .exists()
        && mcu_dir.join("flags").join("includes").exists()
        && mcu_dir.join("lib").join("libfreertos.a").exists()
}

fn sdk_layout_has_complete_mcu_sdk(sdk_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(sdk_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        looks_like_mcu_sdk_dir(&path) && mcu_sdk_complete(&path)
    })
}

fn patch_mcu_compatibility(mcu_dir: &Path, mcu: &str) -> fbuild_core::Result<()> {
    if mcu != "esp32c2" {
        return Ok(());
    }

    let touch_header = mcu_dir
        .join("include")
        .join("hal")
        .join("include")
        .join("hal")
        .join("touch_sensor_legacy_types.h");

    if touch_header.exists() {
        return Ok(());
    }

    if let Some(parent) = touch_header.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &touch_header,
        "// ESP32-C2 has no touch sensor peripheral; Arduino-ESP32 3.3.x includes this unconditionally.\n",
    )?;
    Ok(())
}

impl Esp32Framework {
    /// Ensure the SDK libs are downloaded and extracted into the framework's `tools/` dir.
    pub fn ensure_libs(&self, libs_url: &str) -> fbuild_core::Result<()> {
        let root = self.resolved_dir();
        let tools_dir = root.join("tools");

        // Already have SDK libs? Check both old (sdk/) and new
        // (esp32-arduino-libs/) layouts.
        for dir_name in &[NEW_SDK_LAYOUT, OLD_SDK_LAYOUT] {
            let sdk_dir = tools_dir.join(dir_name);
            if sdk_dir.exists() && sdk_layout_has_complete_mcu_sdk(&sdk_dir) {
                return Ok(());
            }
        }

        std::fs::create_dir_all(&tools_dir)?;

        // Check for already-downloaded archive (skip re-download)
        let archive_filename = libs_url.rsplit('/').next().unwrap_or("libs.tar.xz");
        let archive_path = tools_dir.join(archive_filename);

        if !archive_path.exists() {
            tracing::info!("downloading ESP32 SDK libs");
            let rt = tokio::runtime::Handle::try_current().ok();
            if let Some(handle) = rt {
                handle.block_on(crate::downloader::download_file(libs_url, &tools_dir))?;
            } else {
                let rt = tokio::runtime::Runtime::new().map_err(|e| {
                    fbuild_core::FbuildError::PackageError(format!(
                        "failed to create tokio runtime: {}",
                        e
                    ))
                })?;
                rt.block_on(crate::downloader::download_file(libs_url, &tools_dir))?;
            }
        }

        // Extract to a short temp path to avoid Windows MAX_PATH (260 char) limit.
        let temp_dir = tempfile::Builder::new().prefix("fbuild_sdk_").tempdir()?;

        tracing::info!(
            "extracting ESP32 SDK libs ({} MB)",
            archive_path
                .metadata()
                .map(|m| m.len() / 1_000_000)
                .unwrap_or(0)
        );
        crate::extractor::extract(&archive_path, temp_dir.path())?;
        let _ = std::fs::remove_file(&archive_path);

        merge_sdk_archive_entries(temp_dir.path(), &tools_dir)?;

        tracing::info!("ESP32 SDK libs installed");
        Ok(())
    }

    /// Ensure MCU-specific skeleton libs are downloaded and merged into the framework's `tools/` dir.
    ///
    /// Some MCUs (e.g. ESP32-C2, ESP32-C61) ship their SDK libs in a separate
    /// skeleton package rather than the main `framework-arduinoespressif32-libs`.
    /// This merges the skeleton into the existing `tools/` directory without
    /// clobbering other MCU subdirs.
    pub fn ensure_mcu_libs(&self, libs_url: &str, mcu: &str) -> fbuild_core::Result<()> {
        let root = self.resolved_dir();
        let tools_dir = root.join("tools");

        // The Arduino core archive can include a partial MCU SDK directory.
        // ESP32-C2 has one such tree in Arduino-ESP32 3.3.x, but it lacks
        // FreeRTOS headers, so existence alone is not a valid completion test.
        for mcu_dir in mcu_sdk_dir_candidates(&tools_dir, mcu) {
            if mcu_sdk_complete(&mcu_dir) {
                patch_mcu_compatibility(&mcu_dir, mcu)?;
                return Ok(());
            }
        }

        std::fs::create_dir_all(&tools_dir)?;

        let archive_filename = libs_url.rsplit('/').next().unwrap_or("skeleton.zip");
        let archive_path = tools_dir.join(archive_filename);

        if !archive_path.exists() {
            tracing::info!("downloading {} skeleton libs", mcu);
            let rt = tokio::runtime::Handle::try_current().ok();
            if let Some(handle) = rt {
                handle.block_on(crate::downloader::download_file(libs_url, &tools_dir))?;
            } else {
                let rt = tokio::runtime::Runtime::new().map_err(|e| {
                    fbuild_core::FbuildError::PackageError(format!(
                        "failed to create tokio runtime: {}",
                        e
                    ))
                })?;
                rt.block_on(crate::downloader::download_file(libs_url, &tools_dir))?;
            }
        }

        let temp_dir = tempfile::Builder::new().prefix("fbuild_skel_").tempdir()?;

        tracing::info!("extracting {} skeleton libs", mcu);
        crate::extractor::extract(&archive_path, temp_dir.path())?;
        let _ = std::fs::remove_file(&archive_path);

        // Skeleton archives such as c2_arduino_compile_skeleton.zip extract as
        // a direct esp32c2/ directory. Merge direct MCU roots into the new SDK
        // layout so sdk_mcu_dir() finds the completed tree.
        merge_sdk_archive_entries(temp_dir.path(), &tools_dir)?;

        for mcu_dir in mcu_sdk_dir_candidates(&tools_dir, mcu) {
            if mcu_sdk_complete(&mcu_dir) {
                patch_mcu_compatibility(&mcu_dir, mcu)?;
                tracing::info!("{} skeleton libs installed", mcu);
                return Ok(());
            }
        }

        Err(fbuild_core::FbuildError::PackageError(format!(
            "{} skeleton libs were extracted but required FreeRTOS SDK files are still missing",
            mcu
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn seed_complete_mcu_sdk(mcu_dir: &Path) {
        write(
            &mcu_dir
                .join("include")
                .join("freertos")
                .join("FreeRTOS-Kernel")
                .join("include")
                .join("freertos")
                .join("FreeRTOS.h"),
            "",
        );
        write(&mcu_dir.join("flags").join("includes"), "");
        write(&mcu_dir.join("lib").join("libfreertos.a"), "");
    }

    #[test]
    fn mcu_sdk_complete_requires_freertos_kernel_header() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mcu_dir = tmp.path().join("esp32c2");

        write(&mcu_dir.join("flags").join("includes"), "");
        write(&mcu_dir.join("lib").join("libfreertos.a"), "");
        assert!(!mcu_sdk_complete(&mcu_dir));

        seed_complete_mcu_sdk(&mcu_dir);
        assert!(mcu_sdk_complete(&mcu_dir));
    }

    #[test]
    fn sdk_layout_completion_ignores_metadata_only_layout() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sdk_dir = tmp.path().join(NEW_SDK_LAYOUT);
        write(&sdk_dir.join("package.json"), "{}");
        assert!(!sdk_layout_has_complete_mcu_sdk(&sdk_dir));

        std::fs::create_dir_all(sdk_dir.join("esp32c2")).unwrap();
        assert!(!sdk_layout_has_complete_mcu_sdk(&sdk_dir));

        seed_complete_mcu_sdk(&sdk_dir.join("esp32c2"));
        assert!(sdk_layout_has_complete_mcu_sdk(&sdk_dir));
    }

    #[test]
    fn merge_direct_mcu_archive_entries_under_new_sdk_layout() {
        let tmp = tempfile::TempDir::new().unwrap();
        let archive = tmp.path().join("archive");
        let tools = tmp.path().join("tools");
        let source_mcu = archive.join("esp32c2");
        seed_complete_mcu_sdk(&source_mcu);

        merge_sdk_archive_entries(&archive, &tools).unwrap();

        let merged = tools.join(NEW_SDK_LAYOUT).join("esp32c2");
        assert!(mcu_sdk_complete(&merged));
        assert!(!tools.join("esp32c2").exists());
    }

    #[test]
    fn patch_esp32c2_missing_touch_header_creates_compat_header() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mcu_dir = tmp.path().join("esp32c2");

        patch_mcu_compatibility(&mcu_dir, "esp32c2").unwrap();

        assert!(mcu_dir
            .join("include")
            .join("hal")
            .join("include")
            .join("hal")
            .join("touch_sensor_legacy_types.h")
            .exists());
    }

    #[test]
    fn patch_mcu_compatibility_leaves_other_mcus_untouched() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mcu_dir = tmp.path().join("esp32c3");

        patch_mcu_compatibility(&mcu_dir, "esp32c3").unwrap();

        assert!(!mcu_dir.exists());
    }
}
