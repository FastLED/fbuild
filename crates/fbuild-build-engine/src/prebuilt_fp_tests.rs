use crate::prebuilt_fp::{FrameworkBuildPlan, fingerprint};
use serde_json::json;

#[test]
fn fingerprint_is_canonical_and_sensitive_to_every_input() {
    let value = json!({"abi_version":1,"toolchain":{"id":"gcc","version":"1","sha256":"a".repeat(64)},
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
            "abi_version" => json!(2),
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
