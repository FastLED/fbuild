use crate::prebuilt_fp::{FrameworkBuildPlan, fingerprint};
use serde_json::json;

#[test]
fn fingerprint_is_canonical_and_sensitive_to_every_input() {
    let value = json!({"abi_version":1,"fingerprint_schema":1,"toolchain":{"id":"gcc","version":"1","sha256":"a".repeat(64)},
        "framework":{"id":"arduino-esp32","version":"3.1.1","sha256":"b".repeat(64)},
        "mcu":"esp32", "variant_dir":"variants/esp32", "argv":[["-Os","-DA=1","-DB=2","-Iinclude"]],
        "config_sha256":{"sdkconfig.h":"c".repeat(64)}, "path_map_prefix":"/fbuild"});
    let plan: FrameworkBuildPlan = serde_json::from_value(value.clone()).unwrap();
    let fp = fingerprint(&plan);
    assert_eq!(fp, fingerprint(&plan));
    let reverse: serde_json::Map<_, _> = value
        .as_object()
        .unwrap()
        .iter()
        .rev()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    assert_eq!(
        fp,
        fingerprint(&serde_json::from_value(json!(reverse)).unwrap())
    );
    for field in [
        "abi_version",
        "fingerprint_schema",
        "toolchain",
        "framework",
        "mcu",
        "variant_dir",
        "argv",
        "config_sha256",
        "path_map_prefix",
    ] {
        let mut altered = value.clone();
        altered[field] = match field {
            "abi_version" | "fingerprint_schema" => json!(2),
            "toolchain" => json!({"id":"gcc","version":"2","sha256":"a".repeat(64)}),
            "framework" => json!({"id":"arduino-esp32","version":"3.1.2","sha256":"b".repeat(64)}),
            "argv" => json!([["-Os", "-DB=2", "-DA=1", "-Iinclude"]]),
            "config_sha256" => json!({"sdkconfig.h":"d".repeat(64)}),
            _ => json!("changed"),
        };
        assert_ne!(
            fp,
            fingerprint(&serde_json::from_value(altered).unwrap()),
            "{field}"
        );
    }
}

#[test]
fn every_compile_flag_and_package_digest_participates() {
    use crate::prebuilt_fp::PackageIdentity;
    use std::collections::BTreeMap;
    let package = PackageIdentity {
        id: "gcc".into(),
        version: "1".into(),
        sha256: "a".repeat(64),
    };
    let plan = FrameworkBuildPlan {
        abi_version: 1,
        fingerprint_schema: 1,
        toolchain: package.clone(),
        framework: package,
        mcu: "esp32".into(),
        variant_dir: "variants/esp32".into(),
        argv: vec![vec![
            "-Os".into(),
            "-g0".into(),
            "-flto".into(),
            "-Ione".into(),
            "-Itwo".into(),
        ]],
        config_sha256: BTreeMap::from([
            ("a.h".into(), "a".repeat(64)),
            ("b.h".into(), "b".repeat(64)),
        ]),
        path_map_prefix: "/fbuild".into(),
    };
    let original = fingerprint(&plan);
    for index in 0..plan.argv[0].len() {
        let mut other = plan.clone();
        other.argv[0][index].push_str("changed");
        assert_ne!(original, fingerprint(&other));
    }
    let mut other = plan.clone();
    other.argv[0].swap(3, 4);
    assert_ne!(original, fingerprint(&other));
    let mut other = plan.clone();
    other.toolchain.sha256 = "c".repeat(64);
    assert_ne!(original, fingerprint(&other));
    let mut other = plan.clone();
    other.framework.sha256 = "d".repeat(64);
    assert_ne!(original, fingerprint(&other));
    let mut other = plan.clone();
    other.toolchain.id = "other".into();
    assert_ne!(original, fingerprint(&other));
    let mut other = plan.clone();
    other.framework.id = "other".into();
    assert_ne!(original, fingerprint(&other));
}

#[test]
fn builder_and_normal_output_paths_share_only_reproducibly_mapped_inputs() {
    use crate::prebuilt_fp::{PathMapping, canonical_argv};
    let root = tempfile::tempdir().unwrap();
    let mut variants = Vec::new();
    for name in ["builder", "consumer"] {
        let project = root.path().join(name);
        let framework = project.join("framework");
        let build = project.join("output");
        std::fs::create_dir_all(&framework).unwrap();
        std::fs::create_dir_all(&build).unwrap();
        let source = framework.join("core.c");
        std::fs::write(&source, "int f(void) {return 1;}").unwrap();
        let flags = vec![
            "-g".into(),
            format!("-I{}", framework.display()),
            format!(
                "-ffile-prefix-map={}=/fbuild/framework",
                framework.display()
            ),
        ];
        let (raw, _) = crate::compiler::compile_argv_at_root(
            &source,
            &build.join(format!("{name}.o")),
            &flags,
            &[],
            &[],
            Some(&framework),
        );
        let maps = vec![
            PathMapping::new(&framework, "/fbuild/framework"),
            PathMapping::new(&build, "/fbuild/build"),
        ];
        variants.push(canonical_argv(&raw, &framework, &maps, 0).unwrap());
    }
    assert_eq!(variants[0], variants[1]);
}

#[test]
fn forced_header_mutation_is_an_unsupported_plan_instead_of_a_false_hit() {
    use crate::prebuilt_fp::{PathMapping, canonical_argv};
    let temp = tempfile::tempdir().unwrap();
    let header = temp.path().join("outside.h");
    let mappings = [PathMapping::new(temp.path(), "/fbuild/project")];
    let args = vec![
        "cc".into(),
        "-include".into(),
        header.to_string_lossy().into_owned(),
        "-c".into(),
        "core.c".into(),
        "-o".into(),
        "core.o".into(),
    ];
    for value in ["#define VALUE 1\n", "#define VALUE 2\n"] {
        std::fs::write(&header, value).unwrap();
        assert!(canonical_argv(&args, temp.path(), &mappings, 0).is_none());
    }
}

#[test]
fn mapping_precedence_matches_gcc_last_matching_prefix_not_longest() {
    use crate::prebuilt_paths::{PathMapping, mapped_path};
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let cache = project.join("nested-cache");
    let mappings = [
        PathMapping::new(&cache, "/fbuild/cache"),
        PathMapping::new(&project, "/fbuild/project"),
    ];
    assert_eq!(
        mapped_path(&cache.join("header.h"), temp.path(), &mappings),
        "/fbuild/project/nested-cache/header.h"
    );
}

#[tokio::test]
async fn symlinked_roots_are_ineligible_for_v1() {
    use crate::prebuilt_paths::{PathMapping, unambiguous_roots};
    if fbuild_core::platform::host::is_windows() {
        return;
    } // Directory links require external privileges on Windows.
    let temp = tempfile::tempdir().unwrap();
    let actual = temp.path().join("actual");
    let alias = temp.path().join("alias");
    std::fs::create_dir_all(&actual).unwrap();
    fbuild_core::platform::fs::symlink_dir(&actual, &alias).unwrap();
    assert!(unambiguous_roots(&[PathMapping::new(&actual, "/fbuild/framework")]).await);
    assert!(!unambiguous_roots(&[PathMapping::new(&alias, "/fbuild/framework")]).await);
}
