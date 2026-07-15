//! Managed picotool conversion with version-safe feature probing.

use std::path::Path;

use fbuild_core::Result;

pub(super) async fn convert_elf_to_uf2(
    picotool: &Path,
    elf: &Path,
    uf2: &Path,
    mcu: &str,
) -> Result<()> {
    let family = if mcu.to_ascii_lowercase().starts_with("rp2350") {
        "rp2350-arm-s"
    } else {
        "rp2040"
    };
    let supports_platform = picotool_supports_platform_validation(picotool).await;
    let args = uf2_conversion_args(picotool, elf, uf2, family, supports_platform);
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = fbuild_core::subprocess::run_command(
        &args_ref,
        None,
        None,
        Some(std::time::Duration::from_secs(30)),
    )
    .await?;
    if !output.success() {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "managed picotool could not convert {} to {} for {family}: {}{}{}",
            elf.display(),
            uf2.display(),
            output.stderr.trim(),
            if output.stderr.is_empty() || output.stdout.is_empty() {
                ""
            } else {
                "\n"
            },
            output.stdout.trim()
        )));
    }
    if !uf2.is_file() {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "managed picotool reported success without creating {}",
            uf2.display()
        )));
    }
    Ok(())
}

fn uf2_conversion_args(
    picotool: &Path,
    elf: &Path,
    uf2: &Path,
    family: &str,
    supports_platform: bool,
) -> Vec<String> {
    let mut args = vec![
        picotool.to_string_lossy().to_string(),
        "uf2".to_string(),
        "convert".to_string(),
        elf.to_string_lossy().to_string(),
        uf2.to_string_lossy().to_string(),
        "--family".to_string(),
        family.to_string(),
    ];
    if family.starts_with("rp2350") {
        args.push("--abs-block".to_string());
    }
    if supports_platform {
        args.extend([
            "--platform".to_string(),
            if family.starts_with("rp2350") {
                "rp2350".to_string()
            } else {
                "rp2040".to_string()
            },
        ]);
    }
    args
}

async fn picotool_supports_platform_validation(picotool: &Path) -> bool {
    let executable = picotool.to_string_lossy().to_string();
    let args = [executable.as_str(), "help", "uf2", "convert"];
    fbuild_core::subprocess::run_command(
        &args,
        None,
        None,
        Some(std::time::Duration::from_secs(5)),
    )
    .await
    .is_ok_and(|output| {
        output.success()
            && (output.stdout.contains("--platform") || output.stderr.contains("--platform"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_picotool_args_omit_unsupported_platform_flag() {
        let args = uf2_conversion_args(
            Path::new("picotool"),
            Path::new("firmware.elf"),
            Path::new("firmware.uf2"),
            "rp2040",
            false,
        );
        assert!(!args.iter().any(|arg| arg == "--platform"));
    }

    #[test]
    fn newer_picotool_gets_platform_validation_and_rp2350_errata_block() {
        let args = uf2_conversion_args(
            Path::new("picotool"),
            Path::new("firmware.elf"),
            Path::new("firmware.uf2"),
            "rp2350-arm-s",
            true,
        );
        assert!(args.windows(2).any(|pair| pair == ["--platform", "rp2350"]));
        assert!(args.iter().any(|arg| arg == "--abs-block"));
    }
}
