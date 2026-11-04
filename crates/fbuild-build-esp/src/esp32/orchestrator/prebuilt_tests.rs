use super::*;
use fbuild_core::{
    BuildProfile,
    content_manifest::{SIDECAR, hash_tree},
};
use fbuild_packages::content_manifest::write_manifest;

fn params(root: &Path) -> BuildParams {
    BuildParams {
        prebuilt: None,
        project_dir: root.join("project"),
        env_name: "esp32c3".into(),
        clean_all: false,
        clean_only: false,
        clean: false,
        profile: BuildProfile::Release,
        build_dir: root.join("build"),
        verbose: false,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: true,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: vec![],
        watch_set_cache: None,
        bloat_analysis: false,
        shrink: Default::default(),
        caller_path: None,
    }
}

#[test]
#[cfg(unix)]
fn manifest_pinned_plan_and_registry_precedence() {
    let root = tempfile::tempdir().unwrap();
    let fw = root.path().join("framework");
    let tc = root.path().join("toolchain");
    std::fs::create_dir_all(&fw).unwrap();
    std::fs::create_dir_all(&tc).unwrap();
    std::fs::write(fw.join("core.c"), "int core(void) { return 1; }").unwrap();
    std::fs::write(tc.join("gcc"), "compiler fixture").unwrap();
    let framework = PackageInfo {
        name: "arduino-esp32".into(),
        version: "3.3.12".into(),
        url: "https://example.invalid/core.zip".into(),
        install_path: fw.clone(),
        checksum: None,
        content_id: None,
        installed_bytes: None,
    };
    let toolchain = PackageInfo {
        name: "gcc".into(),
        install_path: tc.clone(),
        ..framework.clone()
    };
    for info in [&framework, &toolchain] {
        write_manifest(
            &info.install_path,
            info,
            hash_tree(&info.install_path).unwrap(),
        )
        .unwrap();
    }
    let params = params(root.path());
    let compiler = Esp32Compiler::new(
        tc.join("gcc"),
        tc.join("g++"),
        crate::esp32::mcu_config::get_mcu_config("esp32c3").unwrap(),
        "160000000L",
        Default::default(),
        vec![],
        BuildProfile::Release,
        false,
    )
    .with_framework_compile_root(true, &fw);
    let sources = [fw.join("core.c")];
    let overlay = LanguageExtraFlags::default();
    let input = Inputs {
        params: &params,
        compiler: &compiler,
        toolchain: &toolchain,
        framework: &framework,
        framework_root: &fw,
        toolchain_root: &tc,
        farm: None,
        eligible: true,
        board: "esp32-c3-devkitm-1",
        mcu: "esp32c3",
        variant: "variants/esp32c3",
        sources: &sources,
        core_dir: &params.build_dir,
        overlay: &overlay,
    };
    let planned = plan(&input).expect("valid manifests pin a shared client/publisher plan");
    assert!(serde_json::to_string(&planned).unwrap().contains("tree:"));
    let mut registry = framework.clone();
    registry.checksum = Some("a".repeat(64));
    assert_eq!(
        package(&registry).unwrap().sha256,
        format!("registry:{}", "a".repeat(64))
    );
    std::fs::remove_file(fw.join(SIDECAR)).unwrap();
    assert!(plan(&input).is_none());
    assert!(package(&registry).unwrap().sha256.starts_with("registry:"));
}
