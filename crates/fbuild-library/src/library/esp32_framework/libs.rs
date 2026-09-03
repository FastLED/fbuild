//! Logic for downloading and installing the ESP-IDF SDK libs and MCU skeleton libs.

use std::path::{Path, PathBuf};

use super::Esp32Framework;
use super::fs_utils::copy_dir_recursive;

/// Archive whose presence proves a per-MCU SDK tree carries real libraries
/// and not just the partial directory some core archives ship.
const FREERTOS_ARCHIVE: &str = "libfreertos.a";

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
        && has_freertos_archive(mcu_dir)
}

/// Locate `libfreertos.a` inside an installed per-MCU SDK tree.
///
/// Most MCUs put every archive in `lib/`. ESP32-S3 does not: its FreeRTOS
/// build differs per flash/PSRAM mode, so `libfreertos.a` ships **only**
/// under the memory-type variant dirs (`dio_opi`, `qio_qspi`, ...) while
/// `lib/` holds the other 165 archives. Requiring `lib/libfreertos.a`
/// therefore judged a complete S3 install incomplete forever, and
/// [`Esp32Framework::ensure_libs`] re-downloaded and re-extracted the
/// 298 MB SDK archive on *every* build — 132 s of a 136 s no-op
/// (FastLED/fbuild#1411).
fn has_freertos_archive(mcu_dir: &Path) -> bool {
    if mcu_dir.join("lib").join(FREERTOS_ARCHIVE).exists() {
        return true;
    }
    // Only one level deep: the variant dirs sit directly under the MCU dir.
    std::fs::read_dir(mcu_dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| entry.path().join(FREERTOS_ARCHIVE).exists())
        })
        .unwrap_or(false)
}

