//! Logic for downloading and installing the ESP-IDF SDK libs and MCU skeleton libs.

use super::fs_utils::copy_dir_recursive;
use super::Esp32Framework;

impl Esp32Framework {
    /// Ensure the SDK libs are downloaded and extracted into the framework's `tools/` dir.
    pub fn ensure_libs(&self, libs_url: &str) -> fbuild_core::Result<()> {
        let root = self.resolved_dir();
        let tools_dir = root.join("tools");

        // Already have SDK libs? Check both old (sdk/) and new (esp32-arduino-libs/) layouts
        for dir_name in &["esp32-arduino-libs", "sdk"] {
            let sdk_dir = tools_dir.join(dir_name);
            if sdk_dir.exists() && sdk_dir.is_dir() {
                if let Ok(mut entries) = std::fs::read_dir(&sdk_dir) {
                    if entries.next().is_some() {
                        return Ok(());
                    }
                }
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
        // Then rename (atomic on same filesystem) to final location.
        let temp_dir = std::env::temp_dir().join(format!("fbuild_sdk_{}", std::process::id()));
        if temp_dir.exists() {
            let _ = std::fs::remove_dir_all(&temp_dir);
        }
        std::fs::create_dir_all(&temp_dir)?;

        tracing::info!(
            "extracting ESP32 SDK libs ({} MB)",
            archive_path
                .metadata()
                .map(|m| m.len() / 1_000_000)
                .unwrap_or(0)
        );
        crate::extractor::extract(&archive_path, &temp_dir)?;
        let _ = std::fs::remove_file(&archive_path);

        // Move extracted content to final tools/ dir (same filesystem = fast rename)
        if let Ok(entries) = std::fs::read_dir(&temp_dir) {
            for entry in entries.flatten() {
                let src = entry.path();
                let dest = tools_dir.join(entry.file_name());
                if dest.exists() {
                    let _ = std::fs::remove_dir_all(&dest);
                }
                if std::fs::rename(&src, &dest).is_err() {
                    copy_dir_recursive(&src, &dest)?;
                }
            }
        }
        let _ = std::fs::remove_dir_all(&temp_dir);

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

        // Already have this MCU's SDK? Check both layouts.
        for dir_name in &["esp32-arduino-libs", "sdk"] {
            let mcu_dir = tools_dir.join(dir_name).join(mcu);
            if mcu_dir.exists() && mcu_dir.is_dir() {
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

        let temp_dir = std::env::temp_dir().join(format!("fbuild_skel_{}", std::process::id()));
        if temp_dir.exists() {
            let _ = std::fs::remove_dir_all(&temp_dir);
        }
        std::fs::create_dir_all(&temp_dir)?;

        tracing::info!("extracting {} skeleton libs", mcu);
        crate::extractor::extract(&archive_path, &temp_dir)?;
        let _ = std::fs::remove_file(&archive_path);

        // Merge into tools/ — use copy_dir_recursive so existing MCU dirs are preserved.
        if let Ok(entries) = std::fs::read_dir(&temp_dir) {
            for entry in entries.flatten() {
                let src = entry.path();
                let dest = tools_dir.join(entry.file_name());
                if src.is_dir() {
                    copy_dir_recursive(&src, &dest)?;
                } else if std::fs::rename(&src, &dest).is_err() {
                    std::fs::copy(&src, &dest)?;
                }
            }
        }
        let _ = std::fs::remove_dir_all(&temp_dir);

        tracing::info!("{} skeleton libs installed", mcu);
        Ok(())
    }
}
