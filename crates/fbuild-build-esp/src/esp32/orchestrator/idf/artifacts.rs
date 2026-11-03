//! Adapt IDF artifacts to the existing fbuild size/deploy contract.

use std::collections::BTreeMap;
use std::path::Path;

use fbuild_core::path::NormalizedPath;

use fbuild_core::{FbuildError, Result};
use fbuild_packages::Toolchain;
use serde::Deserialize;

#[derive(Deserialize)]
struct Description {
    app_elf: String,
    app_bin: String,
}

#[derive(Deserialize)]
struct FlashArguments {
    flash_files: BTreeMap<String, String>,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|e| FbuildError::BuildFailed(format!("{}: {e}", path.display())))
}

fn artifact(build: &Path, name: &str) -> Result<NormalizedPath> {
    let path = NormalizedPath::new(build.join(name).canonicalize()?);
    let root = NormalizedPath::new(build.canonicalize()?);
    if path.relative_to(&root).is_none() || !path.is_file() {
        return Err(FbuildError::BuildFailed(format!(
            "invalid IDF artifact: {name}"
        )));
    }
    Ok(path)
}

pub(super) async fn export(
    ctx: &crate::pipeline::BuildContext,
    build: &Path,
    toolchain: &fbuild_packages::toolchain::Esp32Toolchain,
) -> Result<crate::linker::LinkResult> {
    let description: Description = read_json(&build.join("project_description.json"))?;
    let flash: FlashArguments = read_json(&build.join("flasher_args.json"))?;
    let mcu = super::super::super::mcu_config::get_mcu_config(&ctx.board.mcu)?;
    let expected = [
        (mcu.bootloader_offset(), "bootloader.bin"),
        (mcu.partitions_offset(), "partitions.bin"),
        (mcu.firmware_offset(), "firmware.bin"),
    ];
    // Existing deployers use MCU offsets. Reject layouts needing more images or
    // different offsets rather than silently flashing a valid image incorrectly.
    if flash.flash_files.len() != expected.len() {
        return Err(FbuildError::ConfigError(
            "ESP-IDF flash layout requires additional images; this slice supports bootloader, partition table and app only".into(),
        ));
    }
    for (offset, destination) in expected {
        let source = flash.flash_files.iter().find(|(key, _)| {
            u64::from_str_radix(key.trim_start_matches("0x"), 16).ok()
                == u64::from_str_radix(offset.trim_start_matches("0x"), 16).ok()
        }).map(|(_, value)| value).ok_or_else(|| FbuildError::ConfigError(format!(
            "ESP-IDF flash layout is unsupported: existing deployer requires {destination} at {offset}",
        )))?;
        if destination == "firmware.bin"
            && artifact(build, source)? != artifact(build, &description.app_bin)?
        {
            return Err(FbuildError::BuildFailed(
                "IDF app image does not match flash manifest".into(),
            ));
        }
        std::fs::copy(artifact(build, source)?, ctx.build_dir.join(destination))?;
    }
    let elf = ctx.build_dir.join("firmware.elf");
    std::fs::copy(artifact(build, &description.app_elf)?, &elf)?;
    std::fs::copy(
        build.join("compile_commands.json"),
        ctx.build_dir.join("compile_commands.json"),
    )?;
    let size = super::super::super::size_report::esp32_report_size(
        &toolchain.get_size_path(),
        &elf,
        ctx.board.max_flash,
        ctx.board.max_ram,
    )
    .await?;
    Ok(crate::linker::LinkResult {
        success: true,
        stdout: String::new(),
        stderr: String::new(),
        elf_path: Some(elf),
        bin_path: Some(ctx.build_dir.join("firmware.bin")),
        hex_path: None,
        size_info: Some(size),
        symbol_map: None,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn artifacts_must_be_files_inside_the_build_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build = tmp.path().join("build");
        std::fs::create_dir(&build).unwrap();
        std::fs::write(build.join("app.bin"), "firmware").unwrap();
        std::fs::write(tmp.path().join("outside.bin"), "outside").unwrap();
        assert_eq!(
            super::artifact(&build, "app.bin").unwrap().as_path(),
            build.join("app.bin").canonicalize().unwrap()
        );
        for name in ["../outside.bin", "."] {
            assert!(
                super::artifact(&build, name)
                    .unwrap_err()
                    .to_string()
                    .contains("invalid IDF artifact")
            );
        }
    }
}