/// Merge a requested MCU directory from a skeleton archive, regardless of a
/// package wrapper directory added by the archive producer. Returns whether
/// the archive actually contained the requested MCU payload.
fn merge_skeleton_mcu_entries(
    temp_dir: &Path,
    tools_dir: &Path,
    mcu: &str,
) -> fbuild_core::Result<bool> {
    fn visit(dir: &Path, destination: &Path, mcu: &str) -> fbuild_core::Result<bool> {
        let mut found = false;
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if entry.file_name() == mcu {
                copy_dir_recursive(&path, destination)?;
                found = true;
            } else {
                found |= visit(&path, destination, mcu)?;
            }
        }
        Ok(found)
    }

    visit(temp_dir, &tools_dir.join(NEW_SDK_LAYOUT).join(mcu), mcu)
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
    pub async fn ensure_libs(&self, libs_url: &str, mcu: &str) -> fbuild_core::Result<()> {
        let root = self.resolved_dir();
        let tools_dir = root.join("tools");

        // Already have SDK libs? Check both old (sdk/) and new
        // (esp32-arduino-libs/) layouts.
        for mcu_dir in mcu_sdk_dir_candidates(&tools_dir, mcu) {
            if mcu_sdk_complete(&mcu_dir) {
                return Ok(());
            }
        }

        // A present-but-incomplete MCU tree means the ~300 MB archive is
        // about to be fetched and extracted again. That is correct on a
        // genuinely partial install and catastrophic when the completion
        // check is simply wrong about the layout — the silent form of this
        // cost every build 132 s on ESP32-S3 (FastLED/fbuild#1411). Say so.
        for mcu_dir in mcu_sdk_dir_candidates(&tools_dir, mcu) {
            if mcu_dir.exists() {
                tracing::warn!(
                    "{} SDK dir {} exists but is incomplete; re-downloading the SDK libs archive",
                    mcu,
                    mcu_dir.display()
                );
            }
        }

        std::fs::create_dir_all(&tools_dir)?;

        // Check for already-downloaded archive (skip re-download)
        let archive_filename = libs_url.rsplit('/').next().unwrap_or("libs.tar.xz");
        let archive_path = tools_dir.join(archive_filename);

        if !archive_path.exists() {
            tracing::info!("downloading ESP32 SDK libs");
            crate::downloader::download_file(libs_url, &tools_dir).await?;
        }

        // Extract to a short temp path to avoid Windows MAX_PATH (260 char) limit.
        // Rooted under `~/.fbuild/{dev|prod}/tmp/esp32-framework/` so the
        // extract scratch dir is reachable from a single user-visible
        // location — FastLED/fbuild#844 bridge pair 10.
        let temp_dir = tempfile::Builder::new()
            .prefix("fbuild_sdk_")
            .tempdir_in(fbuild_paths::temp_subdir("esp32-framework"))?;

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
    pub async fn ensure_mcu_libs(&self, libs_url: &str, mcu: &str) -> fbuild_core::Result<()> {
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
            crate::downloader::download_file(libs_url, &tools_dir).await?;
        }

        // Rooted under `~/.fbuild/{dev|prod}/tmp/esp32-framework/` —
        // FastLED/fbuild#844 bridge pair 10.
        let temp_dir = tempfile::Builder::new()
            .prefix("fbuild_skel_")
            .tempdir_in(fbuild_paths::temp_subdir("esp32-framework"))?;

        tracing::info!("extracting {} skeleton libs", mcu);
        crate::extractor::extract(&archive_path, temp_dir.path())?;
        let _ = std::fs::remove_file(&archive_path);

        // Skeleton archives such as c2_arduino_compile_skeleton.zip extract as
        // a direct esp32c2/ directory. Merge direct MCU roots into the new SDK
        // layout so sdk_mcu_dir() finds the completed tree.
        let merged_skeleton = merge_skeleton_mcu_entries(temp_dir.path(), &tools_dir, mcu)?;

        for mcu_dir in mcu_sdk_dir_candidates(&tools_dir, mcu) {
            if mcu_sdk_complete(&mcu_dir) || merged_skeleton {
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

    /// ESP32-S3 is the one MCU whose `esp32-arduino-libs` tree ships
    /// `libfreertos.a` **only** under the per-memory-type variant dirs
    /// (`dio_opi`, `qio_qspi`, ...) — `lib/` holds the other 165 archives
    /// but not that one. Requiring `lib/libfreertos.a` therefore judged
    /// a fully-installed S3 SDK incomplete on every build, and
    /// `ensure_libs` re-downloaded + re-extracted the 298 MB archive each
    /// time: 132 s of a 136 s no-op build (FastLED/fbuild#1411).
    #[test]
    fn mcu_sdk_complete_accepts_memory_type_variant_freertos_lib() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mcu_dir = tmp.path().join("esp32s3");

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
        // 165 archives land in `lib/`, but not libfreertos.a.
        write(&mcu_dir.join("lib").join("libdriver.a"), "");
        assert!(!mcu_sdk_complete(&mcu_dir));

        write(&mcu_dir.join("qio_qspi").join("libfreertos.a"), "");
        assert!(mcu_sdk_complete(&mcu_dir));
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
    fn skeleton_merge_finds_mcu_under_package_wrapper() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tools_dir = tmp.path().join("tools");
        let archive = tmp.path().join("archive");
        let source_mcu = archive.join("package-wrapper").join("esp32c2");
        write(&source_mcu.join("lib").join("libfreertos.a"), "");

        assert!(merge_skeleton_mcu_entries(&archive, &tools_dir, "esp32c2").unwrap());
        assert!(
            tools_dir
                .join(NEW_SDK_LAYOUT)
                .join("esp32c2")
                .join("lib")
                .join("libfreertos.a")
                .is_file()
        );
    }

    #[test]
    fn mcu_sdk_completion_requires_the_requested_mcu() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sdk_dir = tmp.path().join(NEW_SDK_LAYOUT);
        seed_complete_mcu_sdk(&sdk_dir.join("esp32c3"));

        assert!(mcu_sdk_complete(&sdk_dir.join("esp32c3")));
        assert!(!mcu_sdk_complete(&sdk_dir.join("esp32c2")));
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

        assert!(
            mcu_dir
                .join("include")
                .join("hal")
                .join("include")
                .join("hal")
                .join("touch_sensor_legacy_types.h")
                .exists()
        );
    }

    #[test]
    fn patch_mcu_compatibility_leaves_other_mcus_untouched() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mcu_dir = tmp.path().join("esp32c3");

        patch_mcu_compatibility(&mcu_dir, "esp32c3").unwrap();

        assert!(!mcu_dir.exists());
    }
}
