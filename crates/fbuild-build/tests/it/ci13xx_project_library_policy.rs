//! Real-toolchain coverage of ordinary library filters/dependencies/dedup (#1914).
use fbuild_build::compile_backend;
use fbuild_core::Platform;
use object::{Object, ObjectSection, ObjectSymbol};
use std::path::Path;

fn fixture(duplicate: bool) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    for path in ["src", "lib/Bridge/src", "lib/Leaf/code"] {
        std::fs::create_dir_all(project.join(path)).unwrap();
    }
    std::fs::write(
        project.join("lib/Bridge/library.json"),
        r#"{"name":"Bridge","build":{"libArchive":true}}"#,
    )
    .unwrap();
    std::fs::write(
        project.join("lib/Leaf/library.json"),
        r#"{"name":"Leaf","build":{"srcDir":"code","libArchive":true,"srcFilter":["+<*>","-<poison.cpp>"]}}"#,
    )
    .unwrap();
    std::fs::write(
        project.join("lib/Bridge/src/Bridge.h"),
        "extern \"C\" int library_bridge();\n",
    )
    .unwrap();
    std::fs::write(project.join("lib/Bridge/src/bridge.cpp"),
        "#include <Bridge.h>\n#include <Leaf.h>\nstatic_assert(SHARED_LIBRARY_FLAG == 17, \"library flags missing\");\nextern \"C\" int library_bridge() { return library_leaf() + 1; }\n").unwrap();
    std::fs::write(
        project.join("lib/Leaf/code/Leaf.h"),
        "extern \"C\" int library_leaf();\n",
    )
    .unwrap();
    std::fs::write(
        project.join("lib/Leaf/code/leaf.cpp"),
        "#include <Leaf.h>\nextern \"C\" int library_leaf() { return 41; }\n",
    )
    .unwrap();
    std::fs::write(
        project.join("lib/Leaf/code/poison.cpp"),
        "#error library manifest source exclusion must be honored\n",
    )
    .unwrap();
    let explicit = if duplicate {
        "lib_deps = Leaf=symlink://lib/Leaf\n"
    } else {
        ""
    };
    std::fs::write(project.join("platformio.ini"), format!(
        "[env:ci1306]\nplatform = ci13xx\nboard = ci1306\nframework = arduino\nbuild_unflags = -flto\nbuild_flags = -fno-lto -DSHARED_LIBRARY_FLAG=17\n{explicit}")).unwrap();
    std::fs::write(project.join("src/main.cpp"),
        "#include <Arduino.h>\n#include <Bridge.h>\nvolatile int answer;\nvoid setup() { answer = library_bridge(); }\nvoid loop() {}\n").unwrap();
    tmp
}

fn inspect(result: &fbuild_build::BuildResult) {
    let elf = result.elf_path.as_ref().unwrap();
    let data = std::fs::read(elf).unwrap();
    let object = object::File::parse(data.as_slice()).unwrap();
    assert_eq!(object.architecture(), object::Architecture::Riscv32);
    for name in ["library_bridge", "library_leaf"] {
        assert!(
            object
                .symbols()
                .any(|s| s.name() == Ok(name) && s.is_definition()),
            "missing {name}"
        );
    }
    let argv: Vec<String> = serde_json::from_slice(
        &std::fs::read(elf.parent().unwrap().join("native_link_command.json")).unwrap(),
    )
    .unwrap();
    for name in ["libbridge.a", "libleaf.a"] {
        let paths: Vec<_> = argv
            .iter()
            .filter(|arg| {
                Path::new(arg)
                    .file_name()
                    .is_some_and(|file| file.to_string_lossy().eq_ignore_ascii_case(name))
            })
            .collect();
        assert_eq!(
            paths.len(),
            1,
            "each actual library must link exactly once: {argv:?}"
        );
        let bytes = std::fs::read(paths[0]).unwrap();
        let archive = object::read::archive::ArchiveFile::parse(bytes.as_slice()).unwrap();
        for member in archive.members() {
            let member = member.unwrap();
            let object = object::File::parse(member.data(bytes.as_slice()).unwrap()).unwrap();
            assert!(
                !object
                    .sections()
                    .any(|s| s.name().unwrap().starts_with(".gnu.lto_"))
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1914: actual Nuclei GCC/SDK builds bridge/leaf project libraries and repeated explicit-local route"]
async fn ci1306_project_libraries_filter_dependency_and_duplicate() {
    let backend = compile_backend::CompileBackend::start().await.unwrap();
    compile_backend::install_global(backend);
    for duplicate in [false, true] {
        let project = fixture(duplicate);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(900),
            fbuild_build::get_orchestrator(Platform::Ci13xx)
                .unwrap()
                .build(&super::ci13xx_fastled_acceptance::params(project.path())),
        )
        .await
        .expect("library policy build timeout")
        .expect("real library filtering/dependency/dedup build");
        assert!(result.success);
        inspect(&result);
    }
}
