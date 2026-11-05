//! Real native CI1306 provisioning and no-LTO compile/link acceptance (#1914).

use fbuild_build::{compile_backend, provision, provision_env};
use fbuild_core::Platform;
use fbuild_core::path::NormalizedPath;
use object::Object;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1914: downloads Nuclei GCC 9.2 and Chipintelli Arduino 1.0.18; compiles SDK and Arduino ino tabs"]
async fn ci1306_arduino_ino_preprocessing_build() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::create_dir(project.join("src")).unwrap();
    std::fs::write(project.join("platformio.ini"),
        "[env:ci1306]\nplatform = ci13xx\nboard = ci1306\nframework = arduino\nbuild_unflags = -flto\nbuild_flags = -fno-lto\n").unwrap();
    // Ordinary Arduino preprocessing must supply Arduino.h, the byte type,
    // and the later tab's prototype before this call. No handwritten wrapper.
    std::fs::write(
        project.join("src/main.ino"),
        r#"
volatile unsigned long ino_observed;
void setup() { pinMode(2, OUTPUT); ino_observed = sketchValue(7); }
void loop() { digitalWrite(2, ino_observed++ & 1); }
"#,
    )
    .unwrap();
    std::fs::write(
        project.join("src/z_helper.ino"),
        "unsigned long sketchValue(byte value) { return millis() + value; }\n",
    )
    .unwrap();
    let backend = compile_backend::CompileBackend::start().await.unwrap();
    compile_backend::install_global(backend);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(900),
        fbuild_build::get_orchestrator(Platform::Ci13xx)
            .unwrap()
            .build(&super::ci13xx_fastled_acceptance::params(project)),
    )
    .await
    .expect("ino build timed out")
    .expect("native CI1306 must preprocess and build ino tabs");
    assert!(result.success);
    let bytes = std::fs::read(result.elf_path.as_ref().unwrap()).unwrap();
    let elf = object::File::parse(bytes.as_slice()).unwrap();
    assert_eq!(elf.architecture(), object::Architecture::Riscv32);
    let commands: Vec<serde_json::Value> = serde_json::from_slice(
        &std::fs::read(result.compile_database_path.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    let generated: Vec<_> = commands
        .iter()
        .filter_map(|row| row["file"].as_str())
        .filter(|file| file.ends_with(".ino.cpp"))
        .collect();
    assert_eq!(
        generated.len(),
        1,
        "ino tabs must combine into one compiled unit"
    );
    let text = std::fs::read_to_string(generated[0]).unwrap();
    assert!(
        text.contains("Arduino.h"),
        "shared preprocessor must inject Arduino header"
    );
    assert!(text.contains("sketchValue"));
    assert!(
        text.contains("z_helper.ino"),
        "retain source-tab line mapping"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1914: downloads Nuclei GCC 9.2 and Chipintelli Arduino 1.0.18 (~190 MB), compiles vendor SDK"]
async fn ci1306_native_sdk_no_lto_build() {
    let tmp = native_sdk_fixture();
    let project = tmp.path();
    let report = provision_env(project, "ci1306", provision::ProvisionMode::Install)
        .await
        .expect("native CI13xx provisioning must succeed");
    assert!(!report.failed(), "{report:?}");
    assert!(!report.needs_fetch(), "{report:?}");
    for kind in [
        provision::PackageKind::Toolchain,
        provision::PackageKind::Framework,
    ] {
        assert!(
            report.packages.iter().any(|package| package.kind == kind),
            "native provisioning must report {kind:?}: {report:?}"
        );
    }
    let backend = compile_backend::CompileBackend::start().await.unwrap();
    compile_backend::install_global(backend);
    let params = super::ci13xx_fastled_acceptance::params(project);
    let orchestrator =
        fbuild_build::get_orchestrator(Platform::Ci13xx).expect("native CI13xx dispatch");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(900),
        orchestrator.build(&params),
    )
    .await
    .expect("CI1306 build timed out")
    .expect("native CI1306 SDK/core compile and link must succeed");
    inspect_native_sdk(&result);
}

fn native_sdk_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::create_dir(project.join("src")).unwrap();
    std::fs::create_dir(project.join("boards")).unwrap();
    // Project-local board support is an existing production resolution path.
    // No USB identity or unrelated platform is needed for a native build.
    std::fs::write(
        project.join("boards/ci1306.json"),
        r#"{
      "id":"ci1306", "name":"Chipintelli CI1306", "platform":"ci13xx",
      "vendor":"Chipintelli", "frameworks":["arduino"],
      "build":{"mcu":"ci1306","core":"chipintelli","variant":"ci1306",
        "f_cpu":"246000000L","march":"rv32imafc","mabi":"ilp32f"},
      "upload":{"maximum_size":382121,"maximum_ram_size":532480}
    }"#,
    )
    .unwrap();
    std::fs::write(project.join("platformio.ini"),
        "[env:ci1306]\nplatform = ci13xx\nboard = ci1306\nframework = arduino\nbuild_unflags = -flto\nbuild_flags = -fno-lto\n").unwrap();
    std::fs::write(
        project.join("src/main.cpp"),
        r#"
#include <Arduino.h>
#ifndef ARDUINO_ARCH_CI13XX
#error CI13XX architecture is required
#endif
static_assert(sizeof(void*) == 4, "real 32-bit target required");
static_assert(F_CPU == 246000000L, "CI1306 external crystal clock required");
volatile unsigned long ci13xx_acceptance;
void setup() { pinMode(2, OUTPUT); ci13xx_acceptance = millis(); }
void loop() { digitalWrite(2, ci13xx_acceptance++ & 1); }
"#,
    )
    .unwrap();

    tmp
}

fn inspect_native_sdk(result: &fbuild_build::BuildResult) {
    assert!(result.success);
    let bytes = std::fs::read(result.elf_path.as_ref().expect("linked ELF")).unwrap();
    let elf = object::File::parse(bytes.as_slice()).expect("valid target ELF");
    assert_eq!(elf.architecture(), object::Architecture::Riscv32);
    assert!(!elf.is_64());
    assert!(
        result
            .size_info
            .as_ref()
            .expect("target size report")
            .total_flash
            > 0
    );
    let commands: Vec<serde_json::Value> = serde_json::from_slice(
        &std::fs::read(
            result
                .compile_database_path
                .as_ref()
                .expect("actual core/SDK compile database"),
        )
        .unwrap(),
    )
    .unwrap();
    for marker in ["/src/main.cpp", "/cores/chipintelli/", "/tools/sdk/src/"] {
        let matching: Vec<_> = commands
            .iter()
            .filter(|row| {
                NormalizedPath::new(row["file"].as_str().unwrap_or(""))
                    .display_slash()
                    .contains(marker)
            })
            .collect();
        assert!(
            !matching.is_empty(),
            "missing actual compile records for {marker}"
        );
        for row in matching {
            let command = row["command"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    row["arguments"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap())
                        .collect::<Vec<_>>()
                        .join(" ")
                });
            assert!(command.contains("riscv-nuclei-elf-"), "{command}");
            assert!(command.contains("-fno-lto"), "{command}");
            assert!(
                !command.split_whitespace().any(|arg| arg == "-flto"),
                "{command}"
            );
        }
    }
}
