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
fn manifest_pinned_plan_and_registry_precedence() {
    let root = tempfile::tempdir().unwrap();
    if fbuild_core::platform::fs::executable_bit(&std::fs::metadata(root.path()).unwrap()).is_none()
    {
        return;
    }
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

#[tokio::test]
async fn hydrate_exports_verified_plan_with_prebuilt_client() {
    assert_hydrate_exports_plan(true).await;
}

#[test]
fn plan_export_revision_invalidates_legacy_noop_fingerprint() {
    let original = "legacy build metadata";
    let legacy = sha256(format!("{original}:prebuilt-abi-{PREBUILT_ABI_VERSION}").as_bytes());
    assert_ne!(metadata_hash(original.into(), true), legacy);
    assert_eq!(metadata_hash(original.into(), false), original);
}

#[tokio::test]
async fn hydrate_exports_verified_plan_without_prebuilt_client() {
    assert_hydrate_exports_plan(false).await;
}

async fn assert_hydrate_exports_plan(enabled: bool) {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let fw = root.as_path().join("framework");
    let tc = root.as_path().join("toolchain");
    std::fs::create_dir_all(&fw).unwrap();
    std::fs::create_dir_all(&tc).unwrap();
    std::fs::write(fw.join("core.c"), "int core(void) { return 1; }").unwrap();
    std::fs::write(tc.join("gcc"), "compiler fixture").unwrap();
    let framework = PackageInfo {
        name: "arduino-esp32".into(),
        version: "3.3.12".into(),
        url: "https://example.invalid/core.zip".into(),
        install_path: fw.clone(),
        checksum: fbuild_core::platform::fs::executable_bit(&std::fs::metadata(&fw).unwrap())
            .is_none()
            .then(|| "a".repeat(64)),
        content_id: None,
        installed_bytes: None,
    };
    let toolchain = PackageInfo {
        name: "gcc".into(),
        install_path: tc.clone(),
        ..framework.clone()
    };
    for info in [&framework, &toolchain] {
        if info.checksum.is_none() {
            write_manifest(
                &info.install_path,
                info,
                hash_tree(&info.install_path).unwrap(),
            )
            .unwrap();
        }
    }
    let mut params = params(root.as_path());
    std::fs::create_dir_all(params.project_dir.join("src")).unwrap();
    if enabled {
        params.prebuilt = Some(crate::prebuilt::Client::new(
            std::sync::Arc::new(|_| Box::pin(async { crate::prebuilt::disabled() })),
            std::sync::Arc::new(|_| {}),
        ));
    }
    let farm = root.as_path().join("sdk-includes");
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
        farm: Some(&farm),
        eligible: true,
        board: "esp32-c3-devkitm-1",
        mcu: "esp32c3",
        variant: "variants/esp32c3",
        sources: &sources,
        core_dir: &params.build_dir,
        overlay: &overlay,
    };

    assert!(
        !hydrate(input).await,
        "source fallback must return normally"
    );
    let path = params.build_dir.join("prebuilt-plan.json");
    assert!(path.is_file(), "hydrate must export {}", path.display());
    let exported: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(valid_sha(exported["fp"].as_str().unwrap()));
    assert_eq!(exported["board"], "esp32-c3-devkitm-1");
    for package in ["toolchain", "framework"] {
        let identity = exported["plan"][package]["sha256"].as_str().unwrap();
        assert!(identity.starts_with("tree:") || identity.starts_with("registry:"));
        assert!(valid_sha(identity.split_once(':').unwrap().1));
    }
}
