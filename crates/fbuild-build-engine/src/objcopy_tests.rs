//! The objcopy argv `LinkerBase` builds from an `ObjcopyConfig`.

use std::path::Path;

use crate::compiler::ObjcopyConfig;
use crate::linker::LinkerBase;

fn argv(config: &ObjcopyConfig) -> Vec<String> {
    let out = LinkerBase::objcopy_output_path(Path::new("out"), &config.output_format);
    LinkerBase::objcopy_args(
        Path::new("objcopy"),
        Path::new("firmware.elf"),
        &out,
        config,
    )
}

#[test]
fn ihex_removes_listed_sections_and_writes_a_hex() {
    let config = ObjcopyConfig {
        output_format: "ihex".to_string(),
        remove_sections: vec![".eeprom".to_string()],
    };
    let out = Path::new("out").join("firmware.hex");
    assert_eq!(
        argv(&config),
        [
            "objcopy",
            "-O",
            "ihex",
            "-R",
            ".eeprom",
            "firmware.elf",
            &out.to_string_lossy(),
        ]
    );
}

#[test]
fn binary_writes_a_bin() {
    let config = ObjcopyConfig {
        output_format: "binary".to_string(),
        remove_sections: Vec::new(),
    };
    let out = Path::new("out").join("firmware.bin");
    assert_eq!(
        argv(&config),
        [
            "objcopy",
            "-O",
            "binary",
            "firmware.elf",
            &out.to_string_lossy()
        ]
    );
}
